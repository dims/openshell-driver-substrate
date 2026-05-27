# gpu-counter

Substrate + openshell GPU passthrough + checkpoint/restore demo. Sibling of
`examples/helpdesk/`: same openshell-sandbox supervisor, same gateway/driver
path, but the workload holds a live CUDA context. Proves end-to-end that the
new `ActorTemplate.containers[*].resources.gpu` field round-trips and that a
1 MiB on-device CUDA buffer survives substrate's suspend/resume cycle.

## Demo

```bash
export SUPERVISOR_IMAGE=<gpu-counter image @sha256:...>
./gpu-counter-run.sh
```

The 6 beats: create sandbox → `/info` + boot `/sum` (sample=0x42) →
`POST /set?val=99` + `/sum` (sample=99) → `kubectl ate suspend actor` →
resume by requesting `/sum` (sample MUST still be 99) → delete.

## How it works

- atelet's OCI builder (`cmd/atelet/oci.go`), when it sees `resources.gpu` on
  a container, adds `/dev/nvidia*` to `Linux.Devices` and bind-mounts
  `/usr/local/bin/cuda-checkpoint` + the wrapper into the bundle.
- ateom-gvisor (`cmd/ateom-gvisor/runsc.go`) adds `--nvproxy`,
  `--nvproxy-driver-version`, `--nvproxy-allowed-driver-capabilities` to
  `runsc create`/`restore`, and `--save-restore-exec-argv=<wrapper>` plus
  `--save-restore-exec-timeout=30s` on `runsc checkpoint`/`restore`.
- The wrapper (`substrate/hack/cuda-checkpoint-wrapper.sh`) walks
  `/proc/*/maps`, finds every CUDA-touching PID (including PID 1, which
  *is* the workload inside the sandbox), and runs `cuda-checkpoint --toggle`
  on each. Idempotent, so the same wrapper handles pre-save and
  post-restore.

## Host pre-flight (one-time per kind node)

```bash
# 1. gVisor's gofer can't bind-mount Unix sockets, and
#    nvidia-container-cli configure hard-codes /run/nvidia-persistenced/socket.
sudo systemctl stop nvidia-persistenced && sudo systemctl disable nvidia-persistenced
sudo rm -rf /run/nvidia-persistenced
sudo mkdir -p /run/nvidia-persistenced && sudo touch /run/nvidia-persistenced/socket

# 2. cuda-checkpoint + the wrapper substrate ships.
sudo install -m 0755 \
  <(curl -fsSL https://github.com/NVIDIA/cuda-checkpoint/raw/main/bin/x86_64_Linux/cuda-checkpoint) \
  /usr/local/bin/cuda-checkpoint
sudo install -m 0755 substrate/hack/cuda-checkpoint-wrapper.sh /usr/local/bin/

# 3. Confirm host driver is in the supported set.
runsc nvproxy list-supported-drivers | grep "$(nvidia-smi --query-gpu=driver_version --format=csv,noheader)"
```

## Bare-metal validation

`validate-bare.sh` drives the same flow directly through `docker
--runtime=runsc-gpu` (no substrate); useful for proving the wrapper on a new
host before bringing up the cluster.

## Constraints

R570+ driver, must be in `runsc nvproxy list-supported-drivers`, must match
across checkpoint and restore. x86_64 only.
