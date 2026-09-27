# Upstream changes

What this repository needs from its upstreams beyond their `main` branches,
where each change stands, and what to do when it lands.

## agent-substrate/substrate

Six commits are needed. They are open as four PRs on
`agent-substrate/substrate`, none merged: [#1904](https://github.com/agent-substrate/substrate/pull/1904), [#1906](https://github.com/agent-substrate/substrate/pull/1906), [#1910](https://github.com/agent-substrate/substrate/pull/1910)
and [#1918](https://github.com/agent-substrate/substrate/pull/1918). Until they merge, the demo runs against [`lean-integration`](https://github.com/dims/substrate/tree/lean-integration) on
`dims/substrate`, head [`1eb9b810`](https://github.com/dims/substrate/commit/1eb9b8106aaf14394aadd940e00aba3adfa9a15e): the six PR commits, cherry-picked in order
onto upstream main [`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c). The branch has no commits of its own. When a PR
changes, the branch is re-picked and the pin in step 1 of the root README moves
with it.

| Commit | Change | PR |
|---|---|---|
| [`0578655e`](https://github.com/dims/substrate/commit/0578655e1d6c45ab61f8f1a15cd13df9559a3c12) | `atelet`: honor a container image's own `USER`. `ocispec.Options` gains `UID`/`GID`, resolved by `resolveUser` with containerd's rule: numeric `uid[:gid]`, no group means gid 0, `root` is 0, ids are bounded to int32; a named user is an error, not a silent fall-back to root. The pause container follows the same rule. | [#1918](https://github.com/agent-substrate/substrate/pull/1918), second commit. Merge after [#1906](https://github.com/agent-substrate/substrate/pull/1906) and [#1910](https://github.com/agent-substrate/substrate/pull/1910): a non-root workload needs both. |
| [`8476f5d0`](https://github.com/dims/substrate/commit/8476f5d0e3fa1899c1386a5acfe2752c46500586) | `atelet`: make durable-dir volumes writable by non-root containers (`0777`, as Kubernetes gives an emptyDir). | [#1906](https://github.com/agent-substrate/substrate/pull/1906) |
| [`de620517`](https://github.com/dims/substrate/commit/de62051716f8a3df34100efb23961169163aa760) | `atelet`: add `CAP_DAC_OVERRIDE` to reset dirs a non-root container wrote. Plain root cannot unlink files from a directory another uid owns, so `resetActorDirs` fails after every checkpoint of such an actor and the suspend never completes. | [#1910](https://github.com/agent-substrate/substrate/pull/1910). The body argues the alternatives, cleanup in ateom first among them. |
| [`15d2b715`](https://github.com/dims/substrate/commit/15d2b71512d4cc62e3534be06da9cf14178cd511) | `microvm`: forward `Linux.Sysctl` to the kata agent. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) |
| [`5c2a4c21`](https://github.com/dims/substrate/commit/5c2a4c21b88eb684f88c8ac4de4fc8945be6d595) | `microvm`: let a capability-free container bind a low port: `net.ipv4.ip_unprivileged_port_start=0` in `ShapeMicroVM`, parity with gVisor, whose netstack has no privileged-port check. | [#1904](https://github.com/agent-substrate/substrate/pull/1904) |
| [`1eb9b810`](https://github.com/dims/substrate/commit/1eb9b8106aaf14394aadd940e00aba3adfa9a15e) | `microvm`: set `no_new_privileges` on every guest container, parity with `runsc --allow-suid=false`. | [#1904](https://github.com/agent-substrate/substrate/pull/1904), third commit. The body says why no `allowPrivilegeEscalation` field is proposed and offers to split the commit out. |

Every commit was found by running a real non-root workload; none is specific
to OpenShell. Without them, every actor container silently runs as root, and no
container declaring a non-root `USER` can run at all, on either sandbox class.
Each of the six is necessary for the demo: without `CAP_DAC_OVERRIDE` the
golden snapshot never gets its tag, and without any other the sandbox never
starts. [`why-lean-integration.md`](../examples/helpdesk/docs/why-lean-integration.md)
maps each commit to the OpenShell gate it clears.

A seventh change is not on the branch. `imagecache` leaves the merged rootfs
root at `0700`; on gVisor the container's `/` takes that mode, so a non-root
process cannot search its own root. The micro-VM guest already sees `0755`, so
the demo does not need it. It is the first commit of [#1918](https://github.com/agent-substrate/substrate/pull/1918), which needs it
on gVisor.

Both `ShapeMicroVM` fields stay out of the shared spec builder: `runsc restore`
compares them with the checkpoint-time spec, so a shared default would make
older gVisor snapshots unrestorable.

This repository's [`lean-zero`](https://github.com/dims/openshell-driver-substrate/tree/lean-zero) branch runs the same demo against plain upstream main
[`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c): the sandbox image drops root itself and needs none of the six.

### Behavior changes for existing workloads

Each PR states its own:

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

### After they merge

Point step 1 of the root README at upstream main and retire [`lean-integration`](https://github.com/dims/substrate/tree/lean-integration). The vendored
[`proto/ateapi.proto`](../proto/ateapi.proto) matches upstream at the commit in
[`proto/upstream-rev`](../proto/upstream-rev), which CI checks, and the six
commits do not touch it; [`proto/README.md`](../proto/README.md) says how to
refresh it when upstream changes the file.

### Related

- Not fixed anywhere: when a suspend fails after `CheckpointWorkload` has
  succeeded and ateom has torn the VM down, the reconciler retries the
  checkpoint against a VM that no longer exists, every 10 seconds, without
  end. The actor and its template can then not be deleted (`Aborted: another
  operation is in progress`).
- [#1911](https://github.com/agent-substrate/substrate/issues/1911) proposes renaming the node state root
  `/var/lib/ateom-gvisor` to `/var/lib/ate`; both sandbox classes mount it,
  and the name predates micro-VM. Draft [#1926](https://github.com/agent-substrate/substrate/pull/1926) does the rename with a
  legacy-path fallback, so one release carries both paths.

## kata-containers

No changes. The guest kernel is stock.

## NVIDIA/OpenShell

No changes carried. The driver depends on upstream unmodified
(`openshell-core`, pinned by rev in `Cargo.toml`), and the sandbox and
supervisor binaries in the images are stock.

One fix is pending upstream: [#3745](https://github.com/NVIDIA/OpenShell/pull/3745),
for a supervisor bug that drops a mediated request read together with the
synthesized `CONNECT` header. The pinned revision has the bug, so the demo
stalls about one run in six; `run.sh` reports it. When the fix is in a
release, move the pin in `Cargo.toml` past it and rebuild the images (root
README, step 3). `harness/bootstrap-gen` follows the pin, so budget for it.
