# openshell-driver-substrate

An [Agent Substrate](https://github.com/agent-substrate/substrate) compute
driver for [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell), plus the
harness to reproduce a working run.

OpenShell's per-request sandbox becomes a Substrate actor, so it can be
snapshotted and resumed instead of cold-started. Stock OpenShell v0.1.7 runs
on Substrate's **micro-VM** sandbox class from upstream `main`, with no patch
to either project. The workload in [`examples/helpdesk`](examples/helpdesk)
runs inside a real OpenShell sandbox, its egress is mediated by the
supervisor, it is reachable through atenet-router, and its memory survives a
suspend and resume. It passes on arm64 and on amd64.

A stock `openshell-gateway` drives the driver over `--compute-driver-socket`
and creates sandboxes through it, but cannot bring one to `Ready`; see
[Known gaps](#known-gaps).

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
        |  tonic gRPC, substrate.ateapi.v1.Control
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
| `suspend_sandbox` | not advertised; `stop_sandbox` is the suspend |
| `delete_sandbox` | `DeleteActor` |
| `get_sandbox` / `list_sandboxes` | `GetActor` / `ListActors` |
| `watch_sandboxes` | polls `ListActors` every 2s (Substrate has no watch RPC) |
| `ensure_workspace` | `CreateAtespace` |
| `delete_workspace` | no-op: one atespace serves every workspace |

One `ActorTemplate` is reused for every actor whose template comes out
identical, because its name is a hash of the template itself. So only the first
`create_sandbox` for a workload pays for a golden-snapshot build. Nothing that
varies per sandbox goes into a template: a snapshot freezes process memory, so
a per-sandbox value would come back identical in every actor restored from it.
A synthesized template selects its workers by label (`--worker-selector`,
default `workload=openshell-microvm`), and the pool those labels name decides
the sandbox class. It drops every capability, takes memory snapshots, and gets
its resource limits from the sandbox template's `cpu_limit` and `memory_limit`
(Kubernetes quantities) or the driver's defaults.

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
examples/helpdesk/    the demo: a Python agent under OpenShell, ten beats
harness/
  bootstrap-gen/      mints the Ed25519 / JSON Web Token (JWT) / TLS bundle the binaries require
  manifests/          the WorkerPool
  scripts/prep-host.sh  packages, Docker, Go, kind, kubectl and Rust on Ubuntu 24.04
  scripts/kind-up.sh    Substrate on kind, the pool, the atespace and the model endpoint
  scripts/render.sh     renders a .tmpl from the environment
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
`harness/bootstrap-gen` builds `BoundaryConfig`, `SandboxRuntimeDescriptor`
and `SupervisorAuthBundle` by hand, and those types move between releases.
Budget bootstrap-gen work with every bump.

---

## Reproducing a working run

### 0. A host

A Linux host with **`/dev/kvm`** (nested virtualisation if the host is itself
a VM), Ubuntu 24.04 on amd64 or arm64. Each micro-VM worker takes 2 GiB; the
pool below makes two. 16 GB of RAM is the floor, 32 GB is comfortable, and the
clone, the builds and the images want about 30 GB of free disk. Everything
cloned or pulled is public; no credentials are needed.

```sh
sudo harness/scripts/prep-host.sh      # packages, Docker, Go, kind, kubectl, Rust; idempotent
```

A host firewall that drops forwarded traffic leaves the kind node with no
egress, and the install then looks like a slow registry for twenty minutes.
`kind-up.sh` checks the node's egress before installing anything, and runs
`POST_KIND_HOOK` after the cluster exists so a site script can open the kind
bridge. `ufw` in its default configuration is fine.

### 1. Substrate on kind

```sh
harness/scripts/kind-up.sh
export PATH=$HOME/openshell-substrate/bin:$PATH   # kubectl-ate, ahead of any older copy
```

One command, about eight minutes: it clones Substrate at `SUBSTRATE_REF`
(default `upstream/main`) under `WORK` (default `$HOME/openshell-substrate`),
builds `kubectl-ate`, creates the kind cluster and its registry at
`localhost:5001`, installs `ate-system` with the micro-VM class
(`ate-setup --kind deploy ate-system --sandbox-classes=microvm`), creates the
pool of two micro-VM workers and the atespace `ate-openshell-microvm`, and
deploys Ollama with `qwen2.5:1.5b` in that atespace as the model endpoint.
Steps already done are skipped. Knobs: `ATESPACE`, `MODEL_NAME`, `MODEL_PORT`.

Substrate needs no patch: everything the demo once carried is in `main`
([`docs/upstream-branches.md`](docs/upstream-branches.md)). One gap stays:
micro-VM containers run without `no_new_privileges`, and `SecurityContext` has
no field for it, so the helpdesk image sets it itself (step 2).

### 2. Build the example

```sh
REGISTRY=localhost:5001 examples/helpdesk/build.sh
```

This mirrors the release's `sandbox` and `supervisor` images from
`ghcr.io/nvidia/openshell` into the registry (this platform's manifest), mints
the credentials, builds the two helpdesk images and writes their digests to
`out/helpdesk.env`. Templates reference images by digest; a bare tag fails
atelet's pull cache, and Docker's own `RepoDigests` name the upstream index
for a mirrored tag, so the digests come from the registry.

### 3. Run it

```sh
examples/helpdesk/run.sh        # ten beats, about a minute
examples/helpdesk/soak.sh 10    # the same, ten times in a row
```

[`examples/helpdesk`](examples/helpdesk) explains the beats and what each one
proves. The two agents are reached with `kubectl-ate proxy actor`, which holds
the client certificate atenet-router requires.

### 4. Drive it from a gateway

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
  --atespace ate-openshell-microvm &

openshell-gateway --compute-driver substrate \
  --compute-driver-socket /tmp/substrate.sock --disable-tls
```

`ate-api-server` needs TLS with the `servicedns` trust bundle, server name
`api.ate-system.svc`, and a bearer token whose audience is that same name. It
does not require a client certificate.

Create a sandbox with `grpcurl` (the gateway serves reflection):

```sh
grpcurl -plaintext -d '{
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
`"qualified":true`.

| # | Gate | What it needs |
|---|---|---|
| 1 | non-root UID **and** GID | an image `USER`, which Substrate honors, or `sandbox-entry.sh` dropping to 65532 |
| 2 | all five capability sets empty | `capabilities.drop: ["ALL"]`, and `setpriv --bounding-set=-all` after the start-up work |
| 3 | `no_new_privs == 1` | `sandbox-entry.sh`: `setpriv --no-new-privs`; Substrate has no field for it |
| 4 | same-UID task-memory probe | nothing; stock kata passes |
| 5 | Landlock allow/deny | a writable `/tmp`; Substrate mounts a tmpfs there |
| 6 | seccomp notification | nothing; stock kata passes |
| 7 | socket virtualization, DNS relay bind on `127.0.0.53:53`, loopback confinement, Landlock ABI ≥ 3 | nothing; Substrate lifts the guest's privileged-port limit |

### The boundary channel

OpenShell refuses a boundary listener on a loopback address and a supervisor
peer whose address is the sandbox's own. The sandbox and supervisor containers
share the actor's network namespace, so the channel runs the other way round:
the supervisor listens on the actor's address, `169.254.17.2:17777`, the same
in every Substrate actor, and the sandbox dials it over TLS
(`RuntimeInitiatedTlsTcp`). `sandbox-entry.sh` waits for that listener before
starting the sandbox. In the worker pod log, `Runtime-initiated Sandbox
Protocol listener ready` is the supervisor and `Isolation boundary attached`
is the pair.

### DNS

The sandbox answers the workload's DNS at `127.0.0.53` and its broker refuses
any other nameserver. Substrate writes the actor gateway into `/etc/resolv.conf`
in the rootfs base layer and keeps the actor's own edits, so `sandbox-entry.sh`
writes `nameserver 127.0.0.53` on the first boot and the golden snapshot
carries it. The supervisor's policy DNS then resolves only the hosts the
policy names; a refused name shows as `NET:REFUSE ... policy_dns_ineligible`.

### Two transient conditions

`kubectl-ate delete actor-template` returns `Aborted: another operation is in
progress` while that template's golden warm-up runs. Retry after it finishes.

For a template without a wakeup probe on every container, which is every
template in this repo, the warm-up snapshots at a fixed 20 seconds after the
actor starts, whether or not the containers are ready. A supervisor that has
not attached by then is captured in that state and stays that way in every
restore.

### Where the output is

The worker pod forwards actor logs, each line labelled with the actor and
container; `kubectl-ate logs actor <name> -a <atespace>` filters them, and
`run.sh` prints the matching lines after each beat:

```sh
kubectl logs -n "${ATESPACE}" <worker-pod> | grep -i openshell
```

`Isolation boundary attached` and `PROC:LAUNCH` say the supervisor reached the
sandbox and started the workload.

---

## Known gaps

- **`no_new_privileges` has no `SecurityContext` field.** Micro-VM containers
  run without it, so the sandbox image sets it itself. A change adding
  `allowPrivilegeEscalation` (Kubernetes shape) is on the `dims/substrate`
  fork, branch `allow-privilege-escalation`, pending a pull request.
- **`Linux.Seccomp`, `Process.ApparmorProfile` and `SelinuxLabel` are not
  forwarded** to the kata agent. `Linux.Sysctl` is, but an ActorTemplate cannot
  set it.
- **A missing or full `WorkerPool` is silent.** The template's golden actor
  waits for a worker with an empty status; `create_sandbox` fails after 180 s
  with `deadline exceeded`.
- **Templates are never garbage-collected.**
- **The driver creates no `EgressPolicy`.** Substrate denies an actor's egress
  until one exists, so a gateway-created sandbox cannot reach anything.
- **A gateway can create a sandbox, but not bring it to `Ready`.** The
  synthesized template is one container, with no supervisor and no
  credentials, so no supervisor session can be established. The gateway holds
  `Provisioning`, refuses `StopSandbox` and `StartSandbox` before `Ready`, and
  after five minutes marks the sandbox `Error`. Substrate has no way to hand
  an actor a per-sandbox secret, and the gateway's launch credentials are per
  sandbox, so the two-container shape of `examples/helpdesk` cannot be
  synthesized yet. OpenShell's isolation-backend API (RFC 0012) is the route.

## License

Apache-2.0. See [LICENSE](LICENSE).
