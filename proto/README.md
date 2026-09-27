# Vendored protos

`ateapi.proto` is copied from
`github.com/agent-substrate/substrate/pkg/proto/ateapipb/ateapi.proto` with
one edit: the two `[ debug_redact = true ]` field options are dropped (see
below). `build.rs` runs `tonic_prost_build` over it to generate the `Control`
client the driver uses (`src/lib.rs`'s `ateapi` module). The driver does not
vendor any OpenShell protos; the OpenShell-side message and trait types it
implements come from the `openshell-core` crate dependency.

The copy matches upstream at [`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c).

Refresh by re-copying the file from a current substrate checkout:

```sh
cp path/to/substrate/pkg/proto/ateapipb/ateapi.proto proto/ateapi.proto
gsed -i 's/ \[ debug_redact = true \];/;/' proto/ateapi.proto
```

## Why `debug_redact` is dropped

`debug_redact` is a standard protobuf field option (protobuf 22 and later)
that marks a field as secret-bearing so debug printers mask its value.
Substrate puts it on two fields, `EnvVar.value` and
`MintActorJWTResponse.actor_jwt`, and its gRPC logging interceptor masks those
fields in ate-api-server's logs
([#1803](https://github.com/agent-substrate/substrate/pull/1803)).

The driver compiles the proto with the protoc that the `protobuf-src` crate
bundles, `libprotoc 3.21.5`, which predates the option and rejects the file:

```
ateapi.proto:1065:22: Option "debug_redact" unknown. Ensure that your proto definition file imports the proto which defines the option.
```

Dropping it changes nothing for the driver: prost implements no redaction, so
the generated Rust is the same either way, and the driver never Debug-prints a
request or response. If it ever logs one, it needs its own masking for those
two fields.

To vendor the file byte for byte, build with a protoc that knows the option.
The cheap way is the one OpenShell itself uses: the `protoc-bin-vendored`
crate, which ships prebuilt binaries (libprotoc 31.1 at 3.2.0) and is already
in this repository's dependency graph through `openshell-core`. Swapping it in
for `protobuf-src` in `Cargo.toml` and `build.rs` also drops a from-source
build of libprotobuf from every clean build. Then the `gsed` line above goes.
