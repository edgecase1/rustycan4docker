/*
 * Filename: network.rs
 * Created Date: Tuesday, October 18th 2022, 5:15:15 pm
 * Author: Jonathan Haws
 *
 * Copyright (c) 2022 WiTricity
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

use crate::bpf::{self, BpfSpec};
use crate::endpoint::Endpoint;
use crate::nsutil::run;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Error;
use std::sync::Arc;

#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct JoinResponse {
    SrcName: String,
    DstPrefix: String,
}

pub struct Network {
    device: String,
    peer: String,
    canid: String,
    ifc: String,
    created: bool,
    pub endpoint_list: Arc<RwLock<HashMap<String, Endpoint>>>,
    rules_list: Arc<RwLock<Vec<(String, String)>>>,
}

impl Network {
    pub fn new(device: String, peer: String, canid: String) -> Self {
        let ifcs = interfaces::Interface::get_all().unwrap();

        let mut exists: bool = false;
        let newifc = format!("{device}{canid}");

        for i in ifcs.into_iter() {
            if i.name.eq(&newifc) {
                exists = true;
            }
        }

        if !exists {
            println!(" -> Creating interface {newifc}...");
            std::process::Command::new("ip")
                .arg("link")
                .arg("add")
                .arg("dev")
                .arg(&newifc)
                .arg("type")
                .arg("vcan")
                .output()
                .expect(" !! Failed to add VCAN device");
            std::process::Command::new("ip")
                .arg("link")
                .arg("set")
                .arg("up")
                .arg(&newifc)
                .output()
                .expect(" !! Failed to start VCAN device");
        }
        println!(
            " -> Creating network with settings: device='{}', peer='{}', id='{}' -- new device? {}",
            device, peer, canid, !exists
        );
        Network {
            device: device,
            peer: peer,
            canid: canid,
            ifc: newifc,
            created: !exists,
            endpoint_list: Arc::new(RwLock::new(HashMap::new())),
            rules_list: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Check if the network's VCAN interface exists in the kernel
    fn network_interface_exists(&self) -> bool {
        match interfaces::Interface::get_all() {
            Ok(ifcs) => {
                for i in ifcs.into_iter() {
                    if i.name == self.ifc {
                        return true;
                    }
                }
                false
            }
            Err(_) => {
                eprintln!(" !! Failed to query network interfaces");
                false
            }
        }
    }

    /// Recreate the network's VCAN interface if it's missing
    /// This is called during post-reboot recovery
    fn ensure_network_interface_exists(&mut self) -> Result<(), String> {
        if self.network_interface_exists() {
            return Ok(());
        }

        println!(" -> Network interface {} missing after reboot, recreating...", self.ifc);
        
        // Create the VCAN interface
        let output = std::process::Command::new("ip")
            .arg("link")
            .arg("add")
            .arg("dev")
            .arg(&self.ifc)
            .arg("type")
            .arg("vcan")
            .output();

        match output {
            Ok(result) => {
                if !result.status.success() {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    if stderr.contains("File exists") {
                        println!(" -> Interface {} was created concurrently, continuing", self.ifc);
                        return Ok(());
                    }
                    return Err(format!(" !! Failed to recreate VCAN device {}: {}", self.ifc, stderr));
                }
            }
            Err(e) => return Err(format!(" !! Failed to execute ip command: {}", e)),
        }

        // Bring up the interface
        let output = std::process::Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("up")
            .arg(&self.ifc)
            .output();

        match output {
            Ok(result) => {
                if !result.status.success() {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    return Err(format!(" !! Failed to bring up VCAN device {}: {}", self.ifc, stderr));
                }
            }
            Err(e) => return Err(format!(" !! Failed to execute ip command: {}", e)),
        }

        println!(" -> Successfully recreated network interface: {}", self.ifc);
        self.created = true;
        
        Ok(())
    }

    /// Validate that all network interfaces and rules are properly configured
    /// Returns true if everything is OK, false if issues were found
    pub fn validate_network_health(&self) -> bool {
        let mut healthy = true;

        // Check network interface
        if !self.network_interface_exists() {
            eprintln!(" !! Health check FAILED: Network interface {} does not exist", self.ifc);
            healthy = false;
        } else {
            println!(" -> Health check OK: Network interface {} exists", self.ifc);
        }

        // Check all endpoints
        let map = self.endpoint_list.read();
        for (_uid, ep) in map.iter() {
            if !ep.interface_exists() {
                eprintln!(" !! Health check FAILED: Endpoint interface {} does not exist", ep.device);
                healthy = false;
            } else {
                println!(" -> Health check OK: Endpoint interface {} exists", ep.device);
            }
        }

        healthy
    }

    pub fn endpoint_add(&mut self, ep: Endpoint) {
        // Add the endpoint to the list
        self.endpoint_list.write().insert(ep.uid.clone(), ep);
    }

    pub fn endpoint_remove(&mut self, uid: String) {
        let mut map = self.endpoint_list.write();
        if map.contains_key(&uid) {
            println!(" -> Endpoint exists...removing!");
            map.remove(&uid);
        }
    }

    pub fn endpoint_attach(
        &mut self,
        epuid: String,
        namespace: String,
        peer: String,
        join_opts: serde_json::Map<String, serde_json::Value>,
    ) -> Result<JoinResponse, Error> {
        // REBOOT RESILIENCE: Ensure network interface exists before proceeding
        // After system reboot, Docker metadata persists but kernel interfaces don't.
        // This check recreates missing interfaces transparently during container restart.
        if let Err(e) = self.ensure_network_interface_exists() {
            eprintln!(" !! Failed to ensure network interface exists: {}", e);
            return Err(Error);
        }

        // Drop read lock before acquiring write lock to avoid deadlock
        let endpoint_exists = {
            let map = self.endpoint_list.read();
            map.contains_key(&epuid)
        };

        if !endpoint_exists {
            eprintln!(" !! Endpoint not found in network");
            return Err(Error);
        }

        // REBOOT RESILIENCE: Check and recreate endpoint's vxcan interface if missing
        // We need mutable access to the endpoint to call ensure_interface_exists
        {
            let mut map = self.endpoint_list.write();
            if let Some(ep) = map.get_mut(&epuid) {
                match ep.ensure_interface_exists() {
                    Ok(recreated) => {
                        if recreated {
                            println!(" -> Endpoint interfaces were recreated after reboot");
                        }
                    }
                    Err(e) => {
                        eprintln!(" !! Failed to ensure endpoint interface exists: {}", e);
                        return Err(Error);
                    }
                }
            }
        }

        // Now proceed with normal cangw rule creation
        let map = self.endpoint_list.read();
        match map.get(&epuid) {
            Some(ep) => {
                // Add cangw rules: self->endpoint, endpoint->self
                self.add_cangw_rule(&self.ifc, &ep.device);
                self.add_cangw_rule(&ep.device, &self.ifc);

                for (uid, endpt) in map.iter() {
                    if uid.ne(&epuid) {
                        // REBOOT RESILIENCE: Check other endpoints too
                        // In case multiple containers are restarting simultaneously
                        if !endpt.interface_exists() {
                            println!(" -> Warning: A peer endpoint interface is missing, skipping cross-rules for now");
                            continue;
                        }
                        
                        // Add cangw rules: other->endpoint, endpoint->other
                        self.add_cangw_rule(&endpt.device, &ep.device);
                        self.add_cangw_rule(&ep.device, &endpt.device);
                    }
                }

                let mut peerifc = &peer;
                if peer.is_empty() {
                    peerifc = &self.peer;
                }

                // Optional eBPF program for this container's interface. Options
                // from CreateEndpoint, overridden by any given at Join.
                let mut opts = ep.opts.clone();
                opts.extend(join_opts);
                match BpfSpec::from_options(&opts) {
                    Ok(Some(spec)) => {
                        // Tag the interface so it can be found after Docker moves
                        // and renames it, then attach once it is in the container.
                        let alias = bpf::alias_for(&epuid);
                        if let Err(e) = run("ip", &["link", "set", "dev", &ep.peer, "alias", &alias]) {
                            eprintln!(" !! {e}");
                            return Err(Error);
                        }
                        println!(
                            " -> Will attach BPF {} to {} once it is in {}",
                            spec.obj.display(),
                            ep.peer,
                            namespace
                        );
                        bpf::spawn_attach(namespace, ep.peer.clone(), alias, spec);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!(" !! {e}");
                        return Err(Error);
                    }
                }

                let rsp = JoinResponse {
                    SrcName: ep.peer.clone(),
                    DstPrefix: (*peerifc).clone(),
                };
                Ok(rsp)
            }
            None => Err(Error),
        }
    }

    pub fn endpoint_detach(&mut self, epuid: String) {
        let map = self.endpoint_list.read();
        match map.get(&epuid) {
            Some(ep) => {
                for (uid, endpt) in map.iter() {
                    if uid.ne(&epuid) {
                        // Remove cangw rules: other->endpoint, endpoint->other
                        self.remove_cangw_rule(&endpt.device, &ep.device);
                        self.remove_cangw_rule(&ep.device, &endpt.device);
                    }
                }

                // Remove cangw rules: self->endpoint, endpoint->self
                self.remove_cangw_rule(&ep.device, &self.ifc);
                self.remove_cangw_rule(&self.ifc, &ep.device);
            }
            None => (),
        };
    }

    fn add_cangw_rule(&self, src: &String, dst: &String) {
        println!(" -> Adding cangw rule for {src} to {dst}");

        std::process::Command::new("cangw")
            .arg("-A")
            .arg("-s")
            .arg(&src)
            .arg("-d")
            .arg(&dst)
            .arg("-e")
            .output()
            .expect(" !! Failed to add cangw rule");

        std::process::Command::new("cangw")
            .arg("-A")
            .arg("-s")
            .arg(&src)
            .arg("-d")
            .arg(&dst)
            .arg("-eX")
            .output()
            .expect(" !! Failed to add cangw extended rule");

        self.rules_list.write().push((src.clone(), dst.clone()));
    }

    fn remove_cangw_rule(&self, src: &String, dst: &String) {
        let mut rules = self.rules_list.write();
        if rules.contains(&(src.clone(), dst.clone())) {
            println!(" -> Removing cangw rule for {src} to {dst}");

            std::process::Command::new("cangw")
                .arg("-D")
                .arg("-s")
                .arg(&src)
                .arg("-d")
                .arg(&dst)
                .arg("-e")
                .output()
                .expect(" !! Failed to remove cangw rule");

            std::process::Command::new("cangw")
                .arg("-D")
                .arg("-s")
                .arg(&src)
                .arg("-d")
                .arg(&dst)
                .arg("-eX")
                .output()
                .expect(" !! Failed to remove cangw extended rule");

            let index = rules
                .iter()
                .position(|x| *x == (src.clone(), dst.clone()))
                .unwrap(); // safe unwrap since we already verified key exists
            rules.remove(index);
        }
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        if self.created {
            let ifc = format!("{}{}", self.device, self.canid);

            // Actually delete the network interface
            std::process::Command::new("ip")
                .arg("link")
                .arg("set")
                .arg("down")
                .arg(&ifc)
                .output()
                .expect(" !! Failed to stop VCAN device");
            std::process::Command::new("ip")
                .arg("link")
                .arg("del")
                .arg("dev")
                .arg(&ifc)
                .arg("type")
                .arg("vcan")
                .output()
                .expect(" !! Failed to remove VCAN device");

            println!(
                " -> Dropping network object: device={}, peer={}, id={}",
                self.device, self.peer, self.canid
            );
        }
    }
}
