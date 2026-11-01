/*
 * Filename: endpoint.rs
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

use truncrate::*;

#[derive(Clone)]
pub struct Endpoint {
    pub uid: String,
    pub device: String,
    pub peer: String,
    created: bool,
    /// Driver options given for this endpoint (CreateEndpoint + Join), e.g. vxcan.bpf.*
    pub opts: serde_json::Map<String, serde_json::Value>,
}

impl Endpoint {
    /// Check if the endpoint's vxcan interface exists in the kernel
    pub fn interface_exists(&self) -> bool {
        match interfaces::Interface::get_all() {
            Ok(ifcs) => {
                for i in ifcs.into_iter() {
                    if i.name == self.device {
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

    /// Recreate the vxcan interface pair if it's missing
    /// Returns true if interfaces were recreated, false if they already existed
    pub fn ensure_interface_exists(&mut self) -> Result<bool, String> {
        if self.interface_exists() {
            println!(" -> Interface {} already exists, no recreation needed", self.device);
            return Ok(false);
        }

        println!(" -> Interface {} missing after reboot, recreating...", self.device);
        
        // Try to create the vxcan pair
        let output = std::process::Command::new("ip")
            .arg("link")
            .arg("add")
            .arg("dev")
            .arg(&self.device)
            .arg("type")
            .arg("vxcan")
            .arg("peer")
            .arg("name")
            .arg(&self.peer)
            .output();

        match output {
            Ok(result) => {
                if !result.status.success() {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    // Check if error is "File exists" - that means interface was created by another thread
                    if stderr.contains("File exists") {
                        println!(" -> Interface {} was created concurrently, continuing", self.device);
                        return Ok(false);
                    }
                    return Err(format!(" !! Failed to recreate vxcan device {}: {}", self.device, stderr));
                }
            }
            Err(e) => return Err(format!(" !! Failed to execute ip command: {}", e)),
        }

        // Bring up the interface
        let output = std::process::Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("up")
            .arg(&self.device)
            .output();

        match output {
            Ok(result) => {
                if !result.status.success() {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    return Err(format!(" !! Failed to bring up vxcan device {}: {}", self.device, stderr));
                }
            }
            Err(e) => return Err(format!(" !! Failed to execute ip command: {}", e)),
        }

        println!(" -> Successfully recreated interface pair: {} <-> {}", self.device, self.peer);
        
        // Mark as created so we clean it up properly on drop
        self.created = true;
        
        Ok(true)
    }

    pub fn new(uid: String) -> Self {
        println!("Creating a new endpoint");
        let ifcs = interfaces::Interface::get_all().unwrap();

        let mut exists: bool = false;
        let newifc = format!("vxcan{}", uid.truncate_to_byte_offset(8));
        let peerifc = format!("{newifc}p");

        for i in ifcs.into_iter() {
            if i.name.eq(&newifc) {
                exists = true;
            }
        }

        if !exists {
            std::process::Command::new("ip")
                .arg("link")
                .arg("add")
                .arg("dev")
                .arg(&newifc)
                .arg("type")
                .arg("vxcan")
                .arg("peer")
                .arg("name")
                .arg(&peerifc)
                .output()
                .expect("failed to add VXCAN device");
            std::process::Command::new("ip")
                .arg("link")
                .arg("set")
                .arg("up")
                .arg(&newifc)
                .output()
                .expect("failed to start VXCAN device");
        }
        println!(
            "Creating VXCAN tunnel with settings: device='{}', peer='{}'",
            newifc, peerifc
        );
        Endpoint {
            uid: uid,
            device: newifc,
            peer: peerifc,
            created: !exists,
            opts: serde_json::Map::new(),
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if self.created {
            // Actually delete the network interface
            std::process::Command::new("ip")
                .arg("link")
                .arg("set")
                .arg("down")
                .arg(&self.device)
                .output()
                .expect("failed to start VCAN device");
            std::process::Command::new("ip")
                .arg("link")
                .arg("del")
                .arg("dev")
                .arg(&self.device)
                .arg("type")
                .arg("vxcan")
                .output()
                .expect("failed to remove VCAN device");

            println!(
                "Dropping Endpoint object with {}, {}",
                self.device, self.peer,
            );
        }
    }
}
