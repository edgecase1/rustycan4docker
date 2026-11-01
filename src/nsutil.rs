/*
 * Filename: nsutil.rs
 *
 * Small helpers for running iproute2 commands and reaching into a container's
 * network namespace from the plugin.
 *
 * Docker identifies a container's network namespace by a path (its Join
 * request field is called SandboxKey), e.g. /var/run/docker/netns/1a2b3c4d.
 * `ip -n` / `tc -n` only accept names under /run/netns, so NetnsRef creates a
 * temporary symlink there that points at the namespace file.
 */

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NS_COUNTER: AtomicU64 = AtomicU64::new(0);
const NETNS_DIR: &str = "/run/netns";

/// Run a command, returning stdout on success and a readable error otherwise.
pub fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to execute {cmd}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "`{} {}` failed: {}",
            cmd,
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A temporary /run/netns/<name> entry pointing at a container's network namespace, removed on drop.
pub struct NetnsRef {
    name: String,
    path: String,
}

impl NetnsRef {
    pub fn new(namespace: &str) -> Result<Self, String> {
        if namespace.is_empty() || !Path::new(namespace).exists() {
            return Err(format!(
                "network namespace {namespace:?} is not visible to the plugin (is /var/run/docker/netns mounted?)"
            ));
        }
        fs::create_dir_all(NETNS_DIR).map_err(|e| format!("create {NETNS_DIR}: {e}"))?;
        let name = format!(
            "rc4d-{}-{}",
            std::process::id(),
            NS_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = format!("{NETNS_DIR}/{name}");
        let _ = fs::remove_file(&path);
        std::os::unix::fs::symlink(namespace, &path)
            .map_err(|e| format!("symlink {path} -> {namespace}: {e}"))?;
        Ok(NetnsRef { name, path })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run `ip -n <ns> <args>`.
    pub fn ip(&self, args: &[&str]) -> Result<String, String> {
        let mut full = vec!["-n", self.name.as_str()];
        full.extend_from_slice(args);
        run("ip", &full)
    }

    /// Run `tc -n <ns> <args>`.
    pub fn tc(&self, args: &[&str]) -> Result<String, String> {
        let mut full = vec!["-n", self.name.as_str()];
        full.extend_from_slice(args);
        run("tc", &full)
    }

    /// All links in the namespace as (ifname, ifalias).
    pub fn links(&self) -> Result<Vec<(String, String)>, String> {
        let out = self.ip(&["-j", "link", "show"])?;
        let v: serde_json::Value =
            serde_json::from_str(&out).map_err(|e| format!("parse ip -j output: {e}"))?;
        Ok(v.as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| {
                        let name = l["ifname"].as_str()?.to_string();
                        let alias = l["ifalias"].as_str().unwrap_or("").to_string();
                        Some((name, alias))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Drop for NetnsRef {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Flatten Docker option payloads into one string map.
///
/// Endpoint/join driver options may arrive at the top level of `Options` or
/// nested under "com.docker.network.generic", depending on the Docker version
/// and API path, so both are collected (nested values win).
pub fn flatten_options(v: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    let v = match v {
        serde_json::Value::String(s) => serde_json::from_str(s).unwrap_or(serde_json::Value::Null),
        other => other.clone(),
    };
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if val.is_string() {
                out.insert(k.clone(), val.clone());
            }
        }
        if let Some(generic) = obj.get("com.docker.network.generic").and_then(|g| g.as_object()) {
            for (k, val) in generic {
                if val.is_string() {
                    out.insert(k.clone(), val.clone());
                }
            }
        }
    }
    out
}
