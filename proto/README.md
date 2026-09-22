# Vendored protos

`ateapi.proto` is copied as-is from
`github.com/agent-substrate/substrate/pkg/proto/ateapipb/ateapi.proto`.
`build.rs` runs `tonic_prost_build` over it to generate the `Control`
client the driver uses (`src/lib.rs`'s `ateapi` module). The driver does
not vendor any OpenShell protos — the OpenShell-side message and trait
types it implements come from the `openshell-core` crate dependency.

Refresh by re-copying the file from a current substrate checkout:

```sh
cp path/to/substrate/pkg/proto/ateapipb/ateapi.proto proto/ateapi.proto
```
