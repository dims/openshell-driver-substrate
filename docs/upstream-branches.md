# Upstream changes

Nothing here is merged upstream. Until PRs exist, these branches are the
only copies.

## agent-substrate/substrate

**Branch:** https://github.com/dims/substrate/tree/lean-integration at
[`0ff8b818`](https://github.com/dims/substrate/commit/0ff8b818d515036409d4ce0070f00582cb010659) (fork of `agent-substrate/substrate`, branched from [`92a84388`](https://github.com/agent-substrate/substrate/commit/92a84388b44b9c1b9131ee62f58eaa87b76a53d8))

Eight commits, and one that only tightens comments. Every one was found by
running a real non-root workload; none is specific to OpenShell. Before them, every actor
container silently ran as root, and no container declaring a non-root `USER`
could run at all, on either sandbox class.

| Commit | Change |
|---|---|
| [`cf699047`](https://github.com/dims/substrate/commit/cf699047aa8410eab57841f68e05977418d09405) | `atelet`: honor a container image's own `USER` when building its OCI spec. `ocispec.Options` gains `UID`/`GID`, resolved by `resolveUser`. Numeric `uid[:gid]` only; a named user is a hard error, not a silent fall-back to root. `TestResolveUser`. |
| [`cbb8405e`](https://github.com/dims/substrate/commit/cbb8405eca8b7183bba3116d39fb551860c3ac13) | `atelet`: keep the pause container root regardless of its image's `USER`. Fallout of the above — `registry.k8s.io/pause:3.10.2` declares `USER 65535:65535` and the sandbox init cannot boot under it. |
| [`73fe6062`](https://github.com/dims/substrate/commit/73fe6062fff0fc69ce1e1a2c6cd8d3641742a04e) | `imagecache`: make the merged rootfs root searchable by non-root containers. `rootfs`/`upper` were `0700`, so a non-root process could not search its own `/`. `TestSetupBundleRootfs_RootIsSearchableByNonRoot`. |
| [`4fc5d550`](https://github.com/dims/substrate/commit/4fc5d55040709d2808a311aa6accaf297a022cff) | `ocispec`: set `no_new_privileges` on actor containers. |
| [`f63c6ec0`](https://github.com/dims/substrate/commit/f63c6ec0b6197bf6eb85e2abbd68729d324023df) | `atelet`: make durable-dir volumes writable by non-root containers (`0777`, as Kubernetes gives an emptyDir). |
| [`33397540`](https://github.com/dims/substrate/commit/33397540c9983b52554fff4273b1d8eb3a4f8535) | `microvm`: forward `Linux.Sysctl` to the kata agent, and set `net.ipv4.ip_unprivileged_port_start=0` so a capability-free container can bind the DNS relay port. |
| [`1fb15974`](https://github.com/dims/substrate/commit/1fb159744caeade22cc9dce6669cd86c5c5211d6) | `ocispec`: move both of the preceding two fields out of the shared builder and into `ShapeMicroVM`. `runsc restore` compares them against the checkpoint-time spec, so setting them for every runtime made older gVisor snapshots unrestorable. Neither does anything under gVisor. `TestShapeMicroVM_SetsGuestOnlyProcessAndSysctlFields`. |
| [`0ff8b818`](https://github.com/dims/substrate/commit/0ff8b818d515036409d4ce0070f00582cb010659) | `atelet`: keep `CAP_DAC_OVERRIDE`. A non-root container can create a directory of its own inside a 0777 durable-dir volume, and plain root cannot unlink files from a directory another uid owns, so `resetActorDirs` failed after every checkpoint of such an actor. OpenShell's supervisor writes its CA material into one. |

[`975d1e72`](https://github.com/dims/substrate/commit/975d1e72e7497e2960f67a80b0dc7df26f88fd2b) on the same branch only tightens comments.

Three of these change behaviour for existing workloads and should be called out
in any PR:

- [`4fc5d550`](https://github.com/dims/substrate/commit/4fc5d55040709d2808a311aa6accaf297a022cff) is unconditional and there is no `SecurityContext` field to opt
  out. The kata agent enforces it, so a workload relying on setuid inside its
  sandbox would change behaviour. It applies to micro-VM only, after
  [`1fb15974`](https://github.com/dims/substrate/commit/1fb159744caeade22cc9dce6669cd86c5c5211d6).
- [`f63c6ec0`](https://github.com/dims/substrate/commit/f63c6ec0b6197bf6eb85e2abbd68729d324023df) makes durable-dir volumes world-writable. That is what Kubernetes
  does for an emptyDir, and the volume is per-actor inside a sandbox, but it is
  a deliberate choice worth stating.
- [`0ff8b818`](https://github.com/dims/substrate/commit/0ff8b818d515036409d4ce0070f00582cb010659) gives atelet one capability back after the manifest deliberately
  dropped them all. The alternatives are to move durable-dir cleanup into
  ateom, which already holds the capability, or to squash guest uids to root
  in virtiofsd. Both are larger changes; either would let atelet stay
  capability-free.

The same changes, rebased on `agent-substrate/substrate` main at [`74bbfc52`](https://github.com/agent-substrate/substrate/commit/74bbfc529ca9b5554d20117c2c78a3aea48b5d55)
(2026-09-23), are three PR branches on the fork. Upstream [`74bbfc52`](https://github.com/agent-substrate/substrate/commit/74bbfc529ca9b5554d20117c2c78a3aea48b5d55) deleted
the `ateerrors` taxonomy, so there `resolveUser` returns a plain error.

| Branch | Commits |
|---|---|
| [`pr/non-root-containers`](https://github.com/dims/substrate/tree/pr/non-root-containers) | [`01257dcd`](https://github.com/dims/substrate/commit/01257dcd323f50020d3e2df006d45aab40bb88ad) atelet: honor a container image's own USER · [`dabed87f`](https://github.com/dims/substrate/commit/dabed87f5c273b037e4dfa919ef86efda5458e7e) imagecache: make the merged rootfs root searchable by a non-root container |
| [`pr/durable-dirs`](https://github.com/dims/substrate/tree/pr/durable-dirs) | [`f8288487`](https://github.com/dims/substrate/commit/f828848723d97f77eb1d1133041daeb98c220a31) atelet: make durable-dir volumes writable by non-root containers · [`3349d5b8`](https://github.com/dims/substrate/commit/3349d5b8b5661c6f9d6917d67e94d430c822640c) atelet: keep CAP_DAC_OVERRIDE |
| [`pr/microvm-spec`](https://github.com/dims/substrate/tree/pr/microvm-spec) | [`de143ac6`](https://github.com/dims/substrate/commit/de143ac68a4490b024f1a3346ca41f0bced0c104) microvm: forward Linux.Sysctl to the kata agent · [`1ede1a74`](https://github.com/dims/substrate/commit/1ede1a749f173c7443eafd0b84c112913e31ad5b) microvm: set no_new_privileges and net.ipv4.ip_unprivileged_port_start=0 |

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
