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

Micro-VM needs nested virtualisation (`/dev/kvm`). Six commits in Substrate
are required, each necessary; a seventh matters on gVisor only. They are open
as four PRs on `agent-substrate/substrate`, none merged: [#1904](https://github.com/agent-substrate/substrate/pull/1904), [#1906](https://github.com/agent-substrate/substrate/pull/1906),
[#1910](https://github.com/agent-substrate/substrate/pull/1910) and [#1918](https://github.com/agent-substrate/substrate/pull/1918). [`docs/upstream-branches.md`](docs/upstream-branches.md)
lists the commits and what changes when they merge. The guest kernel is stock
kata.

A second variant needs no Substrate patch at all: on this repo's [`lean-zero`](https://github.com/dims/openshell-driver-substrate/tree/lean-zero) branch
the sandbox image drops root itself, with a short entry script and four
start-up capabilities, and Substrate is plain upstream main from [`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c). It
passes the same ten beats. [`main`](https://github.com/dims/openshell-driver-substrate/tree/main) keeps the stock image because Substrate
should do that work; the PRs are the fix.

---

## How it fits together

[`docs/architecture.md`](docs/architecture.md) has the full picture: every
component, the lifecycle, and sequence diagrams for create, request path,
suspend and resume, and host death.

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

One `ActorTemplate` is reused for every actor whose template comes out
identical, because its name is a hash of the template itself. So only the first
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
docs/                 architecture and the upstream branch index
examples/helpdesk/    the demo: a Python agent under OpenShell, ten beats; docs/ tells why
harness/
  bootstrap-gen/      mints the Ed25519 / JSON Web Token (JWT) / TLS bundle the binaries require
  manifests/          the WorkerPool
  scripts/render.sh   renders a .tmpl from the environment
```

## Build and test

```sh
cargo build --workspace --release
cargo test --lib          # no cluster needed
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Needs rustc ≥ 1.94 (OpenShell's floor). `tests/live.rs` needs a reachable
`ate-api-server` and is ignored by default.

Bumping the OpenShell pin in `Cargo.toml` is not a one-line change:
`harness/bootstrap-gen` builds `BoundaryConfig` and `SandboxRuntimeDescriptor`
by hand, and those types and OpenShell's `proto/openshell.proto` move between
releases.
Budget bootstrap-gen work with every bump.

---

## Reproducing a working run

### 0. Prerequisites

A Linux host with **`/dev/kvm`** (nested virtualisation if the host is itself
a VM):

```sh
ls /dev/kvm && grep -oE 'vmx|svm' /proc/cpuinfo | head -1
```

Each micro-VM worker takes 2 GiB; the pool below makes two. 16 GB of RAM is
the floor, 32 GB is comfortable, and the builds and images want about 50 GB
of free disk. Everything cloned or pulled is public; no credentials are needed.
From nothing cached, the whole walkthrough takes about ten minutes on 16 cores
with a fast connection; the two OpenShell release builds are the long pole at
about a minute and a half each. `mise` may hit GitHub's anonymous API rate
limit while installing OpenShell's toolchain; `export GITHUB_TOKEN=...` avoids
it.

Then:

```sh
sudo apt-get install -y build-essential protobuf-compiler pkg-config \
                       libssl-dev jq gettext-base
curl -fsSL https://sh.rustup.rs | sh -s -- -y
curl -fsSL https://mise.run | sh                           # OpenShell's toolchain (step 3)
export GITHUB_TOKEN=...                                    # mise: 60 anonymous API calls/hour otherwise
go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest   # steps 7 and 8
# go >= 1.27 (Substrate's go.mod; an older go with GOTOOLCHAIN=auto downloads it), docker, kubectl, kind from their upstream installers
```

Use a current `kind`; an old one fails against the node image with `unknown
containerd config version: 4`.

A host with a managed firewall can leave the kind node with no egress at all.
On a host provisioned by NVIDIA Base Command Manager (BCM), nftables `cm_filter` has a forward policy of DROP
and `cmd` re-applies it, so disabling the firewall does not hold; the fix is to
add the kind bridge to the template's `@nat_ifaces` set, and the input chain
needs an allowance for anything on the host the node must reach, such as the
model endpoint in step 7. The symptom is an install that looks like a slow
registry for twenty minutes. The pre-flight in step 1 catches it.

`ko` does **not** need installing separately. Substrate vendors it, and every
build must go through its wrapper, `./hack/run-tool.sh ko ...`.

### 1. Patch Substrate and bring up a cluster

```sh
git clone https://github.com/dims/substrate && cd substrate
git checkout 9d6020f5                  # lean-integration; see docs/upstream-branches.md
export GOFLAGS=-buildvcs=false
./hack/create-kind-cluster.sh
docker run --rm --network kind alpine wget -q -O /dev/null --timeout=5 \
  http://detectportal.firefox.com/success.txt && echo egress ok   # from the kind bridge
./hack/install-ate-kind.sh --deploy-ate-system
kubectl -n ate-system wait --for=condition=Available deploy --all --timeout=10m
kubectl -n ate-system rollout status ds --timeout=10m
kubectl -n ate-system rollout status sts --timeout=10m
make build-atectl && export PATH=$PWD/bin:$PATH   # kubectl-ate, ahead of any older copy
```

[`9d6020f5`](https://github.com/dims/substrate/commit/9d6020f51b47968c3d0f6eb27747f4cb4c7682c7) is the head of
[`lean-integration`](https://github.com/dims/substrate/tree/lean-integration); [`docs/upstream-branches.md`](docs/upstream-branches.md)
links each commit on it. `create-kind-cluster.sh` also starts a local image
registry at `localhost:5001`; that is `<registry>` in every step below. The installer's
own readiness wait is 60 s per workload (`--rollout-timeout`), so keep the
three `kubectl` waits above after it returns.

The install labels the node with the build version. After a Substrate
rebuild, move that label and every pool's `nodeSelector` to the new
`git describe` output, or recreate the cluster.

Without any one of these commits the demo fails, and `run.sh` says so at
beat 1 or 2:

- without `USER`, the sandbox runs as root and never clears its gates;
- without the durable-dir mode, a non-root sandbox cannot write `/tmp`;
- without `CAP_DAC_OVERRIDE`, the golden snapshot never gets its tag: atelet
  cannot reset directories the sandbox wrote;
- without sysctl forwarding, the low-port sysctl, or `no_new_privs`, the
  sandbox never reports `Boundary control listener ready`.

### 2. Install the micro-VM backend

From the Substrate repo:

```sh
ARCH=amd64 ATE_INSTALL_KIND=true hack/install-microvm-deps.sh --install
```

This downloads the kata asset set, stages it to the cluster object store, and
applies the cluster-wide `microvm` SandboxConfig. `ATE_INSTALL_KIND=true`
picks the in-cluster rustfs bucket; without it the script takes the Google
Kubernetes Engine (GKE) path and fails on `gcloud: command not found`.

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
Always push: a template needs the registry's digest, and `kind load
docker-image` does not produce one.

### 4. Mint the credentials

`examples/helpdesk/build.sh` does steps 4 and 5. The tokens last one hour
(below), so do steps 4, 5, 7 and 8 in one sitting, after 1 to 3 and 6.

An Ed25519-signed JWT pair and TLS material bound to one session id:

```sh
mkdir -p out
openssl genpkey -algorithm ed25519 -out out/signing.key.pem
openssl pkey -in out/signing.key.pem -pubout -out out/signing.pub.pem
cargo run -p bootstrap-gen -- out/
```

That writes `bootstrap.json`, `server.crt`, `server.key` (the sandbox's side)
and `runtime-descriptor.json`, `auth.json` (the supervisor's).

The tokens are valid for **one hour**, OpenShell's maximum for a session
token. A gateway refreshes a sandbox's tokens; this hand-applied harness has
nothing to refresh from, so run the actor within the hour. After that the
supervisor logs `Starting sandbox supervision` and then, about 90 seconds
later, `boundary unavailable ... timed out while waiting for remote boundary
boot`, with no mention of authentication. Run `build.sh` again.

Any keypair is accepted as a trust anchor: `SandboxLaunchAuthentication`
validates against its own embedded key, and nothing ties that key to a
gateway.

### 5. Bake the credentials

The sandbox **consumes** its bootstrap, unlinking the file after reading it,
so a read-only image volume fails with `Read-only file system`. `build.sh`
tars `bootstrap.json`, `server.crt` and `server.key` into the helpdesk image's
rootfs instead. Substrate discards image file ownership, so `--chown` has no
effect and the tree is world-writable (see [Known gaps](#known-gaps)). The
image's `CMD` names the bootstrap, so the gateway path, which sends no
command, finds it too.

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

[`examples/helpdesk`](examples/helpdesk) is two commands. `build.sh` mints
the credentials and builds the images. `run.sh` plays ten beats: two agents
from one snapshot, an egress allow-list, a suspend and resume with the memory
intact, a dead host, a revert, a delete. After each beat it prints what
Substrate and OpenShell logged, so the mechanics are on screen.

**Actor state is not proof the containers are alive.** A micro-VM snapshot is
whole-VM memory, so a dead container never surfaces as a restore failure. Read
the worker pod log.

### 8. Drive it from a gateway

A stock `openshell-gateway` dispatches to this driver over a Unix socket. With
`--compute-driver-socket`, the gateway binds the name given to
`--compute-driver` to that socket, whether or not a built-in driver has the
same name.

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

`ate-api-server` needs TLS with the `servicedns` trust bundle, server name
`api.ate-system.svc`, and a bearer token whose audience is that same name. It
does not require a client certificate.

Create a sandbox with `grpcurl` (the gateway serves no reflection):

```sh
grpcurl -plaintext -import-path <openshell>/proto -proto openshell.proto -d '{
  "workspace_scope": {"workspace": "default"},
  "name": "alice",
  "spec": {"log_level": "info",
           "template": {"image": "<SANDBOX_IMAGE from out/helpdesk.env>"},
           "policy": {"version": 1}}
}' 127.0.0.1:17670 openshell.v1.OpenShell/CreateSandbox
```

The driver synthesizes an ActorTemplate, waits for its golden snapshot, then
creates and resumes an actor named by the sandbox id. The gateway holds the
sandbox at `Provisioning` and marks it `Error` (`ProvisioningTimedOut`) after
five minutes; see [Known gaps](#known-gaps).

---

## Debugging

### The gates

`run_boundary` calls `qualify_runtime()` unconditionally; a failed gate ends
the sandbox. The golden warm-up also runs `openshell-sandbox capability-probe`
once; its JSON report is in the worker pod's log, and a passing run says
`"qualified":true` with `landlock_abi: 7`, `seccomp_notification: true`,
`socket_virtualization: true`, `dns_relay_bind: true`.

| # | Gate | What it needs |
|---|---|---|
| 1 | non-root UID **and** GID | [`b5c3fdc1`](https://github.com/dims/substrate/commit/b5c3fdc1e022c759ea993fa630c801a5bdac84cd), and an image that declares `USER` |
| 2 | all five capability sets empty | `capabilities.drop: ["ALL"]` |
| 3 | `no_new_privs == 1` | [`9d6020f5`](https://github.com/dims/substrate/commit/9d6020f51b47968c3d0f6eb27747f4cb4c7682c7) |
| 4 | same-UID task-memory probe | nothing; stock kata passes |
| 5 | Landlock allow/deny | a writable `/tmp` |
| 6 | seccomp notification | nothing; stock kata passes |
| 7 | socket virtualization, DNS relay bind, Landlock ABI ≥ 3 | [`b47e9040`](https://github.com/dims/substrate/commit/b47e9040a6c4fe999c16e1d0957a0a85e5d76b51), [`dc53a35b`](https://github.com/dims/substrate/commit/dc53a35ba72437827198dc7ba921b7ca09f651e2) |

Gate 5 needs `/tmp` because the probe builds its test tree under
`std::env::temp_dir()`, and the stock sandbox image contains one file, the
binary.

### Two transient conditions

`kubectl-ate delete actor-template` returns `Aborted: another operation is in
progress` while that template's golden warm-up runs. Retry after it finishes.

For a template without a wakeup probe on every container, which is every
template in this repo, the warm-up snapshots at a fixed 20 seconds after the
actor starts, whether or not the containers are ready. A supervisor that has
not attached by then is captured in that state and stays that way in every
restore.

### Where the output is

`kubectl-ate logs actor` returns nothing once an actor has been restored from a
snapshot. The worker pod forwards actor logs:

```sh
kubectl logs -n "${ATESPACE}" <worker-pod> | grep -i openshell
```

`Boundary control listener ready` says the sandbox cleared every gate and is
serving. `Isolation boundary attached` and `PROC:LAUNCH` say the supervisor
reached it and started the workload.

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
  its image's `Config.User` and nowhere else, and a named `USER` is rejected
  rather than looked up in the image's passwd file.
- **`Linux.Seccomp`, `Process.ApparmorProfile` and `SelinuxLabel` are not
  forwarded** to the kata agent. `Linux.Sysctl` is, but an ActorTemplate cannot
  set it.
- **A missing or full `WorkerPool` is silent.** The template's golden actor
  waits for a worker with an empty status; `create_sandbox` fails after 180 s
  with `deadline exceeded`.
- **Templates are never garbage-collected.**
- **The driver creates no `EgressPolicy`.** Substrate denies an actor's egress
  until one exists, so a gateway-created sandbox cannot reach anything.
- **OpenShell's supervisor can drop a mediated request** that arrives in the
  same read as the synthesized `CONNECT` header, about once in ten first opens
  after a restore, so the demo stalls about one run in six until
  [NVIDIA/OpenShell#3745](https://github.com/NVIDIA/OpenShell/pull/3745) lands. `run.sh` reports it.
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
