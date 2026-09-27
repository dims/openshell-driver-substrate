# Vendored protos

`ateapi.proto` is a verbatim copy of
`github.com/agent-substrate/substrate/pkg/proto/ateapipb/ateapi.proto` at
[`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c).
`build.rs` runs `tonic_prost_build` over it, with the protoc that
`protoc-bin-vendored` ships, to generate the `Control` client the driver uses
(`src/lib.rs`'s `ateapi` module). `examples/helpdesk/run.sh` hands the same
file to `grpcurl`. The driver does not vendor any OpenShell protos; the
OpenShell-side message and trait types it implements come from the
`openshell-core` crate dependency.

Refresh by copying the file from a current substrate checkout:

```sh
cp path/to/substrate/pkg/proto/ateapipb/ateapi.proto proto/ateapi.proto
```

Two fields carry `[ debug_redact = true ]`, `EnvVar.value` and
`MintActorJWTResponse.actor_jwt`: a standard protobuf option (protobuf 22 and
later) that marks a field as secret-bearing, which Substrate's gRPC logging
interceptor uses to mask them
([#1803](https://github.com/agent-substrate/substrate/pull/1803)). prost
implements no redaction, so the option changes nothing in the generated Rust,
and the driver must not Debug-print a request or response that could carry
those fields.
