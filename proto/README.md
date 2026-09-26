# Vendored protos

`ateapi.proto` is copied from
`github.com/agent-substrate/substrate/pkg/proto/ateapipb/ateapi.proto` with
one edit: the two `[ debug_redact = true ]` field options are dropped, because
the protoc that `protobuf-src` bundles predates them and a client has no use
for them.
`build.rs` runs `tonic_prost_build` over it to generate the `Control`
client the driver uses (`src/lib.rs`'s `ateapi` module). The driver does
not vendor any OpenShell protos — the OpenShell-side message and trait
types it implements come from the `openshell-core` crate dependency.

Refresh by re-copying the file from a current substrate checkout:

```sh
cp path/to/substrate/pkg/proto/ateapipb/ateapi.proto proto/ateapi.proto
gsed -i 's/ \[ debug_redact = true \];/;/' proto/ateapi.proto
```
