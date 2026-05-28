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
[`agent-substrate/substrate#96`](https://github.com/agent-substrate/substrate/pull/96)
— two commits, `c358dff` (original) + `fca2df4` (five follow-up fixes from the
2026-05-27 demo bring-up). Driver-side counterpart on this repo at
commit `eabfbb7`. **Demo verified end-to-end on three GPU classes
2026-05-27:**

- **L40S** brev (driver 580.126.09) — substrate Run + Checkpoint RPCs
  succeed.
- **H100** brev `front-emerald-krill` (driver 570.195.03) — full 6-beat
  with CUDA buffer preserved across substrate suspend/resume.
- **H200 NVL** `bigbox-h200` (driver 580.159.03) — full 6-beat,
  `dev_ptr=0x7f9b69e00000` preserved across `kubectl ate suspend gpu1`
  + idle + `kubectl ate resume gpu1`.

Five substrate-side fixes (in `fca2df4`) needed for the full 6-beat to
pass:
1. `spec.Linux.Resources.Devices` cgroup-allow entries for each nvidia
   device.
2. GPU spec passed to the pause container too (not just supervisor), so
   the sandbox kernel boots with `--dev-io-fd>=0` (dev gofer wired up).
3. cuda-checkpoint + wrapper bind-mounted from
   `/run/ateom-gvisor/static-files/` (atelet runs inside a distroless
   kind-control-plane container that doesn't have `/usr/local/bin/...`).
4. External CUDA drain via `runsc exec supervisor
   /usr/local/bin/cuda-checkpoint --toggle --pid 1` before
   `runsc checkpoint pause` — gVisor's `--save-restore-exec-argv` runs
   the exec in pause's container, which is distroless and has no
   `/bin/sh`, so a wrapper script fails to load.
5. `gpuSaveRestoreFlags=nil` — gVisor does **not** auto-register
   cuda-checkpoint (the previous comment in this code claiming so was
   wrong; the gVisor source has no auto-registration path).

Operational requirements for the demo:
- **(2026-05-28+)** Workload image **no longer needs** to bake `libcuda.so.<host-driver>`. atelet's `injectNVIDIAAssetsIntoRootfs` (in [substrate's `cmd/atelet/oci.go`](https://github.com/agent-substrate/substrate/blob/main/cmd/atelet/oci.go)) mirrors the host's NVIDIA driver libs into each new actor's rootfs at sandbox-create time. The bring-up script stages those libs once into `/run/ateom-gvisor/static-files/nvidia-libs/` on the kind-node; atelet hard-fails if the staging dir is empty. The demo now uses a plain `ubuntu:24.04 + python3` workload image (see `gpu-counter.Dockerfile`). Pre-2026-05-28 boxes still need the baked variant; see "Historical: pre-2026-05-28 libcuda-baking Dockerfile" in the runbook.
- runsc must be the **gVisor nightly 2026-05-26 or later** —
  `release-20260520.0` has a multi-container dev-gofer bug.
- For host drivers not in `runsc nvproxy list-supported-drivers` (e.g.
  580.159.03), tell substrate it's the nearest supported version with
  the same major (e.g. 580.126.20) — patch-level NVIDIA 580.x ioctls
  are wire-compatible. The setup script (below) derives this
  automatically with the snippet:
  ```bash
  HOST=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader \
          | head -1 | tr -d ' ')
  NVPROXY=$(runsc nvproxy list-supported-drivers \
            | awk -v m="${HOST%%.*}" 'index($0, m".") == 1' \
            | sort -V | tail -1)
  # Use $NVPROXY as both the daemon.json --nvproxy-driver-version and
  # the ActorTemplate's spec.containers[*].resources.gpu.driverVersion
  ```
  For an exact-match host (e.g. 570.195.03) the picker returns the
  same string; for an unlisted patch (e.g. 580.159.03) it picks the
  highest same-major entry (580.126.20).

## The 6 beats

Organized as three acts.

| # | Beat | What it proves | RPC path |
|---|---|---|---|
| **I — Provisioning** | | | |
| 1 | Provision `gpu1` via `OpenShell.CreateSandbox` | The gateway carries `SandboxTemplate.annotations["substrate_actor_template"]` (M3.16) through to the driver, which references the pre-applied `gpu-counter` ActorTemplate. atelet builds an OCI bundle with `/dev/nvidia*` devices + bind-mounts cuda-checkpoint; ateom-gvisor invokes `runsc create --nvproxy --nvproxy-driver-version=<host> --nvproxy-allowed-driver-capabilities=compute,utility`. | Gateway → driver.create_sandbox → ateapi.CreateActor + ResumeActor |
| 2 | `GET /info` + `GET /sum` | Boot-time sanity: agent reports the CUDA `dev_ptr` it allocated, the host driver version (read via `cuDriverGetVersion`), and the boot-sentinel byte (`0x42 == 66`) read back through a 4 KiB `cuMemcpyDtoH_v2` probe. Proves nvproxy is live and libcuda inside the sandbox can ioctl through to the host driver. | curl → atenet → sandbox → /dev/nvidia* → host driver |
| **II — Mutate, suspend, resume** | | | |
| 3 | `POST /set?val=99` + `GET /sum` | `cuMemsetD8_v2` rewrites every byte of the 1 MiB device buffer to `99 == 0x63`. `/sum` confirms `sample == 99`. The buffer is genuinely on-device — no host shadow. | curl → atenet → cuMemsetD8_v2 |
| 4 | `kubectl ate suspend actor gpu1` | Substrate's atelet calls ateom-gvisor's CheckpointWorkload. Before invoking `runsc checkpoint pause`, ateom-gvisor first runs `runsc exec supervisor /usr/local/bin/cuda-checkpoint --toggle --pid 1` (the `cmdDrainCUDA` helper) to drain CUDA state out of every live nvproxy client. The `runsc checkpoint pause` then serialises sentry state cleanly. Actor transitions `STATUS_RUNNING → STATUS_SUSPENDING → STATUS_SUSPENDED`. | substrate ateapi.SuspendActor → atelet.Checkpoint → ateom.CheckpointWorkload → cuda-checkpoint --toggle → runsc checkpoint |
| 5 | `GET /sum` after resume | Implicit resume on traffic. atenet routes the request, substrate restores the sandbox via `runsc restore`, nvproxy re-toggles the CUDA state, the agent serves the request. **`sample` MUST still be 99** — on-device GPU memory survived the round-trip. | curl → substrate restore → cuMemcpyDtoH_v2 |
| **III — Hygiene** | | | |
| 6 | `OpenShell.DeleteSandbox gpu1` | Driver reaps the actor; the pre-provisioned `gpu-counter` ActorTemplate survives. | Gateway → driver.delete_sandbox → ateapi.DeleteActor |

## Prerequisites

| Tool / resource | Version / details |
|---|---|
| Linux host with an NVIDIA GPU on `runsc nvproxy list-supported-drivers` (R570+ for cuda-checkpoint NVML support) | Verified on **L40S** (580.126.09), **H100** (570.195.03), **H200 NVL** (580.159.03, claimed to nvproxy as 580.126.20 since 580.159.03 is not in the supported list yet — patch-level 580.x ioctls are wire-compatible). Use the **gVisor nightly 2026-05-26 or later** (sha256 `5810ade5…7842`); `release-20260520.0` has a multi-container dev-gofer bug. |
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
# 1. Build the gpu-counter actor image — vanilla ubuntu:24.04 + python3.
#    No libcuda baking; atelet's injectNVIDIAAssetsIntoRootfs (substrate
#    >= 2026-05-28) mirrors the host's NVIDIA driver libs into the
#    rootfs at sandbox-create time. The gpu-counter agent loads
#    /usr/lib/x86_64-linux-gnu/libcuda.so.1 at startup.
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
| `gpu-counter.Dockerfile` | Layered on `ubuntu:24.04` (intentionally not `nvidia/cuda` — that base bundles a libcuda that may not match the host driver). **As of 2026-05-28** no libcuda / libnvidia-ml baking — atelet's `injectNVIDIAAssetsIntoRootfs` mirrors the host's NVIDIA driver libs into the rootfs at sandbox-create time. The setup script stages those libs once at `/run/ateom-gvisor/static-files/nvidia-libs/`. |
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
| `FATAL ERROR: checkpoint failed: ... save/restore binary is already set` | A previous checkpoint attempt left `kernel.SaveRestoreExecConfig` set after a failed exec. With the `fca2df4` substrate fix this is no longer reachable: `gpuSaveRestoreFlags()` returns nil and substrate drains CUDA externally via `runsc exec supervisor cuda-checkpoint --toggle --pid 1` (`cmdDrainCUDA`) before invoking `runsc checkpoint`. If you see this, your atelet/ateom-gvisor is older than `fca2df4`. |
| `FATAL ERROR: checkpoint failed: can't save with live nvproxy clients` | gVisor refuses to checkpoint a sandbox while CUDA contexts are open. Substrate's `cmdDrainCUDA` is supposed to drain them just before `runsc checkpoint`; if you hit this, either `cuda-checkpoint` is missing from `/run/ateom-gvisor/static-files/` (pre-stage it; see pre-flight) or the supervisor sub-container is named something other than `supervisor`. |
| `FATAL ERROR: starting sub-container [...]: inconsistent private memory files on restore: savedMFOwners = [pause:/]` | Symptom of supervisor crashing pre-checkpoint, NOT a gVisor multi-container bug. Almost always: workload's libcuda doesn't match the host driver, so `cuInit` returns `CUDA_ERROR_NO_DEVICE`, agent.py raises and exits, supervisor sub-container's memory is empty at checkpoint time. **2026-05-28+**: atelet auto-injects host libcuda, so this should not happen unless setup-host.sh didn't populate `/run/ateom-gvisor/static-files/nvidia-libs/`. Verify with `docker exec kind-control-plane ls /run/ateom-gvisor/static-files/nvidia-libs/ \| wc -l` (expect 50+ entries). |
| `nvproxy: failed to open device gofer nvidiactl: devutil.CtxDevGoferClient is not set` | The root sandbox booted without `--nvproxy` (no dev gofer wired). With `fca2df4` substrate puts the GPU spec on the pause container's OCI bundle so `runsc create pause` carries `--nvproxy` and the dev gofer is set up. If you still see this you're on a pre-`fca2df4` substrate, OR on `release-20260520.0`-era runsc which has a multi-container nvproxy bug — switch to the 2026-05-26 nightly. |
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

1. ~~**atelet should run `nvidia-container-cli configure` against the rootfs.**~~
   ✅ **Landed 2026-05-28** (substrate-gpu, not yet pushed to PR #96).
   atelet now has `injectNVIDIAAssetsIntoRootfs` in `cmd/atelet/oci.go`
   which mirrors host NVIDIA driver libs (real `.so` + symlinks) from
   `/run/ateom-gvisor/static-files/nvidia-libs/` into the rootfs at
   sandbox-create time. setup-host.sh stages the libs there once per box
   (see Appendix I of [`2026-05-27-gpu-passthrough-runbook.md`](https://github.com/dims/notes/blob/main/openshell-on-substrate/2026-05-27-gpu-passthrough-runbook.md)).
   The Dockerfile no longer needs `COPY libcuda.so.<driver>` — verified
   end-to-end on bigbox-h200 with an unmodified `ubuntu:24.04 + python3`
   workload image (full 6-beat demo passes; CUDA buffer + dev_ptr
   preserved across suspend/resume). atelet does its own copy+symlink
   rather than exec'ing `nvidia-container-cli configure` because atelet
   runs on `distroless/static-debian13` and has no dynamic linker for
   `libnvidia-container.so.1`; the end state is identical.

2. **Kind-node DaemonSet for cuda-checkpoint + wrapper.** The demo
   pre-stages those binaries into `/run/ateom-gvisor/static-files/`
   inside `kind-control-plane` as a one-shot manual step (see
   pre-flight). For multi-node kind clusters or for a production-cleaner
   single-node bring-up, ship a DaemonSet that drops them automatically
   so atelet's `prepareOCIDirectory` finds them at sandbox-create time.

3. **gVisor nvproxy ABI list extension.** Host drivers not in
   `runsc nvproxy list-supported-drivers` need substrate told a
   neighbouring supported version (e.g. host 580.159.03 → tell substrate
   580.126.20). Patch-level NVIDIA 580.x ioctls are wire-compatible so
   this works, but upstream gVisor should accept a PR adding the
   missing driver versions.

The earlier "gVisor multi-container restore quirk" / `savedMFOwners=[pause:/]`
follow-up turned out to be a downstream symptom of the libcuda mismatch above
(supervisor crashed at boot before checkpoint), not a gVisor bug. Once libcuda
matched the host driver, restore worked cleanly. Verified on H100 + H200 on
2026-05-27.

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
