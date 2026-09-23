# openshell-driver-substrate

An [Agent Substrate](https://github.com/agent-substrate/substrate) compute
driver for [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell), plus the
harness needed to reproduce a working end-to-end run.

OpenShell's per-request sandbox becomes a Substrate actor, so it can be
snapshotted and resumed instead of cold-started.

**Status:** the real, unpatched OpenShell runs on Substrate's **micro-VM**
backend. `openshell-sandbox` passes all seven of its runtime-qualification
gates, consumes its bootstrap, and opens its boundary control listener;
`openshell-supervisor` supervises it. The two-container actor completes
create → golden snapshot → resume → suspend → resume.

It does **not** run under gVisor and cannot — see [gVisor](#gvisor-does-not-work).

Getting there needed six commits in Substrate and a one-line kata kernel change.
None are merged upstream; [`docs/upstream-branches.md`](docs/upstream-branches.md)
is the index of where they live.

---

## How it fits together

```
OpenShell CLI / gateway  (stock, unmodified binary)
        |
        |  --compute-driver-socket /path/to/socket
        v
openshell-driver-substrate   (this repo; its own process)
        |
        |  tonic gRPC, ateapi.Control
        v
Substrate ate-api-server  (ActorTemplate + Actor are gRPC resources)
        |
        v
ActorTemplate -> golden snapshot -> Actor (micro-VM) -> OpenShell workload
```

The driver implements OpenShell's `ComputeDriver` gRPC trait and maps it onto
Substrate's actor lifecycle:

| OpenShell call | Substrate call |
|---|---|
| `create_sandbox` | `CreateActorTemplate` (idempotent, reused by content hash) + `CreateActor` + `ResumeActor` |
| `start_sandbox` | `ResumeActor` |
| `stop_sandbox` | `SuspendActor` |
| `delete_sandbox` | `DeleteActor` with `any_state: true` |
| `get_sandbox` / `list_sandboxes` | `GetActor` / `ListActors` |
| `watch_sandboxes` | polls `ListActors` every 2s (Substrate has no watch RPC) |
| `ensure_workspace` | `CreateAtespace` |
| `delete_workspace` | no-op (Substrate does not garbage-collect templates) |

One `ActorTemplate` is reused for every actor with the same image and command —
its name is a content hash of those two fields — so only the first
`create_sandbox` for a workload pays for a golden-snapshot build.

The driver registers out of process over a Unix socket, exactly as OpenShell's
in-tree `openshell-driver-podman` / `-docker` / `-kubernetes` / `-vm` binaries
do. A stock gateway picks it up with
`--compute-driver substrate --compute-driver-socket <path>`. No OpenShell fork
is involved; `openshell-core` is a plain git dependency pinned by rev.

`src/` is three files: `lib.rs` (the trait impl), `template.rs` (ActorTemplate
synthesis), `main.rs` (the socket server).

## Layout

```
src/                  the driver
proto/                ateapi.proto, used by build.rs
tests/live.rs         full lifecycle against a real cluster
docs/                 upstream branch index
harness/
  bootstrap-gen/      mints the Ed25519/JWT/TLS bundle the binaries require
  capability-probe/   Go probe for uid, caps, seccomp, Landlock, task memory
  images/             derived sandbox image, and the non-root probe image
  manifests/          ActorTemplate / WorkerPool templates (gVisor and micro-VM)
  scripts/            guest-kernel build + staging, version retargeting
```

## Build and test

```sh
cargo build --release
cargo test --lib          # no cluster needed
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Needs rustc ≥ 1.94 (current OpenShell's floor). `tests/live.rs` needs a
reachable `ate-api-server` and is ignored by default.

---

## Reproducing a working run

### 0. Prerequisites

A Linux host with **`/dev/kvm`**. The micro-VM backend is the only sandbox
class the OpenShell sandbox runs on, and it needs real virtualisation — a cloud
VM without nested virt will not do. Check before anything else:

```sh
ls /dev/kvm && grep -oE 'vmx|svm' /proc/cpuinfo | head -1
```

If that comes up empty you can still run everything in
[Debugging](#debugging) on gVisor, but not the OpenShell sandbox itself.

Then:

```sh
sudo apt-get install -y build-essential protobuf-compiler pkg-config \
                       libssl-dev jq gettext-base          # envsubst
# docker, go, rust, kubectl, kind
curl -fsSL https://sh.rustup.rs | sh -s -- -y
# go >= 1.23, kubectl, kind from their upstream installers
```

`ko` does **not** need installing separately — Substrate vendors it and every
build must go through its wrapper, `./hack/run-tool.sh ko ...`. A globally
installed `ko` is not what the install scripts use.

### 1. Patch Substrate and bring up a cluster

```sh
git clone https://github.com/dims/substrate && cd substrate
git checkout lean-integration          # the six commits, see docs/upstream-branches.md
export GOFLAGS=-buildvcs=false
./hack/create-kind-cluster.sh
./hack/install-ate-kind.sh --deploy-ate-system
```

On a **fresh** cluster the node is labelled with the build version
automatically, so no retargeting is needed. That only applies after a rebuild
(see [Retargeting](#retargeting-after-a-rebuild)).

Without these commits:

- every actor container runs as root regardless of its image's `USER`;
- no container declaring a non-root `USER` can execute anything at all,
  because it cannot search its own root directory;
- a non-root container cannot write its own durable-dir volume;
- `no_new_privs` is never set, and sysctls never reach the guest.

### 2. Install the micro-VM backend

From the Substrate repo:

```sh
ARCH=amd64 hack/install-microvm-deps.sh --install
```

This downloads the kata asset set, stages it to the cluster object store, and
applies the cluster-wide `microvm` SandboxConfig.

### 3. Rebuild the guest kernel

Stock kata ships `CONFIG_CROSS_MEMORY_ATTACH=n`, which the sandbox cannot
tolerate (see [Why the kernel rebuild](#why-the-kernel-rebuild)).

```sh
harness/scripts/build-guest-kernel.sh
harness/scripts/stage-guest-kernel.sh <built-vmlinux> <path-to-substrate-repo>
```

### 4. Build the OpenShell images

From an OpenShell checkout, stage the binaries first — `cargo-zigbuild` is
required, not optional, because a plain native build links
`openshell-supervisor` against `libgcc_s.so.1`, which the distroless base does
not ship:

```sh
PREBUILT_ARCH=amd64 tasks/scripts/stage-prebuilt-binaries.sh sandbox
PREBUILT_ARCH=amd64 tasks/scripts/stage-prebuilt-binaries.sh supervisor
docker build -f deploy/docker/Dockerfile.sandbox    -t <registry>/openshell-sandbox:dev .
docker build -f deploy/docker/Dockerfile.supervisor -t <registry>/openshell-supervisor:dev .
```

### 5. Mint the credentials

The binaries speak a real protocol: an Ed25519-signed JWT pair and generated
TLS material bound to one session id.

```sh
openssl genpkey -algorithm ed25519 -out out/signing.key.pem
openssl pkey -in out/signing.key.pem -pubout -out out/signing.pub.pem
cargo run --manifest-path harness/bootstrap-gen/Cargo.toml -- out/
```

That writes `bootstrap.json`, `server.crt`, `server.key` (the sandbox's side)
and `runtime-descriptor.json`, `auth.json` (the supervisor's).

**The driver is an allowed trust anchor for this.**
`SandboxLaunchAuthentication` validates against its own embedded verification
key; nothing requires that key to trace back to a gateway process. A throwaway
keypair produces a bundle that validates cleanly with no gateway running.

### 6. Package the credentials

The two halves are delivered differently, and this is not arbitrary:

- **Supervisor** — a `FROM scratch` image with `supervisor/` in it, mounted as
  a read-only `ImageVolumeSource`. It only reads its files.
- **Sandbox** — baked into a layer on top of the sandbox image
  (`harness/images/sandbox-with-bootstrap`). The sandbox **consumes** its
  bootstrap, unlinking the file after reading it, so a read-only image volume
  fails with `Read-only file system`. And because Substrate discards image file
  ownership, `--chown` has no effect, so the tree has to be world-writable for
  the unlink to succeed — modes are preserved even though ownership is not.

Both of these are workarounds for the ownership gap in
[Known gaps](#known-gaps); fix that and they go away.

### 7. Create the pool and template

```sh
export ATESPACE=ate-openshell-microvm BUCKET_NAME=ate-snapshots
export SUBSTRATE_VERSION=$(git -C <substrate> describe --always --dirty)
export ATEOM_MICROVM_IMAGE=... SANDBOX_BAKED_IMAGE=... SUPERVISOR_IMAGE=... BOOTSTRAP_FILES_IMAGE=...

harness/scripts/render.sh harness/manifests/openshell-microvm-pool.yaml.tmpl | kubectl apply -f -
kubectl-ate create atespace "${ATESPACE}"
harness/scripts/render.sh harness/manifests/openshell-microvm-template.yaml.tmpl \
  | kubectl-ate create actor-template -f -
```

The atespace must exist first, or template creation fails with
`FailedPrecondition ... persistence: failed precondition`, which does not name
the cause.

### 8. Run it

```sh
kubectl-ate create actor osh-1 --atespace "${ATESPACE}" --template openshell-microvm
kubectl-ate resume  actor -a "${ATESPACE}" osh-1     # -> ACTOR_STATE_RUNNING
kubectl-ate suspend actor -a "${ATESPACE}" osh-1
kubectl-ate resume  actor -a "${ATESPACE}" osh-1     # -> ACTOR_STATE_RUNNING
```

**Actor state is not proof the containers are alive.** The golden-snapshot
warmup does not verify it. Under gVisor a dead sub-container surfaces later as
an inconsistent-checkpoint restore failure; under micro-VM it does not surface
at all, because the snapshot is whole-VM memory — an actor reaches
`ACTOR_STATE_RUNNING` with a dead container inside it. Always check container
output.

---

## Debugging

### The fast loop

`openshell-sandbox capability-probe` runs the whole qualification gate on its
own and prints a JSON report — no two-container setup, no credentials:

```sh
harness/scripts/render.sh harness/manifests/capability-probe-template.yaml.tmpl \
  | kubectl-ate create actor-template -f -
```

A passing run reports `"qualified":true` with `landlock_abi: 7`,
`seccomp_notification: true`, `socket_virtualization: true`,
`dns_relay_bind: true`.

### The gates

`run_boundary` calls `qualify_runtime()` unconditionally, and the result is
passed into `openshell_sandbox::run()` — it is not a check that can be skipped.

| # | Gate | gVisor | micro-VM |
|---|---|---|---|
| 1 | non-root UID **and** GID | needs the Substrate fixes | pass |
| 2 | all five capability sets empty | `capabilities.drop: ["ALL"]` | pass |
| 3 | `no_new_privs == 1` | needs `4fc5d550` | pass |
| 4 | same-UID task-memory probe | — | pass |
| 5 | Landlock allow/deny | **fails: no Landlock** | pass (needs a writable `/tmp`) |
| 6 | seccomp notification | — | pass (needs the kernel rebuild) |
| 7 | socket virtualization, DNS relay bind, Landlock ABI ≥ 3 | — | pass (needs `33397540`) |

Gate 5 needs `/tmp` because the Landlock probe builds its test tree under
`std::env::temp_dir()`, and the sandbox image contains **exactly one file** —
the binary. No `/tmp`, no `/etc`, nothing.

### Generic non-root repro

`harness/manifests/nonroot-probe-template.yaml.tmpl` is a busybox image whose
only distinguishing feature is `USER 1000:1000`. It reproduces the Substrate
bugs with no OpenShell parts involved, and runs on gVisor — no KVM needed.

```sh
# the busybox binary MUST be static; busybox:latest's is not (see the Dockerfile)
docker create --name bb busybox:musl && docker cp bb:/bin/busybox ./busybox && docker rm bb
docker build -t <registry>/nonroot-probe:dev harness/images/nonroot-probe
docker push <registry>/nonroot-probe:dev

export ATESPACE=ate-probe BUCKET_NAME=ate-snapshots \
       SANDBOX_CLASS=SANDBOX_CLASS_GVISOR SANDBOX_CONFIG_NAME=gvisor-default \
       NONROOT_PROBE_IMAGE=<registry>/nonroot-probe@sha256:... \
       ATEOM_GVISOR_IMAGE=$(cd <substrate> && ./hack/run-tool.sh ko build ./cmd/ateom-gvisor --platform=linux/amd64 | tail -1) \
       SUBSTRATE_VERSION=$(kubectl get node <node> -o jsonpath='{.metadata.labels.ate\.dev/substrate-version}')

harness/scripts/render.sh harness/manifests/gvisor-pool.yaml.tmpl | kubectl apply -f -
kubectl-ate create atespace "${ATESPACE}"
harness/scripts/render.sh harness/manifests/nonroot-probe-template.yaml.tmpl | kubectl-ate create actor-template -f -
kubectl-ate create actor np-1 --atespace "${ATESPACE}" --template nonroot-probe
kubectl-ate resume actor -a "${ATESPACE}" np-1      # -> ACTOR_STATE_RUNNING
```

Two traps this probe walks into, both of which present as confusing errors:
a dynamically linked busybox in a `FROM scratch` image fails with
`failed to load /busybox: no such file or directory`, and a bare `sleep`
(no applet symlinks, no PATH) exits the container silently, so the golden
snapshot captures only `_pause` and every resume fails with
`savedMFOwners = [_pause:/]`.

`harness/capability-probe` reports uid/gid, all five capability sets,
`no_new_privs`, whether `seccomp(SECCOMP_FILTER_FLAG_NEW_LISTENER)` succeeds,
and whether `/proc/<pid>/mem` and `process_vm_readv` work before and after
`PR_SET_DUMPABLE(0)`.

### Retargeting after a rebuild

atelet and ateom are versioned per node. An install creates a DaemonSet named
`atelet-<version>` with a `nodeSelector` on `ate.dev/substrate-version`, and
deliberately does **not** move an already-labelled node — during an upgrade the
operator owns that value. So a rebuild does nothing until the node label and
every `WorkerPool`'s `nodeSelector` move too, and a pool whose selector does not
match sits in `Pending` forever. A dirty worktree makes `git describe` yield
`<sha>-dirty`, and the label has to match that exactly.

```sh
harness/scripts/retarget-substrate-version.sh <node> <version> [ateom-image]
```

---

## Why the kernel rebuild

Stock kata ships `CONFIG_CROSS_MEMORY_ATTACH=n`, so `process_vm_readv` returns
`ENOSYS` in the guest.

OpenShell's `task_memory::read_exact` tries `process_vm_readv` first and falls
back to `/proc/{tid}/mem` when the error is `EPERM`, `EACCES` **or `ENOSYS`**
(`syscall_profile_denied`). The missing syscall silently routes the read onto
the `/proc` path — and `qualify_runtime()` has by then called
`PR_SET_DUMPABLE(0)` on purpose ("must be nondumpable before it handles
bootstrap or channel secrets"), which re-owns the process's `/proc` entries to
root and locks out a non-root task with no capabilities.

So the reported failure is an `EACCES` from the *second* attempt, and the real
cause — a missing syscall — is invisible. Only the first has to be fixed.

**Do not check this with `strings vmlinux | grep process_vm_readv`.** With the
option off, the syscall table still emits `__x64_sys_process_vm_readv` as a
weak alias to `sys_ni_syscall`, so the symbol is present either way. Call the
syscall and look for `ENOSYS`.

## gVisor does not work

`openshell-sandbox` requires Landlock ABI ≥ 3. gVisor implements no Landlock at
all: `runsc help syscalls` lists no Landlock rows and its highest implemented
syscall number is 441, while the Landlock syscalls are 444–446. The ABI
constants exist in `pkg/abi/linux/landlock.go`, but nothing in `pkg/sentry/` or
`runsc/` references them. NVIDIA's own merged PR
[#1585](https://github.com/NVIDIA/OpenShell/pull/1585) says it plainly: *"On
kernels without Landlock (e.g. gVisor's sentry returns `ENOSYS` for syscall
444)"*.

This is architectural. No flag, ActorTemplate field, or Substrate change moves
it.

Upstream history, for anyone tempted to revive the older attempts:

| PR | What it does | State |
|---|---|---|
| [#1585](https://github.com/NVIDIA/OpenShell/pull/1585) | Probe Landlock before build, skip on unsupported kernels — logging only | merged |
| [#1549](https://github.com/NVIDIA/OpenShell/pull/1549) | `--skip-bootstrap` for netns / supervisor-seccomp / workload-seccomp | closed unmerged |
| [#1548](https://github.com/NVIDIA/OpenShell/pull/1548) | Same idea via an env var | closed unmerged |

None address the qualification gate. #1548 and #1549 targeted the pre-RFC-0012
architecture, where `openshell-sandbox` *was* the supervisor and the skippable
steps were netns and seccomp. RFC-0012 split out the separate boundary binary
and introduced this gate, which made the stack *less* compatible with an outer
sandbox, not more. Unblocking gVisor needs a degraded qualification mode where
the outer sandbox is the enforcing boundary — the argument #1549 made, which
NVIDIA closed.

## Known gaps

- **Image file ownership is discarded.** Substrate's `unpackLayer` never chowns
  to the layer tar's uid/gid, so everything extracts root-owned. An image
  shipping files owned by its runtime user — the norm for a non-root image —
  lands unusable by that user. atelet cannot fix this itself: it runs as uid 0
  with `capabilities.drop: ["ALL"]`, so it has no `CAP_CHOWN`. The natural home
  is `FinalizeLayer`, which already exists in privileged ateom for this class
  of problem. That is a layer-format change, and it is what forces the
  packaging workarounds in step 6.
- **`no_new_privileges` is unconditional**, with no `SecurityContext` field to
  opt out of it.
- **`Linux.Seccomp`, `Process.ApparmorProfile` and `SelinuxLabel` are still not
  forwarded** to the kata agent. `Linux.Sysctl` now is, but only Substrate sets
  it — an ActorTemplate cannot.
- **A missing `WorkerPool` is silent.** `create_sandbox` hangs forever with an
  empty `ActorTemplate.status`, indistinguishable from a golden snapshot still
  building.
- **Templates are never garbage-collected.**
- **The driver has not been driven by a real `openshell-gateway` process.**
  Every run so far calls the driver's methods directly or applies templates by
  hand. The `--compute-driver-socket` wiring is confirmed to exist and match the
  driver's shape, but has not been exercised end to end.

## History

The previous contents of `main` — an earlier proof of concept built against
APIs that have since changed — are preserved on the `old-main` branch.

## License

Apache-2.0. See [LICENSE](LICENSE).
