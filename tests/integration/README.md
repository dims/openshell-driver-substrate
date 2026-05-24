# OpenShell-on-Substrate feature observation suite

Standalone harness used to walk through the supervisor's value-add
features (HTTP CONNECT proxy, OPA/Rego policy, OCSF audit, Landlock
probe, filesystem allow/deny) end-to-end inside a Substrate gVisor
actor. The four `live` integration tests next door only exercise the
driver-side lifecycle (create / get / stop / delete); this harness
exercises the supervisor side.

## Layout

| Path | Purpose |
|---|---|
| `Dockerfile` | Self-contained build from the upstream community sandbox base. Bakes in the substrate-aware wrapper binary as `/usr/local/bin/openshell-sandbox`, the canonical `policy.rego`, a test-mode `data.yaml`, and `test-workload.sh`. |
| `data.yaml` | Test policy: one allow rule (`example.com` via `curl`), one denied path so both allow and deny arms of the OPA engine are exercised. |
| `test-workload.sh` | Workload command line; runs probes (fs read/write, proxy CONNECT, direct egress, supervisor log inspection) and prints `[oshl-test] <key>: <value>` lines to stderr so `kubectl logs` of the worker pod surfaces them. |
| `cluster-setup.yaml` | One-shot bootstrap (namespace, WorkerPool, basic `supervisor` template). Applied only on the first run; `run.sh` guards on namespace presence. WorkerPool's `ateomImage` is substituted at apply time from the `ATEOM_IMAGE` env var. |
| `actor-template.yaml` | Pre-provisioned `oshl-feature-test` ActorTemplate that wraps `test-workload.sh`. |
| `policy.rego` | Canonical sandbox policy vendored from `dims/OpenShell@69d2054:crates/openshell-sandbox/data/sandbox-policy.rego`. Baked into the supervisor image at `/etc/openshell/policy.rego`. |
| `build-image.sh` | Build + push helper. Runs on a Linux host with cargo + docker + access to the kind-registry. Builds the `openshell-sandbox-substrate` wrapper and copies the vendored `policy.rego` + `data.yaml` + `test-workload.sh` into the build context. |
| `run.sh` | End-to-end driver: builds the supervisor image, applies the templates, spawns an actor via `grpcurl`, waits, dumps worker pod logs filtered for `[oshl-test]`, suspends + deletes. |

## Operator flow

1. **First-time setup**: `install-ate-kind.sh` builds and deploys
   `atelet` but NOT `ateom-gvisor`. Produce the latter with `ko publish
   ./cmd/servers/ateom-gvisor` from the substrate repo, then export
   the resulting digest:
   ```sh
   export ATEOM_IMAGE='localhost:5001/ateom-gvisor@sha256:...'
   ```
2. **Run**: `./run.sh`. Subsequent runs do not need `ATEOM_IMAGE`; the
   value is captured in the live `WorkerPool` spec.

The harness expects `grpcurl` on the host PATH (or at `~/go/bin/grpcurl`).

### Host build deps

`build-image.sh` runs `cargo build --release --bin openshell-sandbox`
on the host (not inside docker), so the host needs:

| Tool | Reason | Install |
|---|---|---|
| `cargo` (rust ≥ 1.88, rustup-managed) | builds the supervisor binary | `rustup` |
| `protoc` (system-wide) | `openshell-core`'s build script invokes `prost-build`, which calls `protoc` to compile the workspace's `*.proto` files. Must be system-installed; setting the `PROTOC` env var doesn't always propagate into cargo's build-script subprocess under rustup. | `sudo apt-get install protobuf-compiler` (Debian/Ubuntu) / `brew install protobuf` (macOS) |
| `docker` | image build + push | distro package |

Symptom of missing protoc:
```
Error: Could not find `protoc`. If `protoc` is installed, try setting
the `PROTOC` environment variable to the path of the `protoc` binary.
```
followed by `build-image.sh: line 39: BUILD_CTX: unbound variable` (the
cleanup trap fires before BUILD_CTX is set). Install the package
system-wide; the env-var workaround is unreliable.

The companion gateway image build (`examples/helpdesk/gateway/build-image.sh`)
does NOT have this requirement — it builds the gateway inside docker
where `protoc` is `apt-get install`-ed in the builder stage.

## Findings register

Per-test status lands in `~/notes/2026-05-23-openshell-features-findings.md`. Sharp edges are enumerated at the bottom of that doc as SE-1..SE-7.
