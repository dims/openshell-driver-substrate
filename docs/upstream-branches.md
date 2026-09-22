# Upstream changes

Nothing here is merged upstream yet. Until PRs exist, these branches are the
only copies.

## agent-substrate/substrate

**Branch:** https://github.com/dims/substrate/tree/lean-integration
(fork of `agent-substrate/substrate`, branched from `92a84388`)

Three bugs and two hardening changes. Every one was found by running a real
non-root workload; none is specific to OpenShell. Before them, every actor
container silently ran as root, and no container declaring a non-root `USER`
could run at all, on either sandbox class.

| Commit | Change |
|---|---|
| `cf699047` | `atelet`: honor a container image's own `USER` when building its OCI spec. `ocispec.Options` gains `UID`/`GID`, resolved by `resolveUser`. Numeric `uid[:gid]` only; a named user is a hard error, not a silent fall-back to root. `TestResolveUser`. |
| `cbb8405e` | `atelet`: keep the pause container root regardless of its image's `USER`. Fallout of the above — `registry.k8s.io/pause:3.10.2` declares `USER 65535:65535` and gVisor's sandbox init cannot boot under it. |
| `73fe6062` | `imagecache`: make the merged rootfs root searchable by non-root containers. `rootfs`/`upper` were `0700`, so a non-root process could not search its own `/`. `TestSetupBundleRootfs_RootIsSearchableByNonRoot`. |
| `4fc5d550` | `ocispec`: set `no_new_privileges` on actor containers. |
| `f63c6ec0` | `atelet`: make durable-dir volumes writable by non-root containers (`0777`, as Kubernetes gives an emptyDir). |
| `33397540` | `microvm`: forward `Linux.Sysctl` to the kata agent, and set `net.ipv4.ip_unprivileged_port_start=0` so a capability-free container can bind the DNS relay port. |

`975d1e72` on the same branch only tightens comments.

Two of these change behaviour for existing workloads and should be called out
in any PR:

- `4fc5d550` is unconditional and there is no `SecurityContext` field to opt
  out. Under gVisor it is a no-op (runsc already runs with `--allow-suid`
  disabled); on micro-VM the kata agent enforces it, so a workload relying on
  setuid inside its sandbox would change behaviour.
- `f63c6ec0` makes durable-dir volumes world-writable. That is what Kubernetes
  does for an emptyDir, and the volume is per-actor inside a sandbox, but it is
  a deliberate choice worth stating.

## kata-containers

**Branch:** https://github.com/dims/kata-containers/tree/enable-cross-memory-attach
(fork of `kata-containers/kata-containers`, branched from `main`)

One config fragment plus the required `kata_config_version` bump:

```
CONFIG_CROSS_MEMORY_ATTACH=y
```

Stock kata ships this off, so `process_vm_readv` returns `ENOSYS` in the guest.
OpenShell's `task_memory::read_exact` treats `ENOSYS` the same as a denial and
falls back to `/proc/{tid}/mem`, which a non-root task cannot open once
`qualify_runtime()` has made it nondumpable. The sandbox then fails its
seccomp-notification gate with an `EACCES` that points at the wrong thing.

Sits alongside kata's existing `landlock.conf`, which is the precedent for
enabling a kernel option that a guest workload needs.

## NVIDIA/OpenShell

**No changes.** The driver depends on upstream unmodified (`openshell-core`,
pinned by rev in `Cargo.toml`), and the sandbox and supervisor binaries in the
images are stock. Nothing found in this work needs an OpenShell patch to run on
the micro-VM backend.

The one thing that *would* need upstream is gVisor support, and it is not a
small patch: `openshell-sandbox` hard-requires Landlock ABI ≥ 3 and gVisor
implements no Landlock at all. That needs a degraded qualification mode in
which an outer sandbox is the enforcing boundary — the argument
[#1549](https://github.com/NVIDIA/OpenShell/pull/1549) made, which NVIDIA
closed unmerged. See the README for the history.

## google/gvisor

**No changes.** gVisor is used stock, from the upstream nightly the Substrate
`SandboxConfig` pins. It cannot host `openshell-sandbox` (no Landlock), but
that is a missing feature rather than something a patch here fixes.
