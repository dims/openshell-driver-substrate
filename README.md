# openshell-driver-substrate

An [Agent Substrate](https://github.com/agent-substrate/substrate) compute
driver for [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell), plus the
harness needed to reproduce a working end-to-end run.

OpenShell's per-request sandbox becomes a Substrate actor, so it can be
snapshotted and resumed instead of cold-started.

The real, unpatched OpenShell runs on Substrate's **micro-VM** sandbox class.
`openshell-sandbox` passes all seven of its runtime-qualification gates,
consumes its bootstrap, and opens its boundary control listener. The
two-container actor completes create → golden snapshot → resume → suspend →
resume, and a stock `openshell-gateway` drives the whole lifecycle through this
driver.

**The workload does not run yet.** `openshell-supervisor` starts and logs
`Starting sandbox supervision`, but never attaches to the boundary, so it never
starts the agent. Nothing listens on the actor's ports: a request through
atenet-router returns 502 on both the default port and a CONNECT-tunnelled one.
That is the next thing to fix.

Micro-VM needs real virtualisation, so the host needs **nested virt**
(`/dev/kvm`). A cloud VM without it cannot run the sandbox.

Six commits in Substrate and a one-line kata kernel change are required and
none are merged upstream. [`docs/upstream-branches.md`](docs/upstream-branches.md)
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
`create_sandbox` for a workload pays for a golden-snapshot build. Synthesized
templates are `SANDBOX_CLASS_MICROVM` and name the `microvm` SandboxConfig.

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
  images/             derived sandbox image with its bootstrap baked in
  manifests/          ActorTemplate / WorkerPool templates
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

A Linux host with **`/dev/kvm`**. Check before anything else:

```sh
ls /dev/kvm && grep -oE 'vmx|svm' /proc/cpuinfo | head -1
```

Then:

```sh
sudo apt-get install -y build-essential protobuf-compiler pkg-config \
                       libssl-dev jq gettext-base \
                       libelf-dev flex bison bc dwarves    # guest kernel (step 3)
curl -fsSL https://sh.rustup.rs | sh -s -- -y
curl -fsSL https://mise.run | sh                           # OpenShell's toolchain (step 4)
# go >= 1.23, docker, kubectl, kind from their upstream installers
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
automatically. Retargeting only applies after a rebuild
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
ARCH=amd64 ATE_INSTALL_KIND=true hack/install-microvm-deps.sh --install
```

This downloads the kata asset set, stages it to the cluster object store, and
applies the cluster-wide `microvm` SandboxConfig.

`ATE_INSTALL_KIND=true` is what picks the in-cluster rustfs bucket. Without it
the script takes the GKE path and fails on `gcloud: command not found`, after
it has already assembled the assets.

### 3. Rebuild the guest kernel

Stock kata ships `CONFIG_CROSS_MEMORY_ATTACH=n`, which the sandbox cannot
tolerate (see [Why the kernel rebuild](#why-the-kernel-rebuild)).

```sh
harness/scripts/build-guest-kernel.sh
harness/scripts/stage-guest-kernel.sh <built-vmlinux> <path-to-substrate-repo>
```

### 4. Build the OpenShell images

From an OpenShell checkout, stage the binaries first. The staging script runs
every cargo invocation through `mise x`, so `mise install` has to have run in
that checkout; it pins the Rust, zig and `cargo-zigbuild` versions the build
expects. zigbuild is required, not optional, because a plain native build links
`openshell-supervisor` against `libgcc_s.so.1`, which the distroless base does
not ship:

```sh
mise trust && mise install
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

`harness/scripts/package-credentials.sh` does both and prints the two image
references the template needs:

```sh
harness/scripts/package-credentials.sh out/ <registry>/openshell-sandbox:dev <registry>
```

The workload runs in the sandbox container's filesystem, so the script bakes a
**static** busybox in beside the bootstrap. A dynamic one fails with
`no such file or directory` against the distroless base. Override the source
with `BUSYBOX=/path/to/busybox`.

Both halves are workarounds for the ownership gap in
[Known gaps](#known-gaps); fix that and they go away.

### 7. Create the pool and template

```sh
export ATESPACE=ate-openshell-microvm BUCKET_NAME=ate-snapshots
export SUBSTRATE_VERSION=$(git -C <substrate> describe --always --dirty)
export KO_DOCKER_REPO=<registry>       # ko writes nothing without it, and fails silently
export ATEOM_MICROVM_IMAGE=$(cd <substrate> && ./hack/run-tool.sh ko build ./cmd/ateom-microvm --platform=linux/amd64 | tail -1)
# from step 6, plus the supervisor image from step 4
export SANDBOX_BAKED_IMAGE=... SUPERVISOR_IMAGE=... BOOTSTRAP_FILES_IMAGE=...

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
warmup does not verify it, and a micro-VM snapshot is whole-VM memory, so a
dead container never surfaces as a restore failure — an actor reaches
`ACTOR_STATE_RUNNING` with a dead container inside it. Always check container
output.

### 9. Drive it from a gateway

A stock, unforked `openshell-gateway` dispatches to this driver over a Unix
socket. Any driver name that is not one of its built-ins resolves to an
external driver, so no OpenShell change is needed.

```sh
kubectl port-forward -n ate-system svc/api 8443:443 &
kubectl create token ate-client -n ate-system \
  --audience api.ate-system.svc --duration=24h > creds/token
kubectl get clustertrustbundle servicedns.podcert.ate.dev:identity:primary-bundle \
  -o jsonpath='{.spec.trustBundle}' > creds/ctb.crt

openshell-driver-substrate --bind-socket /tmp/substrate.sock \
  --api-endpoint 127.0.0.1:8443 \
  --api-tls-ca creds/ctb.crt --api-tls-server-name api.ate-system.svc \
  --api-bearer-token-path creds/token \
  --atespace "${ATESPACE}" --snapshots-location "gs://${BUCKET_NAME}/${ATESPACE}/" &

openshell-gateway --compute-driver substrate \
  --compute-driver-socket /tmp/substrate.sock --disable-tls
```

`ate-api-server` needs TLS 1.3, the `servicedns` trust bundle, server name
`api.ate-system.svc`, and a bearer token whose audience is that same name. It
does not require a client certificate.

Create a sandbox with `grpcurl` (the gateway serves no reflection, so pass the
proto):

```sh
grpcurl -plaintext -import-path <openshell>/proto -proto openshell.proto -d '{
  "workspace_scope": {"workspace": "default"},
  "name": "alice",
  "spec": {"log_level": "info",
           "template": {"image": "<sandbox-baked-image>"},
           "policy": {"version": 1}}
}' 127.0.0.1:17670 openshell.v1.OpenShell/CreateSandbox
```

The driver synthesizes a `SANDBOX_CLASS_MICROVM` ActorTemplate, waits for its
golden snapshot, then creates and resumes an actor named by the sandbox id.

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

| # | Gate | What it needs |
|---|---|---|
| 1 | non-root UID **and** GID | `cf699047`, and an image that declares `USER` |
| 2 | all five capability sets empty | `capabilities.drop: ["ALL"]` |
| 3 | `no_new_privs == 1` | `4fc5d550` |
| 4 | same-UID task-memory probe | the kernel rebuild |
| 5 | Landlock allow/deny | a writable `/tmp` |
| 6 | seccomp notification | the kernel rebuild |
| 7 | socket virtualization, DNS relay bind, Landlock ABI ≥ 3 | `33397540` |

Gate 5 needs `/tmp` because the Landlock probe builds its test tree under
`std::env::temp_dir()`, and the sandbox image contains **exactly one file** —
the binary. No `/tmp`, no `/etc`, nothing.

### Where the output is

`kubectl-ate logs actor` returns nothing once an actor has been restored from a
snapshot: the container's stdout is the descriptor captured in the image and no
longer reaches the log pipe. The worker pod forwards actor logs, so read those
instead:

```sh
kubectl logs -n "${ATESPACE}" <worker-pod> | grep -i openshell
```

`Boundary control listener ready` is the line that says the sandbox cleared
every gate, consumed its bootstrap, and is serving.

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
- **`SecurityContext` has no `runAsUser`.** A container's identity comes from
  its image's `Config.User` and nowhere else, so two identities need two images.
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

## License

Apache-2.0. See [LICENSE](LICENSE).
