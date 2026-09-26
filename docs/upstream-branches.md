# Upstream changes

Three of the changes are open as draft PRs on `agent-substrate/substrate`.
The rest exist only on the fork branch below.

## agent-substrate/substrate

**Branch:** https://github.com/dims/substrate/tree/lean-integration at
[`8a290199`](https://github.com/dims/substrate/commit/8a290199df2c5dd7b2ecc8d5c43627182803ecab): seven commits on `agent-substrate/substrate` main
[`d3aec58c`](https://github.com/agent-substrate/substrate/commit/d3aec58cddfaf52d0c9a96dbb77b5e2b1ccbb08e). The previous head, [`0ff8b818`](https://github.com/dims/substrate/commit/0ff8b818d515036409d4ce0070f00582cb010659),
is kept as `archive/lean-integration-2026-09-23`.

Every commit was found by running a real non-root workload; none is specific
to OpenShell. Before them, every actor container silently ran as root, and no
container declaring a non-root `USER` could run at all, on either sandbox class.

| Commit | Change | Upstream |
|---|---|---|
| [`6ee8bfa3`](https://github.com/dims/substrate/commit/6ee8bfa3edc154b5c8aff26394b54e4930702b27) | `imagecache`: make the merged rootfs root searchable by non-root. `rootfs` and `upper` were `0700`; on gVisor the container's `/` takes that mode, so a non-root process could not search its own root. The micro-VM guest already saw `0755`. `TestSetupBundleRootfs_RootIsSearchableByNonRoot`. | [#1905](https://github.com/agent-substrate/substrate/pull/1905) draft |
| [`b98f84a0`](https://github.com/dims/substrate/commit/b98f84a020f0d6e38ff949248ee06cdfc0087746) | `atelet`: honor a container image's own `USER`. `ocispec.Options` gains `UID`/`GID`, resolved by `resolveUser`. Numeric `uid[:gid]` only; a named user is an error, not a silent fall-back to root. The pause container stays root. `TestResolveUser`. | not filed: the no-group default (`gid = uid`) matches no runtime, named users including `USER root` fail, and the pause exception needs re-testing now that `/` is `0755` |
| [`48ef3417`](https://github.com/dims/substrate/commit/48ef341739dfc1940f416cfb52747d4e01359532) | `atelet`: make durable-dir volumes writable by non-root containers (`0777`, as Kubernetes gives an emptyDir). `TestPrepareDurableDirVolume`. | [#1906](https://github.com/agent-substrate/substrate/pull/1906) draft |
| [`7fa169d9`](https://github.com/dims/substrate/commit/7fa169d92f338ab8c8e84bf3d34457858b890c30) | `atelet`: add `CAP_DAC_OVERRIDE` to reset dirs a non-root container wrote. Plain root cannot unlink files from a directory another uid owns, so `resetActorDirs` failed after every checkpoint of such an actor and the suspend never completed. | not filed: needs the alternatives argued first (cleanup in ateom, which already holds the capability) and an e2e |
| [`b82e046b`](https://github.com/dims/substrate/commit/b82e046b22147802f0cb75bc44ec9488722fdc74) | `microvm`: forward `Linux.Sysctl` to the kata agent. `TestSpecToAgentPB_ForwardsSysctl`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) draft |
| [`1a944f26`](https://github.com/dims/substrate/commit/1a944f2602ff42ed386775772fbbb4fe3bb22d3d) | `microvm`: let a capability-free container bind a low port: `net.ipv4.ip_unprivileged_port_start=0` in `ShapeMicroVM`, parity with gVisor, whose netstack has no privileged-port check. `TestShapeMicroVM_AllowsLowPortsInTheGuest`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) draft |
| [`8a290199`](https://github.com/dims/substrate/commit/8a290199df2c5dd7b2ecc8d5c43627182803ecab) | `microvm`: set `no_new_privileges` on every guest container, parity with `runsc --allow-suid=false`. `TestShapeMicroVM_SetsNoNewPrivileges`. | not filed: a policy flip with no `SecurityContext` opt-out; maintainers will ask for an `allowPrivilegeEscalation` field |

Both `ShapeMicroVM` fields stay out of the shared spec builder: `runsc restore`
compares them with the checkpoint-time spec, so a shared default would make
older gVisor snapshots unrestorable.

Behavior changes for existing workloads, to state in any PR:

- [`b98f84a0`](https://github.com/dims/substrate/commit/b98f84a020f0d6e38ff949248ee06cdfc0087746): an image with a numeric `USER` used to run as root and now
  runs as that user; one with a named `USER` now fails to start.
- [`48ef3417`](https://github.com/dims/substrate/commit/48ef341739dfc1940f416cfb52747d4e01359532): durable dirs are world-writable inside the actor. That is
  what Kubernetes does for an emptyDir, and only that actor's containers can
  reach the directory.
- [`7fa169d9`](https://github.com/dims/substrate/commit/7fa169d92f338ab8c8e84bf3d34457858b890c30): atelet holds one capability where the manifest dropped them
  all. ateom already holds it; the alternative is to move durable-dir cleanup
  there.
- [`8a290199`](https://github.com/dims/substrate/commit/8a290199df2c5dd7b2ecc8d5c43627182803ecab): every micro-VM container gets `no_new_privileges`; there is
  no field to opt out.

Not fixed: when a suspend fails after `CheckpointWorkload` has succeeded and
ateom has torn the VM down, the reconciler retries the checkpoint against a VM
that no longer exists, every 10 seconds, without end. The actor and its
template can then not be deleted (`Aborted: another operation is in progress`).

The driver's vendored `proto/ateapi.proto` tracks this head; see
[`../proto/README.md`](../proto/README.md).

## kata-containers

**No changes.** The guest kernel is stock.

## NVIDIA/OpenShell

**No changes.** The driver depends on upstream unmodified (`openshell-core`,
pinned by rev in `Cargo.toml`), and the sandbox and supervisor binaries in the
images are stock.
