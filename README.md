# openshell-driver-substrate

An [Agent Substrate](https://github.com/agent-substrate/substrate) compute
driver for [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell), plus the
harness to reproduce a working run.

OpenShell's per-request sandbox becomes a Substrate actor, so it can be
snapshotted and resumed instead of cold-started. Stock OpenShell v0.1.7 runs
on Substrate's **micro-VM** sandbox class from upstream `main`, with no patch
to either project. A stock `openshell-gateway` drives the driver over its
socket: `openshell sandbox create` reaches `Ready` in a few seconds, and
`exec`, `logs`, `suspend`, `resume`, `stop`, `start` and `delete` work. The
workload in [`examples/helpdesk`](examples/helpdesk) runs inside a real
OpenShell sandbox, its egress is mediated by the supervisor, it is reachable
through atenet-router, and its memory survives a Substrate suspend and
resume. Both pass on arm64 and on amd64.

A resume through the gateway restarts the workload; only a Substrate-side
suspend keeps its memory. See [Known gaps](#known-gaps).

---

## How it fits together

[`docs/architecture.md`](docs/architecture.md) has the full picture: every
component, the lifecycle, and sequence diagrams for create, the request
path, suspend and resume, host death, and how a sandbox gets its credentials.

```
openshell CLI -> openshell-gateway  (stock, unmodified binary)
                       |
                       |  ComputeDriver over a Unix socket
                       v
             openshell-driver-substrate   (this repo; its own process)
                 |                 ^
                 |  Control gRPC   |  GET /v1/launch/<atespace>/<name>
                 v                 |  (actor JWT put on by atenet-egress)
        Substrate ate-api-server   |
                 |                 |
                 v                 |
ActorTemplate -> golden snapshot -> Actor (micro-VM): launcher -> supervisor + sandbox -> workload
```

The driver implements OpenShell's `ComputeDriver` gRPC trait and maps it onto
Substrate's actor lifecycle:

| OpenShell call | What the driver does |
|---|---|
| `create_sandbox` | `CreateActorTemplate` (reused by content hash), `CreateActor`, `CreateActorEgressPolicy`, store the bundle, `ResumeActor` |
| `start_sandbox` | store the bundle of the new generation, `ResumeActor` |
| `stop_sandbox`, `suspend_sandbox` | `SuspendActor` with synchronous durability; `RevertActor` for a `CRASHED` actor |
| `delete_sandbox` | `DeleteActor`, drop the bundle |
| `get_sandbox` / `list_sandboxes` | `GetActor` / `ListActors`, the actors of `oshl-` templates |
| `watch_sandboxes` | polls `ListActors` every 2 s (Substrate has no watch RPC) |
| `ensure_workspace` | `GetAtespace`, then `CreateAtespace` when it is missing |
| `delete_workspace` | no-op: one atespace serves every workspace |

One `ActorTemplate` is reused for every actor whose template comes out
identical, because its name is a hash of the template itself. So only the first
`create_sandbox` for a workload pays for a golden-snapshot build. Nothing that
varies per sandbox goes into a template: a snapshot freezes process memory, so
a per-sandbox value would come back identical in every actor restored from it.
A synthesized template selects its workers by label (`--worker-selector`,
default `workload=openshell-microvm`), and the pool those labels name decides
the sandbox class. Its shape comes from the configuration file (`--config`,
TOML): an ActorTemplate fragment in protojson YAML gives the containers,
volumes, resources and snapshot settings every sandbox gets, and the
request's image goes into the container named `sandbox`; an EgressPolicy
file gives the rules every actor gets before its first resume. Without a
fragment the template is one container. Every container drops all
capabilities, the template takes memory snapshots, and its limits come from
the sandbox template's `cpu_limit` and `memory_limit` (Kubernetes
quantities), then the fragment, then the driver's defaults.

A sandbox gets its credentials after the restore, not from the snapshot. The
gateway mints a launch bundle per sandbox and per start; the driver keeps it
on disk and serves it on `GET /v1/launch/<atespace>/<name>` (the `[launch]`
section of the configuration). Inside the actor a launcher reads its own name
from a `systemInfo` volume, which Substrate rewrites on every restore, and
polls that URL. An EgressPolicy rule makes atenet-egress replace the
`Authorization` header with a Substrate-minted actor JWT, so the driver knows
which actor asks and answers only with that actor's bundle; it verifies the
token against the IdP's JWKS. The launcher writes the supervisor's and the
sandbox's files, starts `openshell-supervisor` against the gateway (the
`[gateway]` section), then `openshell-sandbox`, and the supervisor's session
takes the sandbox to `Ready`. A new generation from `start` or `resume`
replaces both processes.

The driver registers out of process over a Unix socket, as OpenShell's in-tree
`openshell-driver-podman` / `-docker` / `-kubernetes` / `-vm` binaries do. A
stock gateway picks it up with
`--compute-driver substrate --compute-driver-socket <path>`, or
`compute_driver` and `[openshell.drivers.substrate] socket_path` in its
configuration. No OpenShell fork is involved; `openshell-core` is a plain git
dependency pinned by rev.

`src/`: `lib.rs` (the trait impl), `template.rs` (ActorTemplate synthesis),
`configfile.rs` and `fragment.rs` (the TOML file and its protojson YAML,
decoded with prost-reflect), `bundle.rs` (a bundle from the gateway's launch
authentication), `launch.rs` (the bundle store and the launch endpoint),
`main.rs` (the servers).

## Layout

```
src/                  the driver
proto/                ateapi.proto, used by build.rs
tests/live.rs         full lifecycle against a real cluster
docs/                 architecture
examples/helpdesk/    the demo: a Python agent under OpenShell; the launcher and the templates
harness/
  bootstrap-gen/      mints the Ed25519 / JSON Web Token (JWT) / TLS bundle for the demo without a gateway
  gateway/            the gateway and driver pod: Deployment, gateway.toml, driver.toml, access policy
  manifests/          the WorkerPool
  scripts/prep-host.sh  packages, Docker, Go, kind, kubectl and Rust on Ubuntu 24.04
  scripts/kind-up.sh    Substrate on kind, the pool, the atespace and the model endpoint
  scripts/gateway-up.sh the stock gateway and this driver in the cluster, the openshell CLI
  scripts/driver-up.sh  the driver alone on this host, for grpcurl
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
`ate-api-server` and is ignored by default; its header lists the variables,
and `SUBSTRATE_LIVE_CONFIG` points it at a configuration file.

`proto/ateapi.proto` is a verbatim copy of Substrate's
`api/ateapi/v1/ateapi.proto` at the commit in `proto/upstream-rev`; CI diffs
the two. To move the pin, which usually means driver changes in the same
commit:

```sh
echo <substrate commit> > proto/upstream-rev
curl -fsSL "https://raw.githubusercontent.com/agent-substrate/substrate/$(cat proto/upstream-rev)/api/ateapi/v1/ateapi.proto" -o proto/ateapi.proto
```

Bumping the OpenShell pin in `Cargo.toml` is not a one-line change:
`src/bundle.rs` and `harness/bootstrap-gen` build `BoundaryConfig`,
`SandboxRuntimeDescriptor` and `SupervisorAuthBundle` by hand, and those
types move between releases. Budget that work with every bump.

---

## Reproducing a working run

### 0. A host

A Linux host with **`/dev/kvm`** (nested virtualization if the host is itself
a VM), Ubuntu 24.04 on amd64 or arm64. Each micro-VM worker takes 2 GiB; the
pool below makes two. 16 GB of RAM is the floor, 32 GB is comfortable, and the
clone, the builds and the images want about 30 GB of free disk. Everything
cloned or pulled is public; no credentials are needed. Run every step as the
same user; the walkthrough below assumes root, which is where `prep-host.sh`
installs Rust.

```sh
harness/scripts/prep-host.sh      # packages, Docker, Go, kind, kubectl, Rust; idempotent
```

A host firewall that drops forwarded traffic leaves the kind node with no
egress, and the install then looks like a slow registry. `kind-up.sh` checks
the node's egress before installing anything, and runs `POST_KIND_HOOK` after
the cluster exists so a site script can open the kind bridge. `ufw` in its
default configuration is fine.

### 1. Substrate on kind

```sh
harness/scripts/kind-up.sh
export PATH=${WORK:-$HOME/openshell-substrate}/bin:$PATH   # kubectl-ate, ahead of any older copy
```

One command: it clones Substrate at `SUBSTRATE_REF` (default `upstream/main`)
under `WORK` (default `$HOME/openshell-substrate`, which holds the clone,
`bin/` and `logs/`), builds `kubectl-ate`, creates the kind cluster and its
registry at `localhost:5001`, installs `ate-system` with the micro-VM class
(`ate-setup --kind deploy ate-system --sandbox-classes=microvm`, with the
bundled credential provider and `atenet-client` on the router's ingress
allowlist), creates the pool of two micro-VM workers and the atespace
`ate-openshell-microvm`, and deploys Ollama with `qwen2.5:1.5b` in that
atespace as the model endpoint. Steps already done are skipped. The install
log is `$WORK/logs/install.log`. Knobs: `WORK`, `SUBSTRATE_REF`, `ATESPACE`,
`MODEL_NAME`, `MODEL_PORT`, `POST_KIND_HOOK`.

Substrate, OpenShell and kata run unmodified. One gap stays: micro-VM
containers run without `no_new_privileges`, and `SecurityContext` has no field
for it, so the helpdesk launcher sets it itself (step 2).

### 2. Build the example

```sh
REGISTRY=localhost:5001 examples/helpdesk/build.sh
```

This mirrors the release's `sandbox` and `supervisor` images from
`ghcr.io/nvidia/openshell` into the registry, mints the credentials for the
run without a gateway, builds the helpdesk image (both OpenShell binaries,
python, the agent, the launcher) and the supervisor's files image, and writes
their digests to `out/helpdesk.env`. Only this platform's manifest is
mirrored, so build on the architecture the cluster runs. Substrate's API requires image digests, and Docker's own
`RepoDigests` name the upstream index for a mirrored tag, so the digests come
from the registry. Knobs: `REGISTRY` (required), `ATESPACE`, `MODEL_HOST`,
`MODEL_PORT`, `MODEL_NAME`, `OPENSHELL_VERSION`.

### 3. Run it

```sh
examples/helpdesk/run.sh        # ten beats, about a minute
examples/helpdesk/soak.sh 10    # the same, ten times in a row
```

[`examples/helpdesk`](examples/helpdesk) explains the beats and what each one
proves. The two agents are reached with `kubectl-ate proxy actor`, which holds
the client certificate atenet-router requires. `soak.sh` keeps one log per
run under `SOAK_LOGS` (default `$HOME/openshell-substrate/logs/soak-<time>`).

To tear everything down:

```sh
kind delete cluster && docker rm -f kind-registry && rm -rf "${WORK:-$HOME/openshell-substrate}" out
```

### 4. Drive it from a stock gateway

```sh
harness/scripts/gateway-up.sh
```

This puts the release's `openshell-gateway` image and this driver in one pod
in the atespace, as OpenShell's own e2e runs an external driver: the driver
serves `ComputeDriver` on a socket in a shared `emptyDir` and its launch
endpoint on a port, with two Services, `openshell-gateway:17670` and
`openshell-driver:17790`. It builds the driver image from this checkout,
makes the gateway's TLS pair and Ed25519 JWT keys, renders
`harness/gateway/*.tmpl` and the helpdesk's gateway-mode fragment and egress
policy into ConfigMaps, labels the namespace for API access, gives the
driver's ServiceAccount the `editor` role on the atespace, and installs the
`openshell` CLI under `$WORK/bin`. Rerun it after `build.sh` or a driver
change: the pod restarts, its database and bundles are on volumes, and the
supervisors reconnect. Knobs: `WORK`, `REGISTRY`, `ATESPACE`,
`OPENSHELL_VERSION`; the model host and port come from `out/helpdesk.env`.

Then, with a port-forward to the gateway:

```sh
kubectl -n ate-openshell-microvm port-forward svc/openshell-gateway 17670:17670 &
export OPENSHELL_GATEWAY_ENDPOINT=https://127.0.0.1:17670 OPENSHELL_GATEWAY_INSECURE=1
. out/helpdesk.env
MODEL_HOST=$MODEL_HOST MODEL_PORT=$MODEL_PORT harness/scripts/render.sh examples/helpdesk/data.yaml.tmpl > policy.yaml
openshell sandbox create --name alice --from "$SANDBOX_IMAGE" --policy policy.yaml --detach \
  --env "OPENAI_BASE_URL=http://$MODEL_HOST:$MODEL_PORT/v1" --env HELPDESK_MODEL=qwen2.5:1.5b \
  -- python3 /opt/helpdesk/agent.py
openshell sandbox get alice            # Ready in about 2 s; about 30 s more the first time, for the golden snapshot
openshell sandbox exec alice -- cat /etc/resolv.conf
openshell logs alice
openshell sandbox suspend alice        # the actor is checkpointed and its worker freed
openshell sandbox resume alice         # restored, new credentials, workload restarted
openshell sandbox stop alice && openshell sandbox start alice
openshell sandbox delete alice
```

The agent is reachable the demo's way too, with `kubectl-ate proxy actor`
on the sandbox id (`openshell sandbox get alice -o json | jq -r .id`). The
policy file matters: without one, the gateway's default Landlock rules do not
cover `/opt/helpdesk` and the workload exits with `Permission denied`. The
image must run as uid and gid 65532: the bundle names that identity and
`openshell-sandbox` checks it.

To run the driver on this host instead, against a port-forward to
`ate-api-server` with a 24 h token, and talk to it with `grpcurl` over the
socket (it serves reflection):

```sh
harness/scripts/driver-up.sh
grpcurl -plaintext -unix "$HOME/openshell-substrate/driver/driver.sock" list
```

`ate-api-server` needs TLS with the `servicedns` trust bundle, server name
`api.ate-system.svc`, and a bearer token whose audience is that same name. It
does not require a client certificate.

---

## Debugging

### The gates

`openshell-sandbox` runs `qualify_runtime()` before anything else; a failed
gate ends the sandbox, and the worker pod log names it.

| # | Gate | What it needs |
|---|---|---|
| 1 | non-root UID **and** GID | the image's `USER 65532:65532`, which Substrate honors |
| 2 | all five capability sets empty | `capabilities.drop: ["ALL"]`; a non-root process holds none |
| 3 | `no_new_privs == 1` | `launcher.py` runs `setpriv --no-new-privs`, which needs no privilege; Substrate has no field for it |
| 4 | same-UID task-memory probe | nothing; stock kata passes |
| 5 | Landlock allow/deny, ABI ≥ 3 | a writable `/tmp`; Substrate mounts a tmpfs there, and the guest kernel has Landlock |
| 6 | seccomp notification | nothing; stock kata passes |
| 7 | socket virtualization, DNS relay bind on `127.0.0.53:53`, loopback confinement | nothing; Substrate lifts the guest's privileged-port limit |

### The boundary channel

OpenShell refuses a boundary listener on a loopback address and a supervisor
peer whose address is the sandbox's own. The sandbox and supervisor containers
share the actor's network namespace, so the channel runs the other way round:
the supervisor listens on the actor's address, `169.254.17.2:17777`, the same
in every Substrate actor, and the sandbox dials it over TLS
(`RuntimeInitiatedTlsTcp`). The launcher waits for that listener before
starting the sandbox. In the worker pod log, `Runtime-initiated Sandbox
Protocol listener ready` is the supervisor and `Isolation boundary attached`
is the pair.

### The launch endpoint

The launcher's lines in the worker pod log say what it waits for: `launch
endpoint unreachable` while the egress has no policy yet or the Service is
not up, `launch endpoint answered 401` or `403` when the driver refused the
actor JWT (the driver's log says why), and `generation <id> (epoch N)
published` when the files are in place. The driver logs `bundle
handed out` for each delivery. A supervisor that dies logs its reason just
before `the supervisor exited`; the SSH relay's socket, for example, must sit
on the container's own filesystem, because a durable dir takes no Unix
socket. After `the supervisor exited` or `openshell-sandbox exited` the
container is gone, nothing restarts it, and the actor stays `RUNNING` around
it; delete the sandbox.

### DNS

The sandbox answers the workload's DNS at `127.0.0.53` and its broker refuses
any other nameserver. Substrate writes the actor gateway into `/etc/resolv.conf`
in the rootfs base layer and keeps the actor's own edits, so the launcher
replaces the file with `nameserver 127.0.0.53` on the first boot and the golden
snapshot carries it; the image's `/etc` belongs to the sandbox user so no root
is needed. The supervisor keeps its own container, because its policy DNS
resolves upstream names through the container's resolver. The supervisor's policy DNS then resolves only the hosts the policy
names; a refused name shows as `NET:REFUSE ... policy_dns_ineligible`.

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

- **`no_new_privileges` has no `SecurityContext` field.** Neither runtime sets
  it, by upstream's choice (parity with a Kubernetes container that allows
  privilege escalation), so the launcher sets it with an unprivileged
  `setpriv --no-new-privs`.
- **`Linux.Seccomp`, `Process.ApparmorProfile` and `SelinuxLabel` are not
  forwarded** to the kata agent. `Linux.Sysctl` is, but an ActorTemplate cannot
  set it.
- **A missing or full `WorkerPool` is silent.** The template's golden actor
  waits for a worker with an empty status; `create_sandbox` fails after 180 s
  with `deadline exceeded`.
- **Templates are never garbage-collected.**
- **A resume through the gateway restarts the workload.** The gateway mints a
  new generation for every `start` and `resume`, and in OpenShell v0.1.7 a
  dedicated supervisor has no way onto a running `openshell-sandbox`: the
  sandbox holds its TLS identity and session id from its first bootstrap in
  memory, the new tokens name the new generation, and only the shared-pool
  takeover (not available to drivers) advances a session. `openshell-sandbox`
  also answers `SuspendBoundary` with `Unimplemented`. So the launcher
  restarts the supervisor and the sandbox for every new generation. The
  actor itself is restored, not booted, and a Substrate-side suspend and
  resume keeps the workload's memory, as the demo shows.
- **A Substrate-side suspend of a gateway sandbox shows as `Provisioning`.**
  The driver reports it as `Ready=False (DependenciesNotReady)`, so the
  gateway keeps the sandbox's credentials and admits only `delete` until
  Substrate resumes the actor (`kubectl-ate resume`, or whatever suspended
  it); then the supervisor reconnects and the sandbox is `Ready` again with
  its memory. Reported as a stop, the gateway would retire the credentials
  and need a new generation.
- **The old supervisor keeps retrying with retired credentials** between a
  stop and the next start; the gateway logs a denied logon per attempt until
  the launcher replaces it.
- **Without the launch endpoint** (`driver-up.sh`, no `[launch]` section) a
  sandbox's name is its id, `spec.command` is rejected, and a sandbox runs
  with whatever credentials its image carries.
- **The launch hop is plain HTTP inside the cluster.** The bundle travels
  from atenet-egress to the driver's Service unencrypted; an `https` rule
  with the trust-bundle `systemInfo` source would close that on a shared
  network.
- **GPU requests and `user_namespaces` are rejected.** The driver also does
  not retry Substrate's `Aborted` (a concurrent update), and it acknowledges
  only the default resource-admission policy.

## License

Apache-2.0. See [LICENSE](LICENSE).
