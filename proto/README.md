# Vendored protos

This directory holds proto definitions copied from upstream projects. The
build script (`build.rs`) compiles them with `tonic_build` to produce the
gRPC client used by the Substrate driver.

| File | Source | Notes |
|---|---|---|
| `ateapi.proto` | `github.com/agent-substrate/substrate/proto/ateapipb/ateapi.proto` | Substrate's `Control` + `SessionIdentity` services. We only use `Control`. |

Refresh by copying the upstream file as-is. The Rust generation handles
`option go_package` lines automatically.
