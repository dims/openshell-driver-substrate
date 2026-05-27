# openshell-driver-substrate

Agent Substrate (gVisor + checkpoint/restore via runsc) compute driver
for OpenShell.

**Read first, depending on what you want:**

- [**`docs/poc-intro.md`**](docs/poc-intro.md) — joint POC overview for
  teammates familiar with OpenShell *or* Substrate. Explains what this
  is, why OpenShell is better with Substrate, how the boot path
  degrades safely under gVisor, and the boundary between this crate and
  upstream.
- [**`examples/helpdesk/README.md`**](examples/helpdesk/README.md) —
  the 10-beat driver-driven helpdesk demo. Three acts (provisioning,
  lifecycle, hygiene), every `CreateSandbox`/`ListSandboxes`/`DeleteSandbox`
  flows through `openshell-gateway → openshell-driver-substrate →
  ate-api-server`. Prereqs, quick-start, expected output, troubleshooting.
- [**`examples/gpu-counter/README.md`**](examples/gpu-counter/README.md) —
  sibling of the helpdesk demo for NVIDIA GPU passthrough. The
  openshell-sandbox supervisor execs a Python agent that holds a 1 MiB
  on-device CUDA buffer via libcuda. Proves the new
  `ActorTemplate.containers[*].resources.gpu` CRD field (substrate
  [PR #96](https://github.com/agent-substrate/substrate/pull/96)) round-trips
  through atelet's OCI builder and ateom-gvisor's `runsc --nvproxy`
  invocations end-to-end on an L40S (driver 580.126.09).
- [**`cmd/kubectl-osh/README.md`**](cmd/kubectl-osh/README.md) —
  `kubectl-osh`, an operator-shaped kubectl plugin that talks to the
  gateway. Exposes the substrate-driver-specific
  `substrate_actor_template` annotation (M3.16) which the upstream
  `openshell` CLI can't set today. The helpdesk demo uses it instead of
  raw `grpcurl`.

**Status (2026-05-25):** Driver crate is now load-bearing in a real
OpenShell gateway. The OpenShell-side wiring lives on
[`dims/OpenShell@integration/openshell-driver-substrate`](https://github.com/dims/OpenShell/tree/integration/openshell-driver-substrate)
as a single integration commit
([`753d3e4c`](https://github.com/dims/OpenShell/commit/753d3e4c)) that
adds the `ComputeDriverKind::Substrate` enum entry, the dispatch arm,
the `[openshell.drivers.substrate]` config parser, and a Cargo git-rev
pin to this repo. **This repo's `main` is the authoritative source for
the driver code**; the OpenShell integration branch consumes a pinned
rev. The helpdesk demo above exercises every
`CreateSandbox`/`ListSandboxes`/`DeleteSandbox` through the driver
against a real substrate kind cluster — verified end-to-end on bigbox
2026-05-24 evening and re-verified on a fresh cluster 2026-05-25.

This repository depends on a small change in OpenShell that lets the
supervisor tolerate the bootstrap subsystems gVisor degrades. Two
alternative shapes are filed upstream; one of them will land:

- [`NVIDIA/OpenShell#1548`](https://github.com/NVIDIA/OpenShell/pull/1548)
  `[WIP]` — `OPENSHELL_BEST_EFFORT_FAILURES` env-var gate (3 files,
  +51/-7).
- [`NVIDIA/OpenShell#1549`](https://github.com/NVIDIA/OpenShell/pull/1549)
  — `SandboxFailureHandler` trait + `set_failure_handler` (3 files,
  +71/-7). Programmatic override only — no env var, no CLI flag.

Cargo's `openshell-core` dep is pinned to the corresponding
`dims/OpenShell` fork tip.

## How to use it

The crate is a library — consumers link it from Cargo and wire it into
their compute-runtime dispatcher. The canonical consumer is OpenShell's
`openshell-server`; the wiring lives on
[`dims/OpenShell@integration/openshell-driver-substrate`](https://github.com/dims/OpenShell/tree/integration/openshell-driver-substrate)
as a single integration commit
([`753d3e4c`](https://github.com/dims/OpenShell/commit/753d3e4c)).
For a fresh consumer the three pieces are:

**1. Cargo dep.** Add to `openshell-server/Cargo.toml`. Pin a specific commit so the build is reproducible; bump the rev to pick up new driver work:
```toml
openshell-driver-substrate = { git = "https://github.com/dims/openshell-driver-substrate", rev = "<full-sha-from-main>" }
```
The driver pins `openshell-core` to a specific rev of its own; if your workspace already builds `openshell-core` from a different rev, add a `[patch."https://github.com/dims/OpenShell"]` override at the workspace root pointing at your local copy.

**2. Dispatcher arm.** `SubstrateComputeDriver` implements `ComputeDriver`
directly (same `WatchSandboxesStream` type the gateway expects), so the
constructor mirrors `new_kubernetes` but skips the adapter:
```rust
let driver: SharedComputeDriver =
    Arc::new(SubstrateComputeDriver::new(config));
ComputeRuntime::from_driver(driver, /* … */).await
```

**3. Activate in `gateway.toml`:**
```toml
[openshell.gateway]
compute_drivers = ["substrate"]

[openshell.drivers.substrate]
api_endpoint          = "api.ate-system.svc:443"
api_tls_ca_path       = "/etc/openshell-substrate/ca.crt"
api_bearer_token_path = "/etc/openshell-substrate/token"
default_namespace     = "ate-demo-helpdesk"
default_worker_pool   = "helpdesk-pool"
pause_image           = "registry.k8s.io/pause:3.10.2@sha256:…"
snapshots_location    = "gs://ate-snapshots/ate-demo-helpdesk/"
runsc_amd64_sha256    = "a397…"
runsc_amd64_url       = "gs://gvisor/releases/nightly/…/runsc"
gateway_endpoint      = ""    # empty → supervisors stay in standalone mode
```

With those three pieces in place, every `openshell.v1.OpenShell.CreateSandbox`
call routes through this crate. A working sample — gateway image build,
projected SA-token + CA bundle wiring, kustomize-shaped Deployment, RBAC —
lives at [`examples/helpdesk/gateway/`](examples/helpdesk/gateway/); the
10-beat helpdesk demo at [`examples/helpdesk/`](examples/helpdesk/) drives
it end-to-end.

## What's in the box

| path | what |
|---|---|
| `src/lib.rs` | `SubstrateComputeDriver` — implements OpenShell's `ComputeDriver` gRPC trait against Substrate's `ateapi.Control`. The driver synthesizes `ate.dev/v1alpha1 ActorTemplate` resources and injects `OPENSHELL_BEST_EFFORT_FAILURES=1` into the supervisor container's env. |
| `src/template.rs` | `kube-rs` mirror of Substrate's `ActorTemplate` CRD; just the fields the driver writes and waits on. |
| `tests/live.rs` | Four live integration tests against a real `ate-api-server` (`#[ignore]`d; gated on `SUBSTRATE_LIVE_*` env vars). |
| `tests/integration/` | Feature-observation harness: builds the patched supervisor image, applies templates, spawns an actor, dumps `[oshl-test]` markers from worker pod logs. |
| `tests/integration/gateway/` | §7b end-to-end harness: deploys a real `openshell-gateway` (with a `docker:28-dind` sidecar + stub `supervisor_bin`), mints Ed25519 JWT signing material via `generate-jwt-keys.sh` (private key never lands in the repo), spawns a test actor wired with `OPENSHELL_ENDPOINT` + `OPENSHELL_SANDBOX_TOKEN` + `OPENSHELL_SANDBOX_ID`, and runs `verify-features.sh` to record PASS/FAIL for each of the five gateway-driven features. |
| `examples/helpdesk/` | 10-beat OpenShell-on-Substrate demo, three acts (provisioning, lifecycle, hygiene): create alice + bob → cold ask → suspend → idle → follow-up (memory preserved) → exfil deny → pod-kill migration → delete. Drives the gateway via `kubectl osh`; uses `kubectl ate` for ops OpenShell doesn't expose publicly (suspend, raw actor inspection). See [`examples/helpdesk/README.md`](examples/helpdesk/README.md). |
| `examples/gpu-counter/` | 6-beat GPU passthrough demo. Same gateway → driver → substrate provisioning path as helpdesk, but the supervisor execs a Python agent holding a 1 MiB on-device CUDA buffer via libcuda. Substrate's atelet picks up `containers[*].resources.gpu`, ateom-gvisor adds `--nvproxy` to runsc. Includes `validate-bare.sh` for pre-substrate `docker --runtime=runsc-gpu` validation. See [`examples/gpu-counter/README.md`](examples/gpu-counter/README.md). |
| `cmd/kubectl-osh/` | `kubectl-osh` plugin: operator-shaped CRUD against the gateway gRPC. Exposes the M3.16 `substrate_actor_template` annotation the upstream `openshell` CLI can't set. Used by the helpdesk demo and intended as the operator-facing tool for substrate-backed gateways. `make install` puts it on `$GOBIN`. See [`cmd/kubectl-osh/README.md`](cmd/kubectl-osh/README.md). |
| `proto/` | Vendored proto definitions: `ateapi.proto` (substrate, consumed by the Rust driver via `build.rs`), `openshell.proto` + `sandbox.proto` + `datamodel.proto` (OpenShell, consumed by the Go kubectl-osh plugin via `make proto`). |

## Build

```sh
cargo build --release
```

Cargo resolves `openshell-core` from the pinned-rev git dep on first
build; subsequent builds are cached.

Unit tests (no cluster required):
```sh
cargo test --lib
```

## Live integration tests

`tests/live.rs` exercises the full driver lifecycle against a running
`ate-api-server`. Required env vars: see the top of `tests/live.rs` for
the full list. Skip silently when any required var is missing.

```sh
SUBSTRATE_LIVE_API_ENDPOINT=127.0.0.1:18443 \
SUBSTRATE_LIVE_NAMESPACE=ate-openshell-m0 \
SUBSTRATE_LIVE_CA_PATH=/tmp/ate-servicedns-ca.pem \
SUBSTRATE_LIVE_BEARER_TOKEN_PATH=/tmp/ate-bearer.token \
SUBSTRATE_LIVE_TLS_SERVER_NAME=api.ate-system.svc \
SUBSTRATE_LIVE_WORKER_POOL=openshell-m0-pool \
SUBSTRATE_LIVE_SNAPSHOTS_LOCATION=gs://ate-snapshots/ate-openshell-m0/ \
SUBSTRATE_LIVE_RUNSC_AMD64_SHA=... \
SUBSTRATE_LIVE_RUNSC_AMD64_URL=gs://gvisor/releases/.../runsc \
SUBSTRATE_LIVE_PAUSE_IMAGE=registry.k8s.io/pause:3.10.2@sha256:... \
SUBSTRATE_LIVE_TEMPLATE_NAME=supervisor \
SUBSTRATE_LIVE_TEST_IMAGE=localhost:5001/oshl-feature-test@sha256:... \
  cargo test --test live -- --ignored --test-threads=1
```

## Feature-observation harness

`tests/integration/` builds a feature-test supervisor image, applies
the templates it depends on, spawns an actor via `grpcurl`, and dumps
the `[oshl-test]` markers from the worker pod's stdout for inspection.

The supervisor binary is built from the patched OpenShell source
(`build-image.sh` resolves the source tree in this order: `$OPENSHELL_REPO`,
sibling `../OpenShell`, then a clone at the pinned commit). The
resulting image bakes `OPENSHELL_BEST_EFFORT_FAILURES=1` in via the
Dockerfile and the YAML templates re-state it in `containers[].env`
for visibility.

Operator first-run:
1. From the substrate repo (`agent-substrate/substrate` or a fork):
   `KO_DOCKER_REPO=localhost:5001 ko publish ./cmd/servers/ateom-gvisor`
   and `export ATEOM_IMAGE='localhost:5001/ateom-gvisor@sha256:...'`.
2. From this repo: `./tests/integration/run.sh`.

Subsequent runs: `./tests/integration/run.sh` (the `ATEOM_IMAGE` env
var is captured in the live `WorkerPool` spec on first apply).

## §7b gateway-integration harness

`tests/integration/gateway/` stands up a real `openshell-gateway`
Deployment alongside the worker pool and exercises the supervisor's
cluster-mode features (settings poll, inference routing, log push, SSH
attach via `RelayStream`, cross-sandbox identity guard).

```sh
# One-time, before the first run on a fresh cluster:
export ATEOM_IMAGE='localhost:5001/ateom-gvisor@sha256:...'

cd tests/integration/gateway
./run-gateway-integration.sh        # builds + deploys + spawns + captures
./verify-features.sh /tmp/oshl-v3-<TS>   # PASS/FAIL summary for F1..F5
```

`generate-jwt-keys.sh` mints (or reuses) the Ed25519 JWT signing
material at `$OPENSHELL_JWT_DIR` (default: `/tmp`) and renders the
gateway Secret manifest to stdout — the private key never enters the
repo. Three features (F1 settings poll, F2 inference routing, F3 log
push) are PASS verified end-to-end; F4 SSH attach and F5 cross-sandbox
IDOR are deferred (template wiring exists; verification needs an
external SSH driver / per-actor JWTs). See
`~/notes/openshell-on-substrate/2026-05-23-openshell-features-findings.md`
§7b verification for the full results + sharp-edges register (SE-8..SE-13).

## Companion changes upstream

| PR | Effect |
|---|---|
| [`NVIDIA/OpenShell#1548`](https://github.com/NVIDIA/OpenShell/pull/1548) `[WIP]` | `OPENSHELL_BEST_EFFORT_FAILURES` env-var gate. 3 files, +51/-7. Default strict; opt-in via the env var. **Alternative shape; one of #1548 / #1549 will land.** |
| [`NVIDIA/OpenShell#1549`](https://github.com/NVIDIA/OpenShell/pull/1549) | `SandboxFailureHandler` trait + `StrictHandler` default + `set_failure_handler` setter. 3 files, +71/-7. Programmatic override only — no env var, no CLI flag, no `main.rs` changes. **Alternative shape; one of #1548 / #1549 will land.** |
| [`agent-substrate/substrate#66`](https://github.com/agent-substrate/substrate/pull/66) | `ateom-gvisor` `eth0` move/restore idempotency + deferred rollback. Without it, the test harness alternates between green and red runs. |
| [`agent-substrate/substrate#67`](https://github.com/agent-substrate/substrate/pull/67) | `install-ate-kind.sh` builds + pushes `ateom-gvisor` automatically, so a `WorkerPool` is usable out of `--deploy-ate-system`. Closes the manual `ko publish` operator step. |
| [`agent-substrate/substrate#73`](https://github.com/agent-substrate/substrate/pull/73) | Per-container `securityContext` on `ActorTemplate.spec.containers[]`: `capabilities.add` + `runAsUser` / `runAsGroup`. Empty templates produce the same OCI bundle as before. Unblocks the driver's `synthesize_template` from emitting capability adds + a non-root supervisor start UID once it merges. |
| [`agent-substrate/substrate#75`](https://github.com/agent-substrate/substrate/pull/75) | `ateapi/syncer: release actor when host pod is deleted`. `WorkerPoolSyncer`'s pod-delete hook resets the bound actor to `STATUS_SUSPENDED` so the next request migrates it onto a free worker, instead of stranding it pointing at a dead pod. Beat 9 of the helpdesk demo (pod-kill migration with multi-tenant proof) depends on it; verified end-to-end on bigbox 2026-05-24. |
| [`agent-substrate/substrate#96`](https://github.com/agent-substrate/substrate/pull/96) | GPU passthrough end-to-end: new `ActorTemplate.containers[*].resources.gpu` CRD field, ateletpb/ateompb proto threading, atelet OCI builder injects `/dev/nvidia*` + bind-mounts `cuda-checkpoint`, ateom-gvisor adds `--nvproxy --nvproxy-driver-version --nvproxy-allowed-driver-capabilities` to runsc create/checkpoint/restore. Driver-side counterpart in this repo: `0b46450` (single squashed commit). Required by [`examples/gpu-counter/`](examples/gpu-counter/). Verified end-to-end on an NVIDIA L40S (driver 580.126.09) 2026-05-27. |
