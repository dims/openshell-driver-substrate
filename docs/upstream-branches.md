# Upstream changes

Six changes are on the branch; a seventh, for gVisor only, lives on as the
first commit of [#1918](https://github.com/agent-substrate/substrate/pull/1918). Four PRs, ready for review, on
`agent-substrate/substrate`, none merged. State as of 2026-09-27.

## agent-substrate/substrate

**Branch:** [`lean-integration`](https://github.com/dims/substrate/tree/lean-integration) at
[`1eb9b810`](https://github.com/dims/substrate/commit/1eb9b8106aaf14394aadd940e00aba3adfa9a15e): the six PR commits below, cherry-picked in order onto `agent-substrate/substrate` main
[`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c), the merge of
[#1923](https://github.com/agent-substrate/substrate/pull/1923). Previous heads are kept as
[`archive/lean-integration-2026-09-23`](https://github.com/dims/substrate/tree/archive/lean-integration-2026-09-23) ([`0ff8b818`](https://github.com/dims/substrate/commit/0ff8b818d515036409d4ce0070f00582cb010659)),
[`archive/lean-integration-2026-09-25`](https://github.com/dims/substrate/tree/archive/lean-integration-2026-09-25) ([`8a290199`](https://github.com/dims/substrate/commit/8a290199df2c5dd7b2ecc8d5c43627182803ecab)),
[`archive/lean-integration-2026-09-26`](https://github.com/dims/substrate/tree/archive/lean-integration-2026-09-26) ([`ec91ff65`](https://github.com/dims/substrate/commit/ec91ff65dab1a3a674cbb0660bf9be1b178b4b86))
[`archive/lean-integration-2026-09-26b`](https://github.com/dims/substrate/tree/archive/lean-integration-2026-09-26b) ([`d6249cde`](https://github.com/dims/substrate/commit/d6249cde54de32ecd5fe085e67de8758c54a7972))
and [`archive/lean-integration-2026-09-27`](https://github.com/dims/substrate/tree/archive/lean-integration-2026-09-27) ([`ae03ebbb`](https://github.com/dims/substrate/commit/ae03ebbbac81311af81c7bed510d539e77e0559c), the same code with the branch's own commit messages and comments).

Each commit's necessity is verified: on 2026-09-26, at
[`ae03ebbb`](https://github.com/dims/substrate/commit/ae03ebbbac81311af81c7bed510d539e77e0559c) (the same code),
`examples/helpdesk/run.sh` passed all ten beats on a Linux host with
`/dev/kvm` with Substrate rebuilt from scratch, and a rebuild without any one
of the six commits failed at beat 1 or 2. On 2026-09-27 the rebuilt head
[`1eb9b810`](https://github.com/dims/substrate/commit/1eb9b8106aaf14394aadd940e00aba3adfa9a15e) passed the same ten beats with the
stock image, again from a scratch rebuild. The seventh
change, the rootfs mode, is not needed on micro-VM, whose guest already sees
`0755`; it lives on as the first commit of [#1918](https://github.com/agent-substrate/substrate/pull/1918), which needs it on
gVisor. One failure is still open, on both test hosts: about one run in three
stalls once at beat 10, where the supervisor logs the connection to the model
host but never the request, for 80 s. It appears with and without the six
commits, so it sits inside the micro-VM between the sandbox and the supervisor;
see the [helpdesk notes](../examples/helpdesk/docs/why-lean-integration.md#still-open).
`run.sh` fails the demo when it happens.

This repo's [`lean-zero`](https://github.com/dims/openshell-driver-substrate/tree/lean-zero) branch needs none of the six:
the sandbox image drops root itself, and Substrate is plain upstream main from
[`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c). On 2026-09-26
that passed the ten beats on both hosts with the same strict `run.sh`, subject
to the same open stall.

Every commit was found by running a real non-root workload; none is specific
to OpenShell. Before them, every actor container silently ran as root, and no
container declaring a non-root `USER` could run at all, on either sandbox class.

| Commit | Change | Upstream |
|---|---|---|
| not on the branch | `imagecache`: make the merged rootfs root searchable by non-root. `rootfs` and `upper` were `0700`; on gVisor the container's `/` takes that mode, so a non-root process could not search its own root. The micro-VM guest already saw `0755`, so the helpdesk demo does not need it. `TestSetupBundleRootfs_RootIsSearchableByNonRoot`. | [#1905](https://github.com/agent-substrate/substrate/pull/1905) closed 2026-09-26; the change is the first commit of [#1918](https://github.com/agent-substrate/substrate/pull/1918), which needs it on gVisor |
| [`0578655e`](https://github.com/dims/substrate/commit/0578655e1d6c45ab61f8f1a15cd13df9559a3c12) | `atelet`: honor a container image's own `USER`. `ocispec.Options` gains `UID`/`GID`, resolved by `resolveUser` with containerd's rule: numeric `uid[:gid]`, no group means gid 0, `root` is 0, ids are bounded to int32; a named user is an error, not a silent fall-back to root. The pause container follows the same rule. `TestResolveUser`, `TestBuild_ProcessUser`, `TestShapers_PreserveProcessUser`, `TestSpecToAgentPB_ForwardsProcessUser`. | [#1918](https://github.com/agent-substrate/substrate/pull/1918) ready for review, two commits with the rootfs change first; not to merge before [#1906](https://github.com/agent-substrate/substrate/pull/1906) and [#1910](https://github.com/agent-substrate/substrate/pull/1910) |
| [`8476f5d0`](https://github.com/dims/substrate/commit/8476f5d0e3fa1899c1386a5acfe2752c46500586) | `atelet`: make durable-dir volumes writable by non-root containers (`0777`, as Kubernetes gives an emptyDir). `TestPrepareDurableDirVolume`. | [#1906](https://github.com/agent-substrate/substrate/pull/1906) ready for review |
| [`de620517`](https://github.com/dims/substrate/commit/de62051716f8a3df34100efb23961169163aa760) | `atelet`: add `CAP_DAC_OVERRIDE` to reset dirs a non-root container wrote. Plain root cannot unlink files from a directory another uid owns, so `resetActorDirs` failed after every checkpoint of such an actor and the suspend never completed. | [#1910](https://github.com/agent-substrate/substrate/pull/1910) ready for review; its own PR after [#1906](https://github.com/agent-substrate/substrate/pull/1906), the body argues the alternatives, cleanup in ateom first among them |
| [`15d2b715`](https://github.com/dims/substrate/commit/15d2b71512d4cc62e3534be06da9cf14178cd511) | `microvm`: forward `Linux.Sysctl` to the kata agent. `TestSpecToAgentPB_ForwardsSysctl`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) ready for review |
| [`5c2a4c21`](https://github.com/dims/substrate/commit/5c2a4c21b88eb684f88c8ac4de4fc8945be6d595) | `microvm`: let a capability-free container bind a low port: `net.ipv4.ip_unprivileged_port_start=0` in `ShapeMicroVM`, parity with gVisor, whose netstack has no privileged-port check. `TestShapeMicroVM_AllowsLowPortsInTheGuest`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) ready for review |
| [`1eb9b810`](https://github.com/dims/substrate/commit/1eb9b8106aaf14394aadd940e00aba3adfa9a15e) | `microvm`: set `no_new_privileges` on every guest container, parity with `runsc --allow-suid=false`. `TestShapeMicroVM_SetsNoNewPrivileges`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) ready for review, third commit; the body says why no `allowPrivilegeEscalation` field is proposed and offers to split the commit out |

Both `ShapeMicroVM` fields stay out of the shared spec builder: `runsc restore`
compares them with the checkpoint-time spec, so a shared default would make
older gVisor snapshots unrestorable.

Related, not on the branch:

- [#1912](https://github.com/agent-substrate/substrate/pull/1912) put the ActorIdentity extension back on the
  ateom-for-actor certificate as a stopgap for the agentgateway end-to-end test lane
  ([#1922](https://github.com/agent-substrate/substrate/issues/1922)); [#1923](https://github.com/agent-substrate/substrate/pull/1923), the pin bump to an
  agentgateway build that reads the URI subject alternative name (SAN), merged on 2026-09-26 and both
  are closed. The four PRs above were rebased onto that main the same day.
- [#1911](https://github.com/agent-substrate/substrate/issues/1911) proposes
  renaming the node state root `/var/lib/ateom-gvisor` to `/var/lib/ate`. Both
  sandbox classes mount it; the name predates micro-VM. Draft
  [#1926](https://github.com/agent-substrate/substrate/pull/1926) does the rename with a
  legacy-path fallback, so one release carries both paths.

Behavior changes for existing workloads, to state in any PR:

- [`0578655e`](https://github.com/dims/substrate/commit/0578655e1d6c45ab61f8f1a15cd13df9559a3c12): an image with a numeric `USER` used to run as root and now
  runs as that user (gid 0 when the image names no group); one with a named
  `USER` now fails to start. On gVisor, a snapshot taken before this change
  from an image with a numeric `USER` does not restore after it, the golden
  snapshot included: `runsc restore` enforces that `Process.User` matches the
  checkpoint-time spec. Such templates need a new golden snapshot; micro-VM
  has no such check.
- [`8476f5d0`](https://github.com/dims/substrate/commit/8476f5d0e3fa1899c1386a5acfe2752c46500586): durable dirs are world-writable inside the actor. That is
  what Kubernetes does for an emptyDir, and only that actor's containers can
  reach the directory.
- [`de620517`](https://github.com/dims/substrate/commit/de62051716f8a3df34100efb23961169163aa760): atelet holds one capability where the manifest dropped them
  all. ateom already holds it; the alternative is to move durable-dir cleanup
  there.
- [`1eb9b810`](https://github.com/dims/substrate/commit/1eb9b8106aaf14394aadd940e00aba3adfa9a15e): every micro-VM container gets `no_new_privileges`; there is
  no field to opt out.

Not fixed: when a suspend fails after `CheckpointWorkload` has succeeded and
ateom has torn the VM down, the reconciler retries the checkpoint against a VM
that no longer exists, every 10 seconds, without end. The actor and its
template can then not be deleted (`Aborted: another operation is in progress`).

The driver's vendored `proto/ateapi.proto` matches upstream's
`pkg/proto/ateapipb/ateapi.proto` at [`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c); the six commits do not touch
it. [`../proto/README.md`](../proto/README.md) says how to refresh it.

## kata-containers

**No changes.** The guest kernel is stock.

## NVIDIA/OpenShell

**No changes carried.** The driver depends on upstream unmodified
(`openshell-core`, pinned by rev in `Cargo.toml`), and the sandbox and
supervisor binaries in the images are stock. One fix is pending upstream:
[#3745](https://github.com/NVIDIA/OpenShell/pull/3745), for a supervisor bug that drops a mediated request
read together with the synthesized `CONNECT` header; the pinned revision has
it, and the demo stalls about one run in six until it lands.
