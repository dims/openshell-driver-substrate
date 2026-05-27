# gpu-counter — NVIDIA GPU passthrough on Agent Substrate

A 6-beat demo that runs a Python agent holding a live CUDA context inside an
OpenShell sandbox, on top of an [agent-substrate](https://github.com/agent-substrate/substrate)
actor (gVisor + nvproxy + cuda-checkpoint). Sibling of the [`helpdesk`](../helpdesk/)
demo; same provisioning path, but the workload owns 1 MiB of on-device GPU
memory and the demo's headline property is that those bytes survive
`substrate suspend → idle → resume`.

```
operator  ──gRPC──>  openshell-gateway  ──in-process──>  openshell-driver-substrate  ──gRPC──>  ate-api-server  ──>  atelet  ──>  ateom-gvisor  ──>  runsc --nvproxy
```

**Status:** Substrate-side wiring landed via
[`agent-substrate/substrate#96`](https://github.com/agent-substrate/substrate/pull/96).
Driver-side counterpart on this repo's `main` at commit `0b46450`. Verified
end-to-end on an NVIDIA L40S (driver 580.126.09) 2026-05-27: substrate's
golden-actor `RunWorkload` + `CheckpointWorkload` RPCs both succeed with
`--nvproxy` on every runsc invocation; gVisor's nvproxy auto-registers
cuda-checkpoint internally on `release-20260520.0`. Two follow-up items are
documented under "Open follow-ups" below.

## The 6 beats

Organized as three acts.

| # | Beat | What it proves | RPC path |
|---|---|---|---|
| **I — Provisioning** | | | |
| 1 | Provision `gpu1` via `OpenShell.CreateSandbox` | The gateway carries `SandboxTemplate.annotations["substrate_actor_template"]` (M3.16) through to the driver, which references the pre-applied `gpu-counter` ActorTemplate. atelet builds an OCI bundle with `/dev/nvidia*` devices + bind-mounts cuda-checkpoint; ateom-gvisor invokes `runsc create --nvproxy --nvproxy-driver-version=<host> --nvproxy-allowed-driver-capabilities=compute,utility`. | Gateway → driver.create_sandbox → ateapi.CreateActor + ResumeActor |
| 2 | `GET /info` + `GET /sum` | Boot-time sanity: agent reports the CUDA `dev_ptr` it allocated, the host driver version (read via `cuDriverGetVersion`), and the boot-sentinel byte (`0x42 == 66`) read back through a 4 KiB `cuMemcpyDtoH_v2` probe. Proves nvproxy is live and libcuda inside the sandbox can ioctl through to the host driver. | curl → atenet → sandbox → /dev/nvidia* → host driver |
| **II — Mutate, suspend, resume** | | | |
| 3 | `POST /set?val=99` + `GET /sum` | `cuMemsetD8_v2` rewrites every byte of the 1 MiB device buffer to `99 == 0x63`. `/sum` confirms `sample == 99`. The buffer is genuinely on-device — no host shadow. | curl → atenet → cuMemsetD8_v2 |
| 4 | `kubectl ate suspend actor gpu1` | Substrate's atelet calls ateom-gvisor's CheckpointWorkload; ateom-gvisor invokes `runsc checkpoint pause`. gVisor's nvproxy auto-runs cuda-checkpoint to drain GPU FDs before serialising sentry state. Actor transitions `STATUS_RUNNING → STATUS_SUSPENDING → STATUS_SUSPENDED`. | substrate ateapi.SuspendActor → atelet.Checkpoint → ateom.CheckpointWorkload → runsc checkpoint |
| 5 | `GET /sum` after resume | Implicit resume on traffic. atenet routes the request, substrate restores the sandbox via `runsc restore`, nvproxy re-toggles the CUDA state, the agent serves the request. **`sample` MUST still be 99** — on-device GPU memory survived the round-trip. | curl → substrate restore → cuMemcpyDtoH_v2 |
| **III — Hygiene** | | | |
| 6 | `OpenShell.DeleteSandbox gpu1` | Driver reaps the actor; the pre-provisioned `gpu-counter` ActorTemplate survives. | Gateway → driver.delete_sandbox → ateapi.DeleteActor |

## Prerequisites

| Tool / resource | Version / details |
|---|---|
| Linux host with an NVIDIA GPU on `runsc nvproxy list-supported-drivers` (R570+ for cuda-checkpoint NVML support) | Verified on L40S, driver 580.126.09. The `release-20260520.0` runsc supports 16 driver versions across the 535/550/570/580/590 families. |
| `docker` | 28+ (Brev box ships 29.4.3) |
| `kind` | `v0.31.0+` — `go install sigs.k8s.io/kind@v0.31.0` |
| `kubectl` | matches kind |
| `go` | 1.26+ (substrate's `go.mod` pins 1.26.1) |
| `ko` | Pinned tool under substrate's `hack/run-tool.sh`; no system install needed |
| `kubectl-ate` (from agent-substrate/substrate) | `go install ./cmd/kubectl-ate` from the substrate repo |
| `kubectl-osh` (this repo) | `(cd cmd/kubectl-osh && make install)` |
| `jq`, `curl` | distro packages |
| `runsc` `release-20260520.0` | `wget https://storage.googleapis.com/gvisor/releases/release/20260520.0/x86_64/runsc` |
| `cuda-checkpoint` matching the host driver | `wget https://github.com/NVIDIA/cuda-checkpoint/raw/main/bin/x86_64_Linux/cuda-checkpoint` |
| The wrapper substrate ships | `hack/cuda-checkpoint-wrapper.sh` from agent-substrate/substrate |

### Companion changes upstream

| Repo + branch / PR | Required for | Status |
|---|---|---|
| [`agent-substrate/substrate#96`](https://github.com/agent-substrate/substrate/pull/96) | The substrate-side wiring: CRD field, ateletpb/ateompb threading, atelet OCI builder, ateom-gvisor `--nvproxy` flags | open PR; this demo can't run without it merged or built from `dims/substrate:feat/gpu-passthrough` |
| Driver-side counterpart in this repo | `validate_substrate_sandbox` GPU reject dropped, `get_capabilities.supports_gpu=true`, `synthesize_template` populates `containers[0].resources.gpu` | merged: `0b46450` on `dims/openshell-driver-substrate:main` |
| Helpdesk-shared prereqs (PR #66 / #67 / #75 + OpenShell M3.14–M3.16) | Same as the helpdesk demo — see [`../helpdesk/README.md`](../helpdesk/README.md) for the full list | mix of merged + open |

## One-time host pre-flight (per kind host with a GPU)

These are the substrate-level operational gaps the M4 follow-up will close
(see "Open follow-ups"). Until then, every host that wants to run this demo
needs them. The substrate kind-local-dev runbook
[`~/notes/agent-substrate/2026-05-21-agent-substrate-kind-local-dev.md`](https://github.com/dims/openshell-driver-substrate/blob/main/examples/gpu-counter/README.md) §11 has the same script.

```sh
# 1. Replace /run/nvidia-persistenced/socket with a regular file. gVisor's
#    gofer can't bind-mount Unix sockets, and nvidia-container-cli (which
#    runsc-with-nvproxy invokes) hard-codes a bind of this socket regardless
#    of persistenced state.
sudo systemctl stop nvidia-persistenced
sudo systemctl disable nvidia-persistenced
sudo rm -rf /run/nvidia-persistenced
sudo mkdir -p /run/nvidia-persistenced && sudo touch /run/nvidia-persistenced/socket

# 2. cuda-checkpoint (binary version is host-driver-tied; just download
#    NVIDIA's latest — it advertises its version via --version).
sudo install -m 0755 \
  <(curl -fsSL https://github.com/NVIDIA/cuda-checkpoint/raw/main/bin/x86_64_Linux/cuda-checkpoint) \
  /usr/local/bin/cuda-checkpoint

# 3. Wrapper substrate ships in agent-substrate/substrate:hack/.
sudo install -m 0755 \
  <path-to-substrate>/hack/cuda-checkpoint-wrapper.sh \
  /usr/local/bin/cuda-checkpoint-wrapper.sh

# 4. Confirm the host driver is in runsc's supported list.
runsc nvproxy list-supported-drivers | \
  grep "$(nvidia-smi --query-gpu=driver_version --format=csv,noheader)"

# 5. After `hack/create-kind-cluster.sh`, copy cuda-checkpoint + wrapper
#    inside the kind node where atelet runs:
docker cp /usr/local/bin/cuda-checkpoint kind-control-plane:/usr/local/bin/
docker cp /usr/local/bin/cuda-checkpoint-wrapper.sh kind-control-plane:/usr/local/bin/
docker exec kind-control-plane chmod 755 \
  /usr/local/bin/cuda-checkpoint /usr/local/bin/cuda-checkpoint-wrapper.sh
```

## Quick start

```bash
# 1. Build the gpu-counter actor image with libcuda baked in. The Ubuntu
#    noble packages match the host driver line; the gpu-counter agent
#    loads /usr/lib/x86_64-linux-gnu/libcuda.so.1 at startup.
cd examples/gpu-counter
docker build -t localhost:5001/gpu-counter:demo .
docker push localhost:5001/gpu-counter:demo
GPU_COUNTER_IMAGE=$(docker inspect localhost:5001/gpu-counter:demo \
                     --format '{{index .RepoDigests 0}}')

# 2. Apply the substrate manifests via ko (resolves the ateom-gvisor
#    ko:// reference into a published image).
sed -e "s|__ATEOM_IMAGE__|ko://github.com/agent-substrate/substrate/cmd/ateom-gvisor|" \
    -e "s|__GPU_COUNTER_IMAGE__|$GPU_COUNTER_IMAGE|" \
    gpu-counter-template.yaml \
  | (cd ~/go/src/github.com/agent-substrate/substrate && bash hack/run-tool.sh ko apply -f -)

# 3. Wait for ActorTemplate to take its golden snapshot.
kubectl wait --for=jsonpath='{.status.phase}'=Ready \
  -n ate-demo-gpu-counter actortemplate/gpu-counter --timeout=120s

# 4. Drive the demo.
./gpu-counter-run.sh
```

## What's in this folder

| file | purpose |
|---|---|
| `gpu-counter-agent.py` | Python HTTP server. Holds a 1 MiB on-device CUDA buffer via `libcuda` ctypes. `GET /info` reports `dev_ptr` + driver version; `GET /sum` reads a 4 KiB probe via `cuMemcpyDtoH_v2`; `POST /set?val=N` calls `cuMemsetD8_v2`. |
| `gpu-counter-data.yaml` | OPA + Landlock policy. Same shape as helpdesk's `data.yaml`; allow-list extended with `/dev/nvidia*`. |
| `gpu-counter.Dockerfile` | Layered on the openshell-sandbox `${BASE}` image (same `BASE` arg the helpdesk demo uses). Adds python3 + `libnvidia-compute-580` + `nvidia-utils-580` from Ubuntu noble. Substrate's atelet doesn't run `nvidia-container-cli configure` today, so libcuda has to come from the image. |
| `gpu-counter-template.yaml` | Substrate `ActorTemplate` with the new `containers[*].resources.gpu.{count,device,driverCapabilities,driverVersion}` block. WorkerPool replicas=1 (bump for capacity). |
| `gpu-counter-run.sh` | 6-beat demo driver. Provisions via `kubectl osh create sandbox`, hits the data plane via atenet port-forward, suspends via `kubectl ate suspend actor`, resumes implicitly, asserts the sample byte. |
| `validate-bare.sh` | Pre-substrate validation: drives `docker --runtime=runsc-gpu --gpus all` directly to stress-test the cuda-checkpoint wrapper on any host. The way the substrate-shipped wrapper was first proven on the L40S brev box before the kind-cluster integration. |

## Verified output (excerpt from 2026-05-27 brev L40S run)

```
$ runsc nvproxy list-supported-drivers | grep 580.126.09
580.126.09

$ docker run --rm --runtime=runsc-gpu --gpus all \
    nvidia/cuda:12.6.3-base-ubuntu24.04 nvidia-smi -L
GPU 0: NVIDIA L40S (UUID: GPU-6dd6c711-aa57-bfeb-9b40-9bfaf62b0d88)

$ ./validate-bare.sh
[runsc checkpoint with --save-restore-exec-argv=/usr/local/bin/cuda-checkpoint-wrapper.sh ...]
total 139024
-rw-r--r-- 1 root root   1424438 checkpoint.img        # 1.4 MB sentry state
-rw-r--r-- 1 root root 140865536 pages.img             # 140 MB memory + GPU host shadow
-rw-r--r-- 1 root root     19250 pages_meta.img
PASS: runsc checkpoint succeeded with wrapper-driven cuda-checkpoint --toggle.
```

Substrate-side RPC log excerpts (atelet, during the golden actor's Run+Checkpoint):

```
"method":"/atelet.AteomHerder/Run", "spec":{"containers":[{"name":"supervisor","gpu":{"count":1},...}]},
   "resp":{}, "err":null, "elapsed-time":"2.915787301s"

"method":"/atelet.AteomHerder/Checkpoint", ...
   "resp":{}, "err":null, "elapsed-time":"430.879255ms"
```

The runsc command lines emitted by ateom-gvisor carry `--nvproxy` (the
substrate-side wiring works):

```
runsc ... --root=... --nvproxy create --bundle=... pause
runsc ... --root=... --nvproxy create --bundle=... supervisor
runsc ... --root=... --nvproxy checkpoint --image-path=... pause
```

## Troubleshooting

| symptom | fix |
|---|---|
| `FATAL ERROR: error setting up FS: mounting /run/nvidia-persistenced/socket: open(...): no such device or address` | gVisor's gofer can't bind-mount Unix sockets; replace the socket with a regular file. See pre-flight step 1. |
| `FATAL ERROR: checkpoint failed: ... save/restore binary is already set` | gVisor's nvproxy on `release-20260520.0` auto-registers cuda-checkpoint when `--nvproxy` is set on `create`. Don't pass `--save-restore-exec-argv` at checkpoint time. Substrate's wiring already accounts for this (`gpuSaveRestoreFlags()` returns nil); this error means you're running a stale substrate binary. |
| `FATAL ERROR: starting sub-container [...]: inconsistent private memory files on restore: savedMFOwners = [pause:/]` | gVisor multi-container restore limitation — see "Open follow-ups" item 2. |
| `python3 -c "import ctypes; ctypes.CDLL('libcuda.so.1')"` raises `OSError` inside the sandbox | The workload image isn't carrying `libnvidia-compute-580` (or matching). Check `docker run --rm <image> ls /usr/lib/x86_64-linux-gnu/libcuda.so*`. Build with the full `python3` package; `python3-minimal` doesn't include `_ctypes`. |
| `kubectl ate get actor` shows STATUS_RESUMING that won't progress | The worker pod the actor was bound to is gone. Either wait for substrate's syncer to recover (PR #75 release-on-pod-delete) or scale the WorkerPool replicas up. Stuck STATUS_RESUMING actors block `delete actor`. |
| Image build pulls `nvidia-utils-580` for several minutes | Ubuntu noble's NVIDIA package is ~270 MB. Cache `docker pull nvidia/cuda:12.6.3-runtime-ubuntu24.04` once; the layered installs are then deterministic. |

## Cleanup

```bash
# Tear down the demo actor + namespace.
kubectl osh delete sandbox gpu1 --ignore-not-found
kubectl delete -n ate-demo-gpu-counter actortemplate gpu-counter workerpool gpu-counter-pool
kubectl delete namespace ate-demo-gpu-counter

# Only if you don't want to iterate on the cluster:
~/go/src/github.com/agent-substrate/substrate/hack/delete-kind-cluster.sh
```

The host-level changes from "Pre-flight" can stay in place — they're either
no-ops (the persistenced fixup) or read-only file installs.

## Open follow-ups (not in PR #96)

1. **atelet should run `nvidia-container-cli configure` against the rootfs
   when the container has `resources.gpu`.** Without it, the workload image
   must bake matching driver libs (this demo bakes `libnvidia-compute-580`
   + `nvidia-utils-580` from Ubuntu noble). Production-grade integration
   would call `nvidia-container-cli` from atelet so any unmodified CUDA
   workload image works without driver libs baked in. Tracked in
   `~/notes/openshell-on-substrate/2026-05-27-gpu-passthrough-impl-log.md`.

2. **gVisor multi-container restore of nvproxy state drops the supervisor
   sub-container.** `runsc checkpoint pause` saves `savedMFOwners=[pause:/]`
   — the supervisor's process memory isn't captured. On restore, gVisor
   fails the sub-container start with `inconsistent private memory files
   on restore`. Empirically verified on L40S 2026-05-27 with libcuda + a
   live CUDA context inside the supervisor. Either substrate switches to
   a per-sub-container checkpoint loop, or this needs an upstream gVisor
   issue. Same notes file has the full error trace + theory.

## Further reading

- [`../../src/lib.rs`](../../src/lib.rs) — driver's `synthesize_template` populating `containers[0].resources.gpu` from `sandbox.spec.gpu` / `sandbox.spec.gpu_device`.
- [`../../src/template.rs`](../../src/template.rs) — Rust mirror of substrate's CRD GPU types (`ContainerResources`, `GpuResource`).
- [`../helpdesk/README.md`](../helpdesk/README.md) — the non-GPU sibling demo; same provisioning path, no nvproxy.
- [`../../docs/poc-intro.md`](../../docs/poc-intro.md) — architecture overview.
- [substrate PR #96](https://github.com/agent-substrate/substrate/pull/96) — substrate-side wiring (CRD + protos + atelet OCI + ateom-gvisor runsc flags + wrapper script).
- [`~/notes/openshell-on-substrate/2026-05-25-gpu-passthrough-analysis.md`](https://github.com/dims/openshell-on-substrate-notes/blob/main/2026-05-25-gpu-passthrough-analysis.md) — pre-impl feasibility analysis with the full gVisor source crawl (driver-version pinning, cuda-checkpoint workflow, R570+ requirement, arm64 unsupported, performance claims).
- [`~/notes/openshell-on-substrate/2026-05-27-gpu-passthrough-impl-log.md`](https://github.com/dims/openshell-on-substrate-notes/blob/main/2026-05-27-gpu-passthrough-impl-log.md) — implementation session log; every sharp edge captured with the corresponding code fix.
- [gVisor user_guide/gpu.md (release-20260520.0)](https://github.com/google/gvisor/blob/release-20260520.0/g3doc/user_guide/gpu.md) — upstream documentation for `runsc --nvproxy`.
- [gVisor user_guide/checkpoint_restore.md (release-20260520.0)](https://github.com/google/gvisor/blob/release-20260520.0/g3doc/user_guide/checkpoint_restore.md) — GPU checkpoint/restore section.
