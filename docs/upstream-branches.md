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

**Branch:** https://github.com/dims/OpenShell/tree/gvisor-backend
(fork of `NVIDIA/OpenShell`, branched from `df88bedb3`)

`OPENSHELL_ISOLATION_MODE=gvisor` tells the sandbox it runs under an outer
sandbox that implements neither Landlock nor seccomp user notification. The
probes for both are skipped, the workload launcher starts without a listener,
and the DNS relay runs on its own. The qualification reports the absence
instead of asserting success, so `landlock_allow_deny`, `socket_virtualization`,
`retained_socket_operation` and `proc_fd_identity` all read false. Egress is
unmediated in this mode; a caller must supply confinement by other means.

A second commit stops the DNS relay probe dying on a missing
`/proc/sys/net/ipv4/ip_unprivileged_port_start`. gVisor's procfs has no such
file, and the bind that follows is the real test.

With both, `openshell-sandbox capability-probe` returns `"qualified": true`
under `SANDBOX_CLASS_GVISOR` on Substrate.

Default behaviour is unchanged and an unrecognized mode is an error.

**Micro-VM needs none of this.** There the binaries are stock and
`openshell-core` is a plain git dependency pinned by rev.

## google/gvisor

**No changes.** gVisor is used stock, from the upstream nightly the Substrate
`SandboxConfig` pins.

The sentry implements no `SECCOMP_RET_USER_NOTIF`, accepts only
`SECCOMP_FILTER_FLAG_TSYNC`, and implements no Landlock. That is why the
sandbox needs its own gVisor mode rather than the native-Linux one: neither
mechanism can be built there, so the egress broker has to be replaced rather
than ported.

Two smaller gVisor gaps found on the way, neither blocking:

- `SO_ORIGINAL_DST` returns `ENOTCONN` for a dual-stack listener, because
  `tcp/endpoint.go:2300` passes the socket's protocol to conntrack rather than
  the connection's. Listening on `tcp4` avoids it; the fix is four lines.
- procfs has no `net/ipv4/ip_unprivileged_port_start`.
