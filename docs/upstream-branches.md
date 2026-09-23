# Upstream changes

Nothing here is merged upstream. Until PRs exist, these branches are the
only copies.

## agent-substrate/substrate

**Branch:** https://github.com/dims/substrate/tree/lean-integration at
`0ff8b818` (fork of `agent-substrate/substrate`, branched from `92a84388`)

Eight commits, and one that only tightens comments. Every one was found by
running a real non-root workload; none is specific to OpenShell. Before them, every actor
container silently ran as root, and no container declaring a non-root `USER`
could run at all, on either sandbox class.

| Commit | Change |
|---|---|
| `cf699047` | `atelet`: honor a container image's own `USER` when building its OCI spec. `ocispec.Options` gains `UID`/`GID`, resolved by `resolveUser`. Numeric `uid[:gid]` only; a named user is a hard error, not a silent fall-back to root. `TestResolveUser`. |
| `cbb8405e` | `atelet`: keep the pause container root regardless of its image's `USER`. Fallout of the above — `registry.k8s.io/pause:3.10.2` declares `USER 65535:65535` and the sandbox init cannot boot under it. |
| `73fe6062` | `imagecache`: make the merged rootfs root searchable by non-root containers. `rootfs`/`upper` were `0700`, so a non-root process could not search its own `/`. `TestSetupBundleRootfs_RootIsSearchableByNonRoot`. |
| `4fc5d550` | `ocispec`: set `no_new_privileges` on actor containers. |
| `f63c6ec0` | `atelet`: make durable-dir volumes writable by non-root containers (`0777`, as Kubernetes gives an emptyDir). |
| `33397540` | `microvm`: forward `Linux.Sysctl` to the kata agent, and set `net.ipv4.ip_unprivileged_port_start=0` so a capability-free container can bind the DNS relay port. |
| `1fb15974` | `ocispec`: move both of the preceding two fields out of the shared builder and into `ShapeMicroVM`. `runsc restore` compares them against the checkpoint-time spec, so setting them for every runtime made older gVisor snapshots unrestorable. Neither does anything under gVisor. `TestShapeMicroVM_SetsGuestOnlyProcessAndSysctlFields`. |
| `0ff8b818` | `atelet`: keep `CAP_DAC_OVERRIDE`. A non-root container can create a directory of its own inside a 0777 durable-dir volume, and plain root cannot unlink files from a directory another uid owns, so `resetActorDirs` failed after every checkpoint of such an actor. OpenShell's supervisor writes its CA material into one. |

`975d1e72` on the same branch only tightens comments.

Three of these change behaviour for existing workloads and should be called out
in any PR:

- `4fc5d550` is unconditional and there is no `SecurityContext` field to opt
  out. The kata agent enforces it, so a workload relying on setuid inside its
  sandbox would change behaviour. It applies to micro-VM only, after
  `1fb15974`.
- `f63c6ec0` makes durable-dir volumes world-writable. That is what Kubernetes
  does for an emptyDir, and the volume is per-actor inside a sandbox, but it is
  a deliberate choice worth stating.
- `0ff8b818` gives atelet one capability back after the manifest deliberately
  dropped them all. The alternatives are to move durable-dir cleanup into
  ateom, which already holds the capability, or to squash guest uids to root
  in virtiofsd. Both are larger changes; either would let atelet stay
  capability-free.

The same changes, rebased on `agent-substrate/substrate` main `74bbfc52`,
are the three PR branches on the fork: `pr/non-root-containers` (`dabed87f`),
`pr/durable-dirs` (`3349d5b8`), `pr/microvm-spec` (`1ede1a74`). Upstream
`74bbfc52` deleted the `ateerrors` taxonomy, so there `resolveUser` returns a
plain error.

Not fixed: when a suspend fails after `CheckpointWorkload` has succeeded and
ateom has torn the VM down, the reconciler retries the checkpoint against a VM
that no longer exists, every 10 seconds, without end. The actor and its
template can then not be deleted (`Aborted: another operation is in progress`).

## kata-containers

**No changes.** The guest kernel is stock.

## NVIDIA/OpenShell

**No changes.** The driver depends on upstream unmodified (`openshell-core`,
pinned by rev in `Cargo.toml`), and the sandbox and supervisor binaries in the
images are stock.
