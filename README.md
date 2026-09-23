# openshell-driver-substrate

An [Agent Substrate](https://github.com/agent-substrate/substrate) compute
driver for [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell), plus the
harness to reproduce a working run.

OpenShell's per-request sandbox becomes a Substrate actor, so it can be
snapshotted and resumed instead of cold-started. Stock, unpatched OpenShell
runs on Substrate's **micro-VM** sandbox class. The workload in
[`examples/helpdesk`](examples/helpdesk) runs inside a real OpenShell sandbox,
its egress is mediated, it is reachable through atenet-router, and its memory
survives a suspend and resume.

A stock `openshell-gateway` drives the driver over `--compute-driver-socket`
and creates sandboxes through it, but cannot bring one to `Ready`; see
[Known gaps](#known-gaps).

Micro-VM needs nested virtualisation (`/dev/kvm`). Eight commits in Substrate
are required and none are merged upstream;
[`docs/upstream-branches.md`](docs/upstream-branches.md) lists them. The guest
kernel is stock kata.

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
| `create_sandbox` | `CreateActorTemplate` (reused by content hash) + `CreateActor` + `ResumeActor` |
| `start_sandbox` | `ResumeActor` |
| `stop_sandbox` | `SuspendActor` |
| `delete_sandbox` | `DeleteActor` with `any_state: true` |
| `get_sandbox` / `list_sandboxes` | `GetActor` / `ListActors` |
| `watch_sandboxes` | polls `ListActors` every 2s (Substrate has no watch RPC) |
| `ensure_workspace` | `CreateAtespace` |
| `delete_workspace` | no-op: one atespace serves every workspace |

One `ActorTemplate` is reused for every actor whose template comes out identical
— its name is a hash of the template itself — so only the first
`create_sandbox` for a workload pays for a golden-snapshot build. Nothing that
varies per sandbox goes into a template: a snapshot freezes process memory, so
a per-sandbox value would come back identical in every actor restored from it.
Synthesized templates are `SANDBOX_CLASS_MICROVM`, name the `microvm`
SandboxConfig, drop every capability, and mount a durable dir at `/tmp`.

The driver registers out of process over a Unix socket, as OpenShell's in-tree
`openshell-driver-podman` / `-docker` / `-kubernetes` / `-vm` binaries do. A
stock gateway picks it up with
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
examples/helpdesk/    the workload: a Python agent under OpenShell
harness/
  bootstrap-gen/      mints the Ed25519/JWT/TLS bundle the binaries require
  images/             sandbox image with its bootstrap baked in
  manifests/          WorkerPool and capability-probe templates
  scripts/            credential baking, template rendering, version retargeting
```

## Build and test

```sh
cargo build --release
cargo test --lib          # no cluster needed
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Needs rustc ≥ 1.94 (OpenShell's floor). `tests/live.rs` needs a reachable
`ate-api-server` and is ignored by default.

---

## Reproducing a working run

### 0. Prerequisites

A Linux host with **`/dev/kvm`**:

```sh
ls /dev/kvm && grep -oE 'vmx|svm' /proc/cpuinfo | head -1
```

Then:

```sh
sudo apt-get install -y build-essential protobuf-compiler pkg-config \
                       libssl-dev jq gettext-base
curl -fsSL https://sh.rustup.rs | sh -s -- -y
curl -fsSL https://mise.run | sh                           # OpenShell's toolchain (step 3)
# go >= 1.23, docker, kubectl, kind from their upstream installers
```

`ko` does **not** need installing separately — Substrate vendors it and every
build must go through its wrapper, `./hack/run-tool.sh ko ...`.

### 1. Patch Substrate and bring up a cluster

```sh
git clone https://github.com/dims/substrate && cd substrate
git checkout lean-integration          # the eight commits, see docs/upstream-branches.md
export GOFLAGS=-buildvcs=false
./hack/create-kind-cluster.sh
./hack/install-ate-kind.sh --deploy-ate-system
make build-atectl                      # bin/kubectl-ate; put it on PATH
```

`create-kind-cluster.sh` also starts a local image registry at
`localhost:5001`; that is `<registry>` in every step below.

On a **fresh** cluster the node is labelled with the build version
automatically. Retargeting only applies after a rebuild
(see [Retargeting](#retargeting-after-a-rebuild)).

Without these commits:

- every actor container runs as root regardless of its image's `USER`;
- no container declaring a non-root `USER` can execute anything at all,
  because it cannot search its own root directory;
- a non-root container cannot write its own durable-dir volume;
- `no_new_privs` is never set, and sysctls never reach the guest;
- a suspend of a live OpenShell sandbox never completes.

### 2. Install the micro-VM backend

From the Substrate repo:

```sh
ARCH=amd64 ATE_INSTALL_KIND=true hack/install-microvm-deps.sh --install
```

This downloads the kata asset set, stages it to the cluster object store, and
applies the cluster-wide `microvm` SandboxConfig. `ATE_INSTALL_KIND=true`
picks the in-cluster rustfs bucket; without it the script takes the GKE path
and fails on `gcloud: command not found`.

### 3. Build the OpenShell images

From an OpenShell checkout at the rev `Cargo.toml` pins. The staging script
runs cargo through `mise x`, so `mise install` has to have run in that
checkout. zigbuild is required: a plain native build links
`openshell-supervisor` against `libgcc_s.so.1`, which the distroless base does
not ship.

```sh
mise trust && mise install
PREBUILT_ARCH=amd64 tasks/scripts/stage-prebuilt-binaries.sh sandbox
PREBUILT_ARCH=amd64 tasks/scripts/stage-prebuilt-binaries.sh supervisor
docker build -f deploy/docker/Dockerfile.sandbox    -t <registry>/openshell-sandbox:dev .
docker build -f deploy/docker/Dockerfile.supervisor -t <registry>/openshell-supervisor:dev .
docker push <registry>/openshell-sandbox:dev
docker push <registry>/openshell-supervisor:dev
export SUPERVISOR_IMAGE=$(docker inspect --format '{{index .RepoDigests 0}}' <registry>/openshell-supervisor:dev)
```

Templates reference images by digest; a bare tag fails atelet's pull cache.

### 4. Mint the credentials

The tokens minted here last one hour (below), so do steps 4, 5 and 7 in one
sitting, after 1 to 3 and 6.

An Ed25519-signed JWT pair and TLS material bound to one session id:

```sh
mkdir -p out
openssl genpkey -algorithm ed25519 -out out/signing.key.pem
openssl pkey -in out/signing.key.pem -pubout -out out/signing.pub.pem
cargo run --manifest-path harness/bootstrap-gen/Cargo.toml -- out/
```

That writes `bootstrap.json`, `server.crt`, `server.key` (the sandbox's side)
and `runtime-descriptor.json`, `auth.json` (the supervisor's).

The tokens are valid for **one hour**, OpenShell's maximum for a session
token. A gateway refreshes a sandbox's tokens; this hand-applied harness has
nothing to refresh from, so run the actor within the hour. After that the
supervisor logs `Starting sandbox supervision` and then, about 90 seconds
later, `boundary unavailable ... timed out while waiting for remote boundary
boot`, with no mention of authentication. Mint again and repeat step 5.

Any keypair is accepted as a trust anchor: `SandboxLaunchAuthentication`
validates against its own embedded key, and nothing ties that key to a
gateway.

### 5. Bake the credentials

```sh
harness/scripts/package-credentials.sh out/ <registry>/openshell-sandbox:dev <registry>
```

This prints `SANDBOX_BAKED_IMAGE` — the sandbox image with its bootstrap baked
into the writable rootfs, used by the gateway path and the capability probe —
and leaves `out/bootstrap.tar` for the helpdesk image.

Baked rather than mounted, because the sandbox **consumes** its bootstrap,
unlinking the file after reading it, so a read-only image volume fails with
`Read-only file system`. Substrate discards image file ownership, so `--chown`
has no effect and the tree is world-writable instead (see
[Known gaps](#known-gaps)).

### 6. Create the pool

```sh
export ATESPACE=ate-openshell-microvm BUCKET_NAME=ate-snapshots
export SUBSTRATE_VERSION=$(git -C <substrate> describe --always --dirty)
export KO_DOCKER_REPO=<registry>
export ATEOM_MICROVM_IMAGE=$(cd <substrate> && ./hack/run-tool.sh ko build ./cmd/ateom-microvm --platform=linux/amd64 | tail -1)

harness/scripts/render.sh harness/manifests/openshell-microvm-pool.yaml.tmpl | kubectl apply -f -
kubectl-ate create atespace "${ATESPACE}"
```

The atespace must exist before any template, or template creation fails with
`FailedPrecondition ... persistence: failed precondition`.

### 7. Run the example

[`examples/helpdesk`](examples/helpdesk) builds the two images, creates the
template and the actor, reaches the agent through atenet-router, and suspends
and resumes it.

**Actor state is not proof the containers are alive.** A micro-VM snapshot is
whole-VM memory, so a dead container never surfaces as a restore failure. Read
the worker pod log.

### 8. Drive it from a gateway

A stock `openshell-gateway` dispatches to this driver over a Unix socket. Any
driver name that is not one of its built-ins resolves to an external driver.

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

Create a sandbox with `grpcurl` (the gateway serves no reflection):

```sh
grpcurl -plaintext -import-path <openshell>/proto -proto openshell.proto -d '{
  "workspace_scope": {"workspace": "default"},
  "name": "alice",
  "spec": {"log_level": "info",
           "template": {"image": "<SANDBOX_BAKED_IMAGE>"},
           "policy": {"version": 1}}
}' 127.0.0.1:17670 openshell.v1.OpenShell/CreateSandbox
```

The driver synthesizes an ActorTemplate, waits for its golden snapshot, then
creates and resumes an actor named by the sandbox id. The gateway holds the
sandbox at `Provisioning` and marks it `Error` (`ProvisioningTimedOut`) after
five minutes; see [Known gaps](#known-gaps).

---

## Debugging

### The fast loop

`openshell-sandbox capability-probe` runs the whole qualification gate on its
own and prints a JSON report. The template's golden warm-up runs it once; the
report is in the worker pod's log.

```sh
export SANDBOX_BAKED_IMAGE=...   # step 5
harness/scripts/render.sh harness/manifests/capability-probe-template.yaml.tmpl \
  | kubectl-ate create actor-template -f -
```

A passing run reports `"qualified":true` with `landlock_abi: 7`,
`seccomp_notification: true`, `socket_virtualization: true`,
`dns_relay_bind: true`.

### The gates

`run_boundary` calls `qualify_runtime()` unconditionally; a failed gate ends
the sandbox.

| # | Gate | What it needs |
|---|---|---|
| 1 | non-root UID **and** GID | `cf699047`, and an image that declares `USER` |
| 2 | all five capability sets empty | `capabilities.drop: ["ALL"]` |
| 3 | `no_new_privs == 1` | `4fc5d550` |
| 4 | same-UID task-memory probe | nothing; stock kata passes |
| 5 | Landlock allow/deny | a writable `/tmp` |
| 6 | seccomp notification | nothing; stock kata passes |
| 7 | socket virtualization, DNS relay bind, Landlock ABI ≥ 3 | `33397540` |

Gate 5 needs `/tmp` because the probe builds its test tree under
`std::env::temp_dir()`, and the stock sandbox image contains one file — the
binary.

### Where the output is

`kubectl-ate logs actor` returns nothing once an actor has been restored from a
snapshot. The worker pod forwards actor logs:

```sh
kubectl logs -n "${ATESPACE}" <worker-pod> | grep -i openshell
```

`Boundary control listener ready` says the sandbox cleared every gate and is
serving. `Isolation boundary attached` and `PROC:LAUNCH` say the supervisor
reached it and started the workload.

### Retargeting after a rebuild

atelet and ateom are versioned per node. An install creates a DaemonSet named
`atelet-<version>` with a `nodeSelector` on `ate.dev/substrate-version`, and
does **not** move an already-labelled node. A rebuild does nothing until the
node label and every `WorkerPool`'s `nodeSelector` move too; a pool whose
selector does not match sits in `Pending`. A dirty worktree makes
`git describe` yield `<sha>-dirty`, and the label has to match that exactly.

```sh
harness/scripts/retarget-substrate-version.sh <node> <version> [ateom-image]
```

---

## Known gaps

- **Image file ownership is discarded.** Substrate's `unpackLayer` never chowns
  to the layer tar's uid/gid, so everything extracts root-owned; an image
  shipping files owned by its runtime user lands unusable by that user. The
  natural home for a fix is `FinalizeLayer` in privileged ateom. This is what
  forces the baking in step 5.
- **`no_new_privileges` is unconditional**, with no `SecurityContext` field to
  opt out.
- **`SecurityContext` has no `runAsUser`.** A container's identity comes from
  its image's `Config.User` and nowhere else.
- **`Linux.Seccomp`, `Process.ApparmorProfile` and `SelinuxLabel` are not
  forwarded** to the kata agent. `Linux.Sysctl` is, but an ActorTemplate cannot
  set it.
- **A missing or full `WorkerPool` is silent.** The template's golden actor
  waits for a worker with an empty status; `create_sandbox` fails after 180 s
  with `deadline exceeded`.
- **Templates are never garbage-collected.**
- **A gateway can create a sandbox, but not bring it to `Ready`.** The
  synthesized template is one container, with no supervisor and no
  credentials, so no supervisor session can be established. The gateway holds
  `Provisioning`, refuses `StopSandbox` and `StartSandbox` before `Ready`, and
  after five minutes marks the sandbox `Error`. Substrate has no way to hand
  an actor a per-sandbox secret (no per-actor file or env), and the gateway's
  launch credentials are per sandbox, so the two-container shape of
  `examples/helpdesk` cannot be synthesized yet.

## License

Apache-2.0. See [LICENSE](LICENSE).
