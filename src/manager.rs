/*
 * Filename: manager.rs
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

use crate::endpoint::Endpoint;
use crate::network::{JoinResponse, Network};
use crate::nsutil::flatten_options;
use bollard::network::ListNetworksOptions;
use bollard::Docker;
use parking_lot::{RwLock, Mutex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Error;
use std::fs;
use std::sync::Arc;

// Persisted network configuration
#[derive(Serialize, Deserialize, Clone, Debug)]
struct NetworkConfig {
    device: String,
    peer: String,
    canid: String,
}

const NETWORK_STATE_FILE: &str = "/var/lib/docker/network/files/rustycan4docker-networks.json";

#[derive(Clone)]
pub struct NetworkManager {
    network_list: Arc<RwLock<HashMap<String, Network>>>,
    // Mutex to prevent concurrent network_load operations
    // This prevents race conditions when multiple containers start simultaneously
    load_mutex: Arc<Mutex<()>>,
}

impl NetworkManager {
    pub fn new() -> Self {
        let mgr = NetworkManager {
            network_list: Arc::new(RwLock::new(HashMap::new())),
            load_mutex: Arc::new(Mutex::new(())),
        };
        
        // Try to load persisted networks from file
        mgr.load_networks_from_file();
        
        mgr
    }
    
    /// Load network configurations from persistent storage
    fn load_networks_from_file(&self) {
        // Create directory if it doesn't exist
        if let Some(parent) = std::path::Path::new(NETWORK_STATE_FILE).parent() {
            let _ = fs::create_dir_all(parent);
        }
        
        match fs::read_to_string(NETWORK_STATE_FILE) {
            Ok(contents) => {
                match serde_json::from_str::<HashMap<String, NetworkConfig>>(&contents) {
                    Ok(configs) => {
                        println!(" -> Loaded {} network configurations from file", configs.len());
                        let mut map = self.network_list.write();
                        for (nuid, config) in configs {
                            let nw = Network::new(config.device, config.peer, config.canid);
                            map.insert(nuid, nw);
                        }
                    }
                    Err(e) => {
                        eprintln!(" !! Failed to parse network state file: {}", e);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!(" -> No persisted network state found (first run)");
            }
            Err(e) => {
                eprintln!(" !! Failed to read network state file: {}", e);
            }
        }
    }

    pub async fn network_load(&self) {
        // Check if persisted state file exists
        // If it doesn't exist, skip loading from Docker (fresh start scenario)
        if !std::path::Path::new(NETWORK_STATE_FILE).exists() {
            println!(" -> No persisted network state found, starting fresh (skipping Docker network load)");
            return;
        }

        println!(" -> Persisted state file found, loading networks from Docker...");
        let connection = Docker::connect_with_unix_defaults().unwrap();

        let list_networks_filters: HashMap<&str, Vec<&str>> = HashMap::new();
        let config = ListNetworksOptions {
            filters: list_networks_filters,
        };
        match connection.list_networks(Some(config)).await {
            Ok(networks) => {
                for n in networks {
                    match (n.driver, n.options, n.id) {
                        (Some(driver), Some(options), Some(nid)) => {
                            if driver.eq("rustyvxcan") {
                                let device = if options.contains_key("vxcan.dev") {
                                    options["vxcan.dev"].clone()
                                } else {
                                    String::from("vcan")
                                };
                                let peer = if options.contains_key("vxcan.peer") {
                                    options["vxcan.peer"].clone()
                                } else {
                                    String::from("vcan")
                                };
                                let canid = if options.contains_key("vxcan.id") {
                                    options["vxcan.id"].clone()
                                } else {
                                    String::from("0")
                                };

                                let nw = Network::new(device, peer, canid);
                                self.network_list.write().insert(nid, nw);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => eprintln!(" !! Unable to get docker networks: {}", e),
        }
    }

    pub fn network_create(&self, uid: String, options: String) {
        // Print the options and extract the right values
        // Add the network to the hashmap
        println!(
            " -> Adding network with id '{}' with options '{}'",
            uid, options
        );

        match self.options_parse(options) {
            Ok((d, p, c)) => {
                let nw = Network::new(d.clone(), p.clone(), c.clone());
                self.network_list.write().insert(uid.clone(), nw);
                
                // Persist network configuration to file
                self.persist_network_config(uid, d, p, c);
            }
            Err(_) => {}
        }
    }
    
    /// Persist a single network configuration
    fn persist_network_config(&self, nuid: String, device: String, peer: String, canid: String) {
        // Create directory if it doesn't exist
        if let Some(parent) = std::path::Path::new(NETWORK_STATE_FILE).parent() {
            let _ = fs::create_dir_all(parent);
        }
        
        // Load existing configs
        let mut configs: HashMap<String, NetworkConfig> = match fs::read_to_string(NETWORK_STATE_FILE) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        
        // Add/update this network
        configs.insert(nuid, NetworkConfig { device, peer, canid });
        
        // Save back to file
        match serde_json::to_string_pretty(&configs) {
            Ok(json) => {
                if let Err(e) = fs::write(NETWORK_STATE_FILE, json) {
                    eprintln!(" !! Failed to persist network configuration: {}", e);
                }
            }
            Err(e) => {
                eprintln!(" !! Failed to serialize network configuration: {}", e);
            }
        }
    }

    pub fn network_delete(&self, uid: String) {
        let mut map = self.network_list.write();
        if map.contains_key(&uid) {
            println!(" -> Network exists...removing!");
            map.remove(&uid);
        }
        drop(map);
        
        // Remove from persisted configuration
        let mut configs: HashMap<String, NetworkConfig> = match fs::read_to_string(NETWORK_STATE_FILE) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        
        configs.remove(&uid);
        
        if let Ok(json) = serde_json::to_string_pretty(&configs) {
            let _ = fs::write(NETWORK_STATE_FILE, json);
        }
    }

    pub fn endpoint_create(&self, nuid: String, epuid: String, options: serde_json::Value) {
        // Create the endpoint and keep its driver options (vxcan.bpf.*) for Join
        let mut ep = Endpoint::new(epuid);
        ep.opts = flatten_options(&options);

        // Lock the network list
        let mut map = self.network_list.write();
        match map.get_mut(&nuid) {
            Some(n) => {
                // Add the endpoint to the network
                n.endpoint_add(ep)
            }
            None => (),
        };
    }

    pub fn endpoint_delete(&self, nuid: String, epuid: String) {
        // Lock the network list
        let mut map = self.network_list.write();
        match map.get_mut(&nuid) {
            Some(n) => {
                // Remove the endpoint from the network
                n.endpoint_remove(epuid)
            }
            None => (),
        };
    }

    /// Attach an endpoint to a network with full reboot resilience and race condition protection
    /// 
    /// This method implements a multi-stage synchronization strategy:
    /// 1. Network loading is protected by load_mutex to prevent concurrent loads
    /// 2. Endpoint creation uses double-checked locking pattern
    /// 3. Locks are acquired in consistent order to prevent deadlocks
    /// 4. Write locks are only held when necessary to minimize contention
    /// 
    /// Thread-safe for concurrent calls from multiple containers starting simultaneously
    pub fn endpoint_attach(
        &self,
        nuid: String,
        epuid: String,
        sbox: String,
        options: String,
    ) -> Result<JoinResponse, Error> {
        // REBOOT RESILIENCE: Check if network exists in memory
        // If network_load() failed during startup (Docker socket not ready),
        // the network won't be in memory. We need to load it on-demand.
        let network_exists = {
            let map = self.network_list.read();
            map.contains_key(&nuid)
        };

        if !network_exists {
            println!(" -> Network not found in memory (post-reboot recovery), loading from persisted state...");
            
            // CRITICAL SECTION: Use mutex to prevent concurrent network loads
            let _load_guard = self.load_mutex.lock();
            
            // Double-check: another thread may have loaded the network while we waited
            {
                let map = self.network_list.read();
                if map.contains_key(&nuid) {
                    println!(" -> Network was loaded by another thread, continuing");
                    drop(map);
                    drop(_load_guard);
                } else {
                    drop(map);
                    
                    // Load from persisted configuration file
                    match fs::read_to_string(NETWORK_STATE_FILE) {
                        Ok(contents) => {
                            match serde_json::from_str::<HashMap<String, NetworkConfig>>(&contents) {
                                Ok(configs) => {
                                    if let Some(config) = configs.get(&nuid) {
                                        println!(" -> Found network in persisted state: device={}, peer={}, id={}", 
                                            config.device, config.peer, config.canid);
                                        
                                        // Create the network object
                                        let nw = Network::new(
                                            config.device.clone(),
                                            config.peer.clone(),
                                            config.canid.clone()
                                        );
                                        
                                        let mut map = self.network_list.write();
                                        map.insert(nuid.clone(), nw);
                                        drop(map);
                                        
                                        println!(" -> Successfully recovered network from persisted state");
                                    } else {
                                        drop(_load_guard);
                                        eprintln!(" !! Network not found in persisted state - network may not exist");
                                        return Err(Error);
                                    }
                                }
                                Err(e) => {
                                    drop(_load_guard);
                                    eprintln!(" !! Failed to parse network state file: {}", e);
                                    return Err(Error);
                                }
                            }
                        }
                        Err(e) => {
                            drop(_load_guard);
                            eprintln!(" !! Failed to read network state file: {}", e);
                            return Err(Error);
                        }
                    }
                    
                    drop(_load_guard);
                }
            }
        }

        // Lock the network list for reading first (lower contention)
        let map = self.network_list.read();
        let network_ref = match map.get(&nuid) {
            Some(n) => n,
            None => {
                drop(map);
                eprintln!(" !! Network not found during endpoint attach (should not happen)");
                return Err(Error);
            }
        };

        // REBOOT RESILIENCE: Check if endpoint exists in memory
        // After reboot, Docker's metadata persists but our in-memory endpoint list doesn't.
        // If the endpoint is missing, recreate it transparently.
        let endpoint_exists = {
            let ep_map = network_ref.endpoint_list.read();
            ep_map.contains_key(&epuid)
        };

        // Release read lock before potentially acquiring write lock
        drop(map);

        // If endpoint doesn't exist, we need to create it
        // Upgrade to write lock only if necessary
        if !endpoint_exists {
            println!(" -> Endpoint not found in memory (likely post-reboot), recreating...");
            
            // Acquire write lock on network list to add endpoint
            let mut map_write = self.network_list.write();
            let n = match map_write.get_mut(&nuid) {
                Some(network) => network,
                None => {
                    drop(map_write);
                    eprintln!(" !! Network disappeared during endpoint creation");
                    return Err(Error);
                }
            };
            
            // Double-check: another thread may have created the endpoint while we waited for write lock
            let still_missing = {
                let ep_map = n.endpoint_list.read();
                !ep_map.contains_key(&epuid)
            };
            
            if still_missing {
                // Recreate the endpoint
                let ep = Endpoint::new(epuid.clone());
                n.endpoint_add(ep);
                println!(" -> Successfully recreated endpoint after reboot");
            } else {
                println!(" -> Endpoint was created by another thread, continuing");
            }
            
            // Release write lock before continuing
            drop(map_write);
        }

        // Now perform the actual endpoint attach operation
        // Acquire write lock one final time for the attach operation
        let mut map = self.network_list.write();
        match map.get_mut(&nuid) {
            Some(n) => {
                let peer = match serde_json::from_str::<serde_json::Value>(&options) {
                    Ok(v) => match v["vxcan.peer"].as_str() {
                        Some(u) => u.to_string(),
                        None => String::new(),
                    },
                    Err(_) => String::new(),
                };

                let join_opts = flatten_options(&serde_json::Value::String(options.clone()));

                // Add the endpoint to the network (or reattach after reboot)
                let rsp = n.endpoint_attach(epuid, sbox, peer, join_opts)?;
                Ok(rsp)
            }
            None => {
                eprintln!(" !! Network not found during endpoint attach (should not happen)");
                Err(Error)
            }
        }
    }

    pub fn endpoint_detach(&self, nuid: String, epuid: String) {
        // Lock the network list
        let mut map = self.network_list.write();
        match map.get_mut(&nuid) {
            Some(n) => {
                // Detach the endpoint from the network
                n.endpoint_detach(epuid)
            }
            None => (),
        };
    }

    fn options_parse(&self, options: String) -> Result<(String, String, String), Error> {
        match serde_json::from_str::<serde_json::Value>(&options) {
            Ok(v) => {
                let device = match v["vxcan.dev"].as_str() {
                    Some(u) => u.to_string(),
                    None => {
                        println!(" !! Error parsing vxcan.dev option: {}", v["vxcan.dev"]);
                        String::from("vcan")
                    }
                };
                let peer = match v["vxcan.peer"].as_str() {
                    Some(u) => u.to_string(),
                    None => {
                        println!(" !! Error parsing vxcan.peer option: {}", v["vxcan.peer"]);
                        String::from("vcanp")
                    }
                };
                let canid = match v["vxcan.id"].as_str() {
                    Some(u) => u.to_string(),
                    None => {
                        println!(" !! Error parsing vxcan.dev option: {}", v["vxcan.dev"]);
                        String::from("0")
                    }
                };

                // Return the tuple of options
                Ok((device, peer, canid))
            }
            Err(_) => Err(Error),
        }
    }
}
