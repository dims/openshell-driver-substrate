# openshell-driver-substrate

Agent Substrate (gVisor + checkpoint/restore via runsc) compute driver
for OpenShell.

This repository depends on a small change in OpenShell that adds an
operator-opt-in escape hatch (`OPENSHELL_BEST_EFFORT_FAILURES`) so the
supervisor tolerates the bootstrap subsystems gVisor degrades. The
change is one commit on
[`dims/OpenShell#chore/gvisor-degraded-netns-v2`](https://github.com/dims/OpenShell/tree/chore/gvisor-degraded-netns-v2)
(3 files, +51/-7). Cargo's `openshell-core` dep is pinned to that
commit.

## What's in the box

| path | what |
|---|---|
| `src/lib.rs` | `SubstrateComputeDriver` — implements OpenShell's `ComputeDriver` gRPC trait against Substrate's `ateapi.Control`. The driver synthesizes `ate.dev/v1alpha1 ActorTemplate` resources and injects `OPENSHELL_BEST_EFFORT_FAILURES=1` into the supervisor container's env. |
| `src/template.rs` | `kube-rs` mirror of Substrate's `ActorTemplate` CRD; just the fields the driver writes and waits on. |
| `proto/ateapi.proto` | Vendored from `agent-substrate/substrate`; `build.rs` runs `tonic_build` over it. |
| `tests/live.rs` | Four live integration tests against a real `ate-api-server` (`#[ignore]`d; gated on `SUBSTRATE_LIVE_*` env vars). |
| `tests/integration/` | Feature-observation harness: builds the patched supervisor image, applies templates, spawns an actor, dumps `[oshl-test]` markers from worker pod logs. |

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

## Companion change in OpenShell

The driver relies on `OPENSHELL_BEST_EFFORT_FAILURES` being recognised
by the supervisor. The single-commit branch
[`dims/OpenShell@b6d3a35`](https://github.com/dims/OpenShell/commit/b6d3a35facab8e597a516ebf4ddd2989ad558ce6)
adds the env-var gate (3 files, +51/-7) with strict defaults
preserved for upstream callers.
