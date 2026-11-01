/*
 * Filename: bpf.rs
 *
 * Attach an eBPF program from an object file to a container's CAN interface.
 *
 * Per-endpoint driver options (compose: services.<svc>.networks.<net>.driver_opts):
 *   vxcan.bpf.obj        object file, relative to BPF_DIR (required to enable)
 *   vxcan.bpf.sec        ELF section / program to load      (default: tc's "classifier")
 *   vxcan.bpf.direction  ingress | egress                    (default: ingress)
 *
 * Docker only moves the interface into the container after the Join reply, and
 * a netns move flushes qdiscs, so the program cannot be attached in Join itself.
 * Instead the interface is tagged with an alias in Join, and a background
 * thread waits for it to show up (renamed) inside the container's namespace,
 * then attaches it with `tc ... clsact` + `bpf direct-action`.
 */

use crate::nsutil::NetnsRef;
use serde_json::{Map, Value};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// Host directory with BPF objects; mounted into the plugin at the same path.
pub const BPF_DIR: &str = "/var/lib/rustycan4docker/bpf";
const ATTACH_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug)]
pub struct BpfSpec {
    pub obj: PathBuf,
    pub sec: Option<String>,
    pub direction: String,
}

impl BpfSpec {
    /// Ok(None) if no BPF option is set; Err for invalid settings.
    pub fn from_options(opts: &Map<String, Value>) -> Result<Option<Self>, String> {
        let get = |k: &str| opts.get(k).and_then(|v| v.as_str()).map(str::to_string);

        let obj = match get("vxcan.bpf.obj") {
            Some(o) if !o.is_empty() => o,
            _ => return Ok(None),
        };
        let rel = Path::new(&obj);
        if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(format!(
                "vxcan.bpf.obj must be a plain path relative to {BPF_DIR}, got {obj:?}"
            ));
        }
        let path = Path::new(BPF_DIR).join(rel);
        if !path.is_file() {
            return Err(format!("BPF object {} not found", path.display()));
        }

        let direction = get("vxcan.bpf.direction").unwrap_or_else(|| "ingress".into());
        if direction != "ingress" && direction != "egress" {
            return Err(format!(
                "vxcan.bpf.direction must be ingress or egress, got {direction:?}"
            ));
        }

        Ok(Some(BpfSpec {
            obj: path,
            sec: get("vxcan.bpf.sec").filter(|s| !s.is_empty()),
            direction,
        }))
    }
}

/// Alias used to find the interface again after Docker moves and renames it.
pub fn alias_for(epuid: &str) -> String {
    format!("rustycan4docker:{}", &epuid[..epuid.len().min(12)])
}

/// Wait for the tagged interface inside `namespace`, then attach the program.
pub fn spawn_attach(namespace: String, src_name: String, alias: String, spec: BpfSpec) {
    std::thread::spawn(move || match attach(&namespace, &src_name, &alias, &spec) {
        Ok(ifname) => println!(
            " -> BPF {} ({}) attached to {} {} in {}",
            spec.obj.display(),
            spec.sec.as_deref().unwrap_or("classifier"),
            ifname,
            spec.direction,
            namespace
        ),
        Err(e) => eprintln!(" !! BPF attach for {alias} failed: {e}"),
    });
}

fn attach(namespace: &str, src_name: &str, alias: &str, spec: &BpfSpec) -> Result<String, String> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    let mut last_err = String::from("interface never appeared in the container");

    loop {
        if Instant::now() > deadline {
            return Err(last_err);
        }
        std::thread::sleep(POLL_INTERVAL);

        let ns = match NetnsRef::new(namespace) {
            Ok(ns) => ns,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        let links = match ns.links() {
            Ok(l) => l,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        // Wait until Docker has renamed it to its container-side name.
        let ifname = match links.iter().find(|(name, a)| a == alias && name != src_name) {
            Some((name, _)) => name.clone(),
            None => continue,
        };

        ns.tc(&["qdisc", "replace", "dev", &ifname, "clsact"])?;

        let obj = spec.obj.to_string_lossy();
        let mut args = vec![
            "filter", "replace", "dev", &ifname, &spec.direction, "pref", "1", "handle", "1",
            "bpf", "direct-action", "obj", &obj,
        ];
        if let Some(sec) = &spec.sec {
            args.push("sec");
            args.push(sec);
        }
        ns.tc(&args)?;
        return Ok(ifname);
    }
}
