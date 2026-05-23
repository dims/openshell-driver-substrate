# openshell-driver-substrate

Agent Substrate (gVisor + checkpoint/restore via runsc) compute driver
for OpenShell.

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

## What's in the box

| path | what |
|---|---|
| `src/lib.rs` | `SubstrateComputeDriver` — implements OpenShell's `ComputeDriver` gRPC trait against Substrate's `ateapi.Control`. The driver synthesizes `ate.dev/v1alpha1 ActorTemplate` resources and injects `OPENSHELL_BEST_EFFORT_FAILURES=1` into the supervisor container's env. |
| `src/template.rs` | `kube-rs` mirror of Substrate's `ActorTemplate` CRD; just the fields the driver writes and waits on. |
| `proto/ateapi.proto` | Vendored from `agent-substrate/substrate`; `build.rs` runs `tonic_build` over it. |
| `tests/live.rs` | Four live integration tests against a real `ate-api-server` (`#[ignore]`d; gated on `SUBSTRATE_LIVE_*` env vars). |
| `tests/integration/` | Feature-observation harness: builds the patched supervisor image, applies templates, spawns an actor, dumps `[oshl-test]` markers from worker pod logs. |
| `tests/integration/gateway/` | §7b end-to-end harness: deploys a real `openshell-gateway` (with a `docker:28-dind` sidecar + stub `supervisor_bin`), mints Ed25519 JWT signing material via `generate-jwt-keys.sh` (private key never lands in the repo), spawns a test actor wired with `OPENSHELL_ENDPOINT` + `OPENSHELL_SANDBOX_TOKEN` + `OPENSHELL_SANDBOX_ID`, and runs `verify-features.sh` to record PASS/FAIL for each of the five gateway-driven features. |

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
