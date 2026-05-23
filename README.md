# openshell-driver-substrate

Agent Substrate (gVisor + checkpoint/restore via runsc) compute driver
for OpenShell, plus the substrate-aware supervisor wrapper binary that
lets the OpenShell sandbox actually boot inside a Substrate gVisor
actor in degraded mode.

This repository depends on a small trait-scaffolding change in
OpenShell that lives on
[`dims/OpenShell#chore/gvisor-degraded-netns-v2`](https://github.com/dims/OpenShell/tree/chore/gvisor-degraded-netns-v2)
(single commit, upstream-shaped). Cargo dependencies are pinned to
that exact commit.

## What's in the box

| path | what |
|---|---|
| `src/lib.rs` | `SubstrateComputeDriver` — implements OpenShell's `ComputeDriver` gRPC trait against Substrate's `ateapi.Control`. |
| `src/template.rs` | `kube-rs` mirror of Substrate's `ate.dev/v1alpha1 ActorTemplate` CRD; just enough fields for the driver to synthesize + wait for `Ready`. |
| `src/degraded.rs` | `DegradedHandler` — implementation of `openshell_sandbox::SandboxFailureHandler` that warns + continues when gVisor refuses the supervisor's `unshare(CLONE_NEWNET)` / `seccomp(SECCOMP_SET_MODE_FILTER)`. |
| `src/bin/openshell-sandbox-substrate.rs` | Drop-in supervisor binary: registers `DegradedHandler`, then delegates to `openshell_sandbox::cli::run()`. CLI surface is identical to upstream `openshell-sandbox`. |
| `proto/ateapi.proto` | Vendored from `agent-substrate/substrate`; `build.rs` runs `tonic_build` over it. |
| `tests/live.rs` | Four live integration tests against a real `ate-api-server` (`#[ignore]`d; gated on `SUBSTRATE_LIVE_*` env vars). |
| `tests/integration/` | Feature-observation harness: builds a feature-test supervisor image, applies templates, spawns an actor, dumps `[oshl-test]` markers from worker pod logs. |

## Build

```sh
cargo build --release --bin openshell-sandbox-substrate
```

Cargo resolves `openshell-core` + `openshell-sandbox` from the
pinned-rev git dep on first build; subsequent builds are cached.

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

Operator first-run:
1. From the substrate repo (`agent-substrate/substrate` or a fork):
   `KO_DOCKER_REPO=localhost:5001 ko publish ./cmd/servers/ateom-gvisor`
   and `export ATEOM_IMAGE='localhost:5001/ateom-gvisor@sha256:...'`.
2. From this repo: `./tests/integration/run.sh`.

Subsequent runs: `./tests/integration/run.sh` (the `ATEOM_IMAGE` env
var is captured in the live `WorkerPool` spec on first apply).

## Companion change in OpenShell

The wrapper binary calls `openshell_sandbox::cli::run()` and
`openshell_sandbox::set_failure_handler()`. Neither exists upstream
yet. The single-commit branch
[`dims/OpenShell@69d2054`](https://github.com/dims/OpenShell/commit/69d205479edf8443ab06e28458b350c0d4613fd9)
adds them as a structurally-clean refactor with zero behavioural
change for existing callers.
