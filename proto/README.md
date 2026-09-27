# Vendored protos

`ateapi.proto` is a verbatim copy of
`github.com/agent-substrate/substrate/pkg/proto/ateapipb/ateapi.proto` at the
commit named in [`upstream-rev`](upstream-rev). CI fetches the file at that
commit and fails on any difference, so the copy cannot drift from its pin.
`build.rs` runs `tonic_prost_build` over it, with the protoc that
`protoc-bin-vendored` ships, to generate the `Control` client the driver uses
(`src/lib.rs`'s `ateapi` module). `examples/helpdesk/run.sh` hands the same
file to `grpcurl`. The driver does not vendor any OpenShell protos; the
OpenShell-side message and trait types it implements come from the
`openshell-core` crate dependency.

Refresh by moving the pin and copying the file at it; a bump usually means
driver code changes too, in the same commit:

```sh
echo <substrate commit> > proto/upstream-rev
curl -fsSL "https://raw.githubusercontent.com/agent-substrate/substrate/$(cat proto/upstream-rev)/pkg/proto/ateapipb/ateapi.proto" -o proto/ateapi.proto
```

Two fields carry `[ debug_redact = true ]`, `EnvVar.value` and
`MintActorJWTResponse.actor_jwt`: a standard protobuf option (protobuf 22 and
later) that marks a field as secret-bearing, which Substrate's gRPC logging
interceptor uses to mask them
([#1803](https://github.com/agent-substrate/substrate/pull/1803)). prost
implements no redaction, so the option changes nothing in the generated Rust,
and the driver must not Debug-print a request or response that could carry
those fields.
