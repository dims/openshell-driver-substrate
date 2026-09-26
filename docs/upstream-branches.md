# Upstream changes

Six changes are on the branch and a seventh, for gVisor only, is filed
alongside them: five draft PRs on `agent-substrate/substrate`, none merged.
State as of 2026-09-26.

## agent-substrate/substrate

**Branch:** https://github.com/dims/substrate/tree/lean-integration at
[`d6249cde`](https://github.com/dims/substrate/commit/d6249cde54de32ecd5fe085e67de8758c54a7972): six commits on `agent-substrate/substrate` main
[`1d7ca8ce`](https://github.com/agent-substrate/substrate/commit/1d7ca8ced056192a1801d6565251adcaab3eb0c9) plus
[#1923](https://github.com/agent-substrate/substrate/pull/1923)'s pin bump cherry-picked as [`5d2cd196`](https://github.com/dims/substrate/commit/5d2cd196fad71d43f8b908dfd2b3d8ac7b165e90), which
drops on its own when #1923 merges. Previous heads are kept as
`archive/lean-integration-2026-09-23` ([`0ff8b818`](https://github.com/dims/substrate/commit/0ff8b818d515036409d4ce0070f00582cb010659)),
`archive/lean-integration-2026-09-25` ([`8a290199`](https://github.com/dims/substrate/commit/8a290199df2c5dd7b2ecc8d5c43627182803ecab))
and `archive/lean-integration-2026-09-26` ([`ec91ff65`](https://github.com/dims/substrate/commit/ec91ff65dab1a3a674cbb0660bf9be1b178b4b86)).

The head is verified, and so is each commit's necessity: on 2026-09-26
`examples/helpdesk/run.sh` passed all ten beats on a Linux host with
`/dev/kvm` rebuilt from scratch at [`d6249cde`](https://github.com/dims/substrate/commit/d6249cde54de32ecd5fe085e67de8758c54a7972), and a rebuild without
any one of the six commits fails at beat 1 or 2. The seventh change, the
rootfs mode, is not needed on micro-VM, whose guest already sees `0755`; it
stays filed for gVisor. On a second host the same head passes beats 1 to 9
and then stalls at beat 10: the supervisor logs the connection to the model
host but never the request, for 80 s. That stall is on the OpenShell side,
appears with the seven-commit stack too, and is not understood yet.

Every commit was found by running a real non-root workload; none is specific
to OpenShell. Before them, every actor container silently ran as root, and no
container declaring a non-root `USER` could run at all, on either sandbox class.

| Commit | Change | Upstream |
|---|---|---|
| not on the branch | `imagecache`: make the merged rootfs root searchable by non-root. `rootfs` and `upper` were `0700`; on gVisor the container's `/` takes that mode, so a non-root process could not search its own root. The micro-VM guest already saw `0755`, so the helpdesk demo does not need it; it is filed for gVisor. `TestSetupBundleRootfs_RootIsSearchableByNonRoot`. | [#1905](https://github.com/agent-substrate/substrate/pull/1905) draft |
| [`2b81237b`](https://github.com/dims/substrate/commit/2b81237bc4e58e38c964e8f0834ee8daeabe2d7c) | `atelet`: honor a container image's own `USER`. `ocispec.Options` gains `UID`/`GID`, resolved by `resolveUser` with containerd's rule: numeric `uid[:gid]`, no group means gid 0, `root` is 0, ids are bounded to int32; a named user is an error, not a silent fall-back to root. The pause container follows the same rule. `TestResolveUser`, `TestBuild_ProcessUser`, `TestShapers_PreserveProcessUser`, `TestSpecToAgentPB_ForwardsProcessUser`. | [#1918](https://github.com/agent-substrate/substrate/pull/1918) draft, stacked on [#1905](https://github.com/agent-substrate/substrate/pull/1905)'s commit; not to merge before [#1906](https://github.com/agent-substrate/substrate/pull/1906) and [#1910](https://github.com/agent-substrate/substrate/pull/1910) |
| [`35298df3`](https://github.com/dims/substrate/commit/35298df30c6633fb1bd40d41ad13de3586e50478) | `atelet`: make durable-dir volumes writable by non-root containers (`0777`, as Kubernetes gives an emptyDir). `TestPrepareDurableDirVolume`. | [#1906](https://github.com/agent-substrate/substrate/pull/1906) draft |
| [`30e6ecde`](https://github.com/dims/substrate/commit/30e6ecdea70522c72f94fb3282c2d9941a9a3190) | `atelet`: add `CAP_DAC_OVERRIDE` to reset dirs a non-root container wrote. Plain root cannot unlink files from a directory another uid owns, so `resetActorDirs` failed after every checkpoint of such an actor and the suspend never completed. | [#1910](https://github.com/agent-substrate/substrate/pull/1910) draft; its own PR after [#1906](https://github.com/agent-substrate/substrate/pull/1906), the body argues the alternatives, cleanup in ateom first among them |
| [`eaaaa9fb`](https://github.com/dims/substrate/commit/eaaaa9fb56a1ed0c4fd60e125b9f3a6d4e65fd95) | `microvm`: forward `Linux.Sysctl` to the kata agent. `TestSpecToAgentPB_ForwardsSysctl`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) draft |
| [`8427dafb`](https://github.com/dims/substrate/commit/8427dafbcb8d7890d3fae655e2d051dddbe59142) | `microvm`: let a capability-free container bind a low port: `net.ipv4.ip_unprivileged_port_start=0` in `ShapeMicroVM`, parity with gVisor, whose netstack has no privileged-port check. `TestShapeMicroVM_AllowsLowPortsInTheGuest`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) draft |
| [`d6249cde`](https://github.com/dims/substrate/commit/d6249cde54de32ecd5fe085e67de8758c54a7972) | `microvm`: set `no_new_privileges` on every guest container, parity with `runsc --allow-suid=false`. `TestShapeMicroVM_SetsNoNewPrivileges`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) draft, third commit; the body says why no `allowPrivilegeEscalation` field is proposed and offers to split the commit out |

Both `ShapeMicroVM` fields stay out of the shared spec builder: `runsc restore`
compares them with the checkpoint-time spec, so a shared default would make
older gVisor snapshots unrestorable.

Related, not on the branch:

- [#1912](https://github.com/agent-substrate/substrate/pull/1912) (draft,
  `pr/ateom-cert-actor-identity`) puts the ActorIdentity extension back on the
  ateom-for-actor certificate. Upstream [#1809](https://github.com/agent-substrate/substrate/pull/1809) removed it, and the pinned
  agentgateway image still resolves the actor from it, so the agentgateway e2e
  lane fails on main and on every PR above. The lane is not a required check.
  Tracked upstream as [#1922](https://github.com/agent-substrate/substrate/issues/1922).
  The permanent fix is on the agentgateway side:
  [agentgateway#3677](https://github.com/agentgateway/agentgateway/pull/3677)
  (merged 2026-09-26) reads the URI SAN, and
  [#1923](https://github.com/agent-substrate/substrate/pull/1923) moves
  Substrate's pin in
  `manifests/ate-install/components/agentgateway/kustomization.yaml` to an
  image that carries it. Once that merges, the lane is green without
  [#1912](https://github.com/agent-substrate/substrate/pull/1912), and the PR
  branches above get rebased onto main so their runs pick it up.
- [#1911](https://github.com/agent-substrate/substrate/issues/1911) proposes
  renaming the node state root `/var/lib/ateom-gvisor` to `/var/lib/ate`. Both
  sandbox classes mount it; the name predates micro-VM. The rename needs a
  two-release migration, so it is an issue, not a PR.

Behavior changes for existing workloads, to state in any PR:

- [`2b81237b`](https://github.com/dims/substrate/commit/2b81237bc4e58e38c964e8f0834ee8daeabe2d7c): an image with a numeric `USER` used to run as root and now
  runs as that user (gid 0 when the image names no group); one with a named
  `USER` now fails to start. On gVisor, a snapshot taken before this change
  from an image with a numeric `USER` does not restore after it, the golden
  snapshot included: `runsc restore` enforces that `Process.User` matches the
  checkpoint-time spec. Such templates need a new golden snapshot; micro-VM
  has no such check.
- [`35298df3`](https://github.com/dims/substrate/commit/35298df30c6633fb1bd40d41ad13de3586e50478): durable dirs are world-writable inside the actor. That is
  what Kubernetes does for an emptyDir, and only that actor's containers can
  reach the directory.
- [`30e6ecde`](https://github.com/dims/substrate/commit/30e6ecdea70522c72f94fb3282c2d9941a9a3190): atelet holds one capability where the manifest dropped them
  all. ateom already holds it; the alternative is to move durable-dir cleanup
  there.
- [`d6249cde`](https://github.com/dims/substrate/commit/d6249cde54de32ecd5fe085e67de8758c54a7972): every micro-VM container gets `no_new_privileges`; there is
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
