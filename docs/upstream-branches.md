# Upstream changes

All seven changes are filed on `agent-substrate/substrate` in five draft
PRs. None is merged. State as of 2026-09-26.

## agent-substrate/substrate

**Branch:** https://github.com/dims/substrate/tree/lean-integration at
[`ec91ff65`](https://github.com/dims/substrate/commit/ec91ff65dab1a3a674cbb0660bf9be1b178b4b86): seven commits on `agent-substrate/substrate` main
[`d3aec58c`](https://github.com/agent-substrate/substrate/commit/d3aec58cddfaf52d0c9a96dbb77b5e2b1ccbb08e). Previous heads are kept as
`archive/lean-integration-2026-09-23` ([`0ff8b818`](https://github.com/dims/substrate/commit/0ff8b818d515036409d4ce0070f00582cb010659))
and `archive/lean-integration-2026-09-25` ([`8a290199`](https://github.com/dims/substrate/commit/8a290199df2c5dd7b2ecc8d5c43627182803ecab)).

Every commit was found by running a real non-root workload; none is specific
to OpenShell. Before them, every actor container silently ran as root, and no
container declaring a non-root `USER` could run at all, on either sandbox class.

| Commit | Change | Upstream |
|---|---|---|
| [`6ee8bfa3`](https://github.com/dims/substrate/commit/6ee8bfa3edc154b5c8aff26394b54e4930702b27) | `imagecache`: make the merged rootfs root searchable by non-root. `rootfs` and `upper` were `0700`; on gVisor the container's `/` takes that mode, so a non-root process could not search its own root. The micro-VM guest already saw `0755`. `TestSetupBundleRootfs_RootIsSearchableByNonRoot`. | [#1905](https://github.com/agent-substrate/substrate/pull/1905) draft |
| [`7b3d05cf`](https://github.com/dims/substrate/commit/7b3d05cf7354b16c0476ed68aae34b5a3bde5934) | `atelet`: honor a container image's own `USER`. `ocispec.Options` gains `UID`/`GID`, resolved by `resolveUser` with containerd's rule: numeric `uid[:gid]`, no group means gid 0, `root` is 0, ids are bounded to int32; a named user is an error, not a silent fall-back to root. The pause container follows the same rule. `TestResolveUser`, `TestBuild_ProcessUser`, `TestShapers_PreserveProcessUser`, `TestSpecToAgentPB_ForwardsProcessUser`. | [#1918](https://github.com/agent-substrate/substrate/pull/1918) draft, stacked on [#1905](https://github.com/agent-substrate/substrate/pull/1905)'s commit; not to merge before [#1906](https://github.com/agent-substrate/substrate/pull/1906) and [#1910](https://github.com/agent-substrate/substrate/pull/1910) |
| [`e0443e74`](https://github.com/dims/substrate/commit/e0443e74df7db438dadde4dd8c5c20c1bf92c33a) | `atelet`: make durable-dir volumes writable by non-root containers (`0777`, as Kubernetes gives an emptyDir). `TestPrepareDurableDirVolume`. | [#1906](https://github.com/agent-substrate/substrate/pull/1906) draft |
| [`aec4bd06`](https://github.com/dims/substrate/commit/aec4bd0667090aedcc8ce285c298c0bc4da26300) | `atelet`: add `CAP_DAC_OVERRIDE` to reset dirs a non-root container wrote. Plain root cannot unlink files from a directory another uid owns, so `resetActorDirs` failed after every checkpoint of such an actor and the suspend never completed. | [#1910](https://github.com/agent-substrate/substrate/pull/1910) draft; its own PR after [#1906](https://github.com/agent-substrate/substrate/pull/1906), the body argues the alternatives, cleanup in ateom first among them |
| [`7162086f`](https://github.com/dims/substrate/commit/7162086fce01b5b08cf80ef149d5ff8ac74280d9) | `microvm`: forward `Linux.Sysctl` to the kata agent. `TestSpecToAgentPB_ForwardsSysctl`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) draft |
| [`fa054299`](https://github.com/dims/substrate/commit/fa054299b38d52f39f29d03bf9205f8ffcf62127) | `microvm`: let a capability-free container bind a low port: `net.ipv4.ip_unprivileged_port_start=0` in `ShapeMicroVM`, parity with gVisor, whose netstack has no privileged-port check. `TestShapeMicroVM_AllowsLowPortsInTheGuest`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) draft |
| [`ec91ff65`](https://github.com/dims/substrate/commit/ec91ff65dab1a3a674cbb0660bf9be1b178b4b86) | `microvm`: set `no_new_privileges` on every guest container, parity with `runsc --allow-suid=false`. `TestShapeMicroVM_SetsNoNewPrivileges`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) draft, third commit; the body says why no `allowPrivilegeEscalation` field is proposed and offers to split the commit out |

Both `ShapeMicroVM` fields stay out of the shared spec builder: `runsc restore`
compares them with the checkpoint-time spec, so a shared default would make
older gVisor snapshots unrestorable.

Related, not on the branch:

- [#1912](https://github.com/agent-substrate/substrate/pull/1912) (draft,
  `pr/ateom-cert-actor-identity`) puts the ActorIdentity extension back on the
  ateom-for-actor certificate. Upstream [#1809](https://github.com/agent-substrate/substrate/pull/1809) removed it, and the pinned
  agentgateway image still resolves the actor from it, so the agentgateway e2e
  lane fails on main and on every PR above. The lane is not a required check.
  After [#1912](https://github.com/agent-substrate/substrate/pull/1912) merges, the four PR branches get rebased onto main so their runs
  pick it up.
- [#1911](https://github.com/agent-substrate/substrate/issues/1911) proposes
  renaming the node state root `/var/lib/ateom-gvisor` to `/var/lib/ate`. Both
  sandbox classes mount it; the name predates micro-VM. The rename needs a
  two-release migration, so it is an issue, not a PR.

Behavior changes for existing workloads, to state in any PR:

- [`7b3d05cf`](https://github.com/dims/substrate/commit/7b3d05cf7354b16c0476ed68aae34b5a3bde5934): an image with a numeric `USER` used to run as root and now
  runs as that user (gid 0 when the image names no group); one with a named
  `USER` now fails to start.
- [`e0443e74`](https://github.com/dims/substrate/commit/e0443e74df7db438dadde4dd8c5c20c1bf92c33a): durable dirs are world-writable inside the actor. That is
  what Kubernetes does for an emptyDir, and only that actor's containers can
  reach the directory.
- [`aec4bd06`](https://github.com/dims/substrate/commit/aec4bd0667090aedcc8ce285c298c0bc4da26300): atelet holds one capability where the manifest dropped them
  all. ateom already holds it; the alternative is to move durable-dir cleanup
  there.
- [`ec91ff65`](https://github.com/dims/substrate/commit/ec91ff65dab1a3a674cbb0660bf9be1b178b4b86): every micro-VM container gets `no_new_privileges`; there is
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
