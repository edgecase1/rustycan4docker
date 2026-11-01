This repo is a fork from [ngpbach/rustycan4docker](https://github.com/ngpbach/rustycan4docker) in order to publish multiarch images. This was needed to support arm64 in rustycan4docker.

# VXCAN Network Plugin for Docker

This Docker plugin provides the ability to create VXCAN tunnels for Docker containers. It is based heavily on the work by Christain Gagneraud (https://gitlab.com/chgans/can4docker) and Wiktor S. Ovalle Correa (https://github.com/wsovalle/docker-vxcan).

This plugin has essentially taken the Python implementation and rewrote it in Rust as a way for me to learn Rust and to speed up the plugin startup time, which on an embedded system was way too long when written in Python.

## Requirements

Requires that the vxcan and can-gw modules are built-in or loaded into the kernel.
```
sudo modprobe vxcan
sudo modprobe can-gw
```

## Available Options
**vxcan.id**: Numerical identifier of the interface (i.e., 0 for can0, or 1 for can1). Default is 0.

**vxcan.dev**: Specify the CAN device to use on the host. If the device is present (i.e., a physical CAN device) then it will be used as is; otherwise, a virtual CAN interface is created to use. Default is 'vcan'.

**vxcan.peer**: Prefix for the peer device (i.e., endpoint) to use in the container. This is combined with the vxcan.id to produce an interface name (e.g., vxcanp0). Default is 'vcanp'.

## Usage

### Docker
```
# Create a couple Docker containers to test in separate terminals
docker run --rm -it --name a1 alpine
docker run --rm -it --name a2 alpine

# Create the network
docker network create --driver nomadicdrones/rustycan4docker -o vxcan.dev=vcan -o vxcan.id=0 -o vxcan.peer=vxcanp rust_can1

# Connect the network to the containers
docker network connect rust_can1 a1
docker network connect rust_can1 a2

# Check that the cangw rules are present (twelve total)
cangw -L

# Check that the required interfaces are present (one vcan0, 2 vxcanXXXXXXXX)
ip link

# In the container terminals
apk add can-utils
cangen vxcanp0 # from one container
candump vxcanp0 # from the other container
cangen vcan0 # from the host

# Remove the network (after closing the containers)
docker network rm rust_can1
```

### Compose Application
docker-compose applications can make use of the plugin as well.
```
networks:
  canbus:
    driver: nomadicdrones/rustycan4docker
    driver_opts:
      vxcan.dev: can
      vxcan.peer: can
      vxcan.id: 0
```

### Plugin Installation

#### From Docker Hub (Recommended)
```bash
# Install the latest version (automatically detects your architecture)
docker plugin install nomadicdrones/rustycan4docker:latest

# Or install a specific version
docker plugin install nomadicdrones/rustycan4docker:v0.1.0

# For specific architectures (if needed):
# AMD64: docker plugin install nomadicdrones/rustycan4docker-amd64:latest
# ARM64: docker plugin install nomadicdrones/rustycan4docker-arm64:latest

# Enable the plugin
docker plugin enable nomadicdrones/rustycan4docker
```

#### From Source
```bash
# Clone the repository
git clone https://github.com/Nomadic-Drones/rustycan4docker.git
cd rustycan4docker

# Build and install locally
cd docker-plugin
sudo ./build-plugin.sh
docker plugin enable nomadicdrones/rustycan4docker
```

### Supported Architectures

This plugin supports multiple architectures:
- **AMD64** (x86_64) - Intel/AMD 64-bit processors
- **ARM64** (aarch64) - ARM 64-bit processors (Raspberry Pi 4+, Apple Silicon, etc.)

The plugin automatically detects and installs the correct version for your system architecture.

### Usage After Installation

Once installed, use `nomadicdrones/rustycan4docker` as the driver name:

```bash
# Create the network using the installed plugin
docker network create --driver nomadicdrones/rustycan4docker -o vxcan.dev=vcan -o vxcan.id=0 -o vxcan.peer=vxcanp rust_can1
```

## eBPF programs on container interfaces (tc)

Each container's interface can get an eBPF program from an object file, loaded
with `tc`. Set these as **endpoint** driver options (compose:
`services.<svc>.networks.<net>.driver_opts`, CLI: `docker network connect --driver-opt`):

| Option | Meaning | Default |
|---|---|---|
| `vxcan.bpf.obj` | Object file, relative to `/var/lib/rustycan4docker/bpf` on the host | (none, BPF off) |
| `vxcan.bpf.sec` | ELF section of the program | `classifier` (tc default) |
| `vxcan.bpf.direction` | `ingress` (frames the container receives) or `egress` (frames it sends) | `ingress` |

The vxcan/cangw setup is unchanged. On Join the plugin tags the container-side
vxcan interface with an alias, waits until Docker has moved and renamed it inside
the container, then runs `tc qdisc replace dev <if> clsact` and
`tc filter replace dev <if> <direction> pref 1 handle 1 bpf direct-action obj <file> [sec <sec>]`
in the container's network namespace. Failures are logged by the plugin
(`journalctl -u docker | grep BPF`).

Host prerequisites (both directories must exist before `docker plugin enable`):

```
sudo mkdir -p /var/lib/rustycan4docker/bpf /var/run/docker/netns
```

The plugin additionally gets CAP_SYS_ADMIN, CAP_BPF, CAP_PERFMON and
CAP_SYS_RESOURCE to enter container namespaces and load programs.
