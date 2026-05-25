# Vendored protos

This directory holds proto definitions copied from upstream projects.
Two different toolchains consume them:

- **Rust** — the substrate compute driver in `src/`. `build.rs` runs
  `tonic_build` over `ateapi.proto` only (the driver speaks to
  `ate-api-server`; it does not consume OpenShell's public protos
  directly because it depends on `openshell-core` for those types).
- **Go** — the [`kubectl-osh`](../cmd/kubectl-osh) plugin. `make proto`
  in `cmd/kubectl-osh/` runs `protoc-gen-go` + `protoc-gen-go-grpc`
  over the OpenShell protos to produce the typed gRPC client the
  plugin uses to dial the gateway.

| File | Source | Consumer |
|---|---|---|
| `ateapi.proto` | `github.com/agent-substrate/substrate/proto/ateapipb/ateapi.proto` | Rust (`src/lib.rs`) via `build.rs`. Substrate's `Control` + `SessionIdentity`; only `Control` is used. |
| `openshell.proto` | `github.com/NVIDIA/OpenShell/proto/openshell.proto` | Go (`cmd/kubectl-osh/`) via `make proto`. The public OpenShell gateway service. |
| `sandbox.proto` | `github.com/NVIDIA/OpenShell/proto/sandbox.proto` | Go. Imported by `openshell.proto`. |
| `datamodel.proto` | `github.com/NVIDIA/OpenShell/proto/datamodel.proto` | Go. Imported by `openshell.proto`. |

The three OpenShell protos carry a `option go_package = ...` line that
points at `cmd/kubectl-osh/pkg/proto/.../v1`. The Rust build does not
look at that option (`build.rs` only compiles `ateapi.proto`), so the
Go-specific annotation is inert as far as the driver crate is
concerned. Refresh upstream-side files by re-copying as-is and
re-applying the `option go_package` line; then run `make proto` in
`cmd/kubectl-osh/` to regenerate the Go stubs.
