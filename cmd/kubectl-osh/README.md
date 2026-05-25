# kubectl-osh

A kubectl plugin for the OpenShell gateway on substrate.

Talks to an OpenShell gateway via its public gRPC API and exposes a CRUD
surface shaped for K8s operators. Supports the substrate-driver-specific
annotation (`substrate_actor_template`) that the upstream `openshell`
CLI does not — see "Why this exists" below.

## Install

```sh
cd cmd/kubectl-osh
make install              # builds + puts kubectl-osh on $GOBIN ($HOME/go/bin by default)
kubectl plugin list       # confirm kubectl picks it up
```

Once installed, both `kubectl-osh ...` and `kubectl osh ...` work.

## Usage

```sh
# Pre-req: a port-forward (or direct route) to the gateway.
kubectl port-forward -n ate-openshell-m0 svc/openshell-gateway-substrate 50051:50051 &
export OPENSHELL_GATEWAY=localhost:50051

# Create a sandbox bound to a pre-provisioned substrate ActorTemplate.
kubectl osh create sandbox alice \
   --image=localhost:5001/oshl-helpdesk@sha256:<digest> \
   --template=helpdesk-agent

# List / inspect.
kubectl osh get sandboxes
kubectl osh get sandbox alice -o yaml

# Delete (variadic, with --ignore-not-found for idempotent cleanup).
kubectl osh delete sandbox alice bob --ignore-not-found
```

### Flags

| Subcommand | Flag | Purpose |
|---|---|---|
| (global) | `--gateway HOST:PORT` | OpenShell gateway endpoint (default: `$OPENSHELL_GATEWAY`). |
| (global) | `--insecure` | Plaintext gRPC. Default true in v0.1; mTLS coming. |
| `create` | `--image` | OCI image reference. Required. |
| `create` | `--template` | Pre-provisioned substrate ActorTemplate name. Sets `annotations.substrate_actor_template` via the M3.16 path. |
| `create` | `--annotation key=value` | Extra annotation, repeatable. |
| `create` | `--label key=value` | Sandbox-template label, repeatable. |
| `create` | `--env key=value` | Environment variable, repeatable. |
| `create` | `--log-level` | `info` / `debug` / `warn` / `error`. |
| `get`/`create` | `-o yaml \| json \| wide` | Output format. Default: table for list, single line for create. |
| `delete` | `--ignore-not-found` | NotFound counts as success. |

## Why this exists

The upstream `openshell` CLI ships with the OpenShell tree and supports
sandbox CRUD, but it has two architectural gaps that block its use for
substrate-driver-shaped workloads (the helpdesk demo, the §7b feature
suite, anything that relies on the M3.16 driver path):

1. **No `--annotation` flag.** The CLI's `sandbox create` doesn't expose
   `SandboxTemplate.annotations` at all. The substrate driver reads
   `annotations.substrate_actor_template` (M3.16) to bind a sandbox to a
   pre-provisioned ActorTemplate — without a way to set that annotation,
   the demo would fall into the driver's synthesize-template path which
   hardcodes a sleep-loop child workload.
2. **No "use existing ActorTemplate" semantic.** The CLI's `--from`
   resolves a community sandbox name, a Dockerfile path, or an image
   reference. There's no way to say "use this pre-applied substrate
   ActorTemplate by name."

`kubectl-osh` exists to bridge those two gaps cleanly, *and* to be
operator-shaped — kubeconfig-aware (eventually), kubectl-conventional
output formats, integrates with `kubectl-ate` for the operations
OpenShell doesn't expose publicly (suspend, raw actor inspection).

A long-term path to make `kubectl-osh` redundant would be two small
upstream PRs on the OpenShell CLI:

* Add `--annotation key=value` repeatable flag to `sandbox create`.
* Add `--actor-template <name>` (or equivalent) that opts out of
  `--from`-driven image resolution and reuses an existing substrate
  ActorTemplate.

Until those land, `kubectl-osh` is the path of least resistance for
substrate-side operator tooling.

## What's missing in v0.1 (roadmap)

* `suspend sandbox NAME` (today: drop down to `kubectl ate suspend actor <uuid>`)
* `logs sandbox NAME [-f]`
* `watch sandboxes` streaming
* Auto-port-forward via kubeconfig discovery (no manual `kubectl port-forward` needed)
* mTLS / JWT auth (`--insecure` is the only mode today)
* krew packaging

Each of these is a small, additive change. v0.1's scope is the slice
needed to drop grpcurl from the helpdesk demo and prove the operator UX
direction.

## Layout

```
cmd/kubectl-osh/
├── main.go                # cobra entry point
├── Makefile
├── cmd/                   # subcommand handlers (create/get/delete/root/helpers)
├── internal/
│   ├── client/            # gRPC dial wrapper
│   └── output/            # -o yaml/json/wide + age formatting
└── pkg/proto/             # generated proto Go stubs (committed; regen with `make proto`)
    ├── openshell/v1/
    ├── sandbox/v1/
    └── datamodel/v1/
```

Protos are vendored from upstream OpenShell into `proto/` at the repo
root (alongside `ateapi.proto`). `make proto` regenerates the Go stubs.
The Rust driver in `src/` does not consume these proto files; only
`ateapi.proto` is in the Rust build path (see `build.rs`).
