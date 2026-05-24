---
authors:
  - "@dims"
state: draft
links:
  - (none yet — companion to chore/gvisor-degraded-netns branch)
---

# RFC 0005 - Sandbox Degraded Mode

## Summary

Introduce first-class "degraded mode" support for the OpenShell sandbox
supervisor so it can boot under outer-sandbox runtimes (gVisor, hardened
containers, certain CI runners) that block the supervisor's privileged
bootstrap syscalls. Today the supervisor fails hard if `unshare(CLONE_NEWNET)`
or `seccomp(SECCOMP_SET_MODE_FILTER)` return error, and `drop_privileges`
errors with `EPERM` when the policy's `run_as_user` matches the current
effective UID but `CAP_SETGID` is dropped. This RFC proposes an opt-in
policy-level toggle that turns each of those hard failures into logged,
best-effort degradation, mirroring the existing `landlock.compatibility`
contract. The four surgical patches on `chore/gvisor-degraded-netns` are
the spike that proves the design works; this RFC is the proposal to land
the same behavior cleanly.

## Motivation

OpenShell's supervisor assumes a Linux host that grants it
`CAP_NET_ADMIN`, `CAP_SYS_ADMIN`, `CAP_SETGID`, `CAP_SETUID`, and the
ability to install seccomp filters. That assumption is valid on bare-metal
hosts and standard container runtimes, but is broken under:

- **gVisor (runsc)** — the Substrate runtime, and increasingly other
  hosting platforms. gVisor implements the Linux ABI in userspace and
  intentionally refuses `unshare(CLONE_NEWNET)`, `seccomp(SET_MODE_FILTER)`,
  and the Landlock syscalls. It is itself a complete syscall-filtering
  security boundary, so the supervisor's in-process syscall hardening is
  redundant defense-in-depth in this configuration.
- **Hardened multi-tenant containers** — Kubernetes pods running with
  `securityContext.capabilities.drop: [ALL]` and no AppArmor/SELinux
  exemption hit the same `EPERM`s.
- **CI runners** — some hosted runners (GitHub Actions, GitLab,
  CircleCI) restrict `CAP_NET_ADMIN` even for "Docker-in-Docker" jobs.

In all of these cases the *enforcing* security boundary is the outer
sandbox, not the supervisor. The supervisor's role is to be policy-aware
and observable (loopback proxy, OCSF audit, policy-guided egress
decisions) — none of which depends on the syscalls that are blocked.

Today the only options are:

1. Run the supervisor on a host that grants the privileged caps.
   Excludes gVisor entirely, even though gVisor is a sound enclosing
   sandbox.
2. Fork OpenShell and apply the four surgical patches from
   `chore/gvisor-degraded-netns` (this branch). That is exactly what the
   Agent Substrate integration does today, and it works, but the patches
   are conditionals scattered across three files that the wider
   OpenShell community has no way to discover, opt into, or reason about.

This RFC proposes the upstream-clean version of those patches: one
explicit policy contract for each degradable subsystem.

## Non-goals

- Replacing `gateway-driver-{kubernetes,podman,docker,vm}` with an
  Agent-Substrate driver — that is a separate proposal (the `openshell-driver-substrate` M3 work).
- Auto-detecting the outer sandbox at runtime. The operator opts into
  degraded mode explicitly in policy. Heuristic detection (e.g. reading
  `/proc/1/cgroup` or probing `unshare` at startup) would silently
  weaken security on misconfigured hosts.
- Changing the semantics of the existing strict mode. Default behavior
  for every existing call site stays identical.
- Re-deriving Landlock's `compatibility: best_effort` — that contract
  already exists and is the model this RFC follows for the new subsystems.

## Proposal

### Policy schema

Extend `SandboxPolicy` with two new fields, parallel to
`landlock.compatibility`:

```yaml
network:
  mode: proxy
  proxy:
    # New: explicitly opts into proxy-env-only enforcement when the
    # kernel refuses network-namespace creation. Default `false` keeps
    # the supervisor's current hard-fail behavior under reduced caps.
    bypass_proof_required: false   # NEW
    http_addr: 127.0.0.1:3128

seccomp:
  # New: best_effort logs and continues when seccomp install returns
  # EINVAL/ENOSYS; hard_requirement (default) keeps the current
  # hard-fail behavior. Mirrors `landlock.compatibility`.
  compatibility: hard_requirement   # NEW; alternative: best_effort
```

`process.run_as_user`/`run_as_group` is unchanged at the policy layer
but the implementation gains an idempotent fast-path (see "Code changes"
below).

### Code changes

Replace the four conditionals in `chore/gvisor-degraded-netns` with:

1. **Network namespace**. In `lib.rs::run_sandbox`, the `NetworkNamespace::create()` failure arm reads `policy.network.proxy.bypass_proof_required`. If `true` (the default), return the existing fatal error. If `false`, emit a `tracing::warn!` + OCSF detection finding, log the loss of bypass-proof egress, and continue with `netns = None`. The downstream cascade (`ProxyHandle::start_with_bind_addr`, `ProcessHandle::spawn` HTTP_PROXY injection, `bypass_monitor::spawn`) already handles `None` correctly.

2. **Supervisor seccomp prelude**. In `lib.rs::run_sandbox`, wrap the `apply_supervisor_startup_hardening()` call. On `Err`, branch on `policy.seccomp.compatibility`: `hard_requirement` propagates the error (current behavior), `best_effort` emits a warning + OCSF finding and continues. Same pattern Landlock uses.

3. **Workload seccomp filter**. In `sandbox/linux/mod.rs::enforce` (which runs in the child's `pre_exec`), wrap `seccomp::apply(&prepared.policy)` and branch on the same `policy.seccomp.compatibility`. `best_effort` swallows the error; `hard_requirement` propagates.

4. **Idempotent `drop_privileges`**. In `process.rs::drop_privileges`, after resolving `user`/`group`, short-circuit when `geteuid() == user.uid && getegid() == group.gid`. This is a pure correctness fix and is upstreamable independent of the rest of this RFC — the function should not call `initgroups`/`setgid`/`setuid` when the call is a no-op.

The OCSF events emitted under degraded mode follow the same pattern as the existing Landlock `"landlock-unavailable"` finding: `DetectionFinding`, `severity: High`, `is_alert: true`, with a descriptive message that names the missing capability and the resulting loss of enforcement.

### Defaults

All new policy fields default to the strict value. Existing policy
files, the restrictive built-in default, and all current behavior
remain unchanged. The only way to enter degraded mode is for the
operator to explicitly set `bypass_proof_required: false` and/or
`seccomp.compatibility: best_effort` in their policy.

### CLI / config surface

No new CLI flags. The supervisor reads policy from `--policy-rules` +
`--policy-data` (standalone mode) or from the gateway (managed mode), so
the new fields land in the YAML data file or the gateway's policy
record.

## Security

Degraded mode is genuinely less secure than strict mode. Specifically:

- **Network**: without a netns, the supervisor cannot enforce
  bypass-proof egress. A non-cooperating workload can ignore
  `HTTP_PROXY` and reach any address the outer sandbox's network policy
  permits. The OCSF finding explicitly warns about this. Operators who
  enable `bypass_proof_required: false` are asserting that the outer
  sandbox provides equivalent (or sufficient) egress isolation.
- **Seccomp**: without the per-policy filter, syscall-based escape
  techniques (`kexec_load`, `unshare(CLONE_NEWUSER)`, fileless execution
  via `execveat AT_EMPTY_PATH`) are not blocked by the supervisor.
  Under gVisor these are blocked by gVisor itself; under reduced-cap
  containers, AppArmor/SELinux must compensate. The OCSF finding
  documents which filter would have been installed.
- **drop_privileges idempotent path**: no security impact. The
  short-circuit only fires when the policy already matches the current
  identity, which is the same outcome the existing code is trying to
  achieve.

The RFC explicitly does not weaken any default. Operators must declare
the trade-off in policy, and the OCSF audit trail records the choice.

## Migration

- v1 of this RFC: add the two policy fields with strict defaults.
  Existing deployments see no behavior change.
- v2: announce degraded mode in release notes; document the recommended
  outer-sandbox profiles (gVisor, reduced-cap containers) that warrant
  enabling it.
- v3 (optional): consider a small `openshell-doctor` check that surfaces
  "the host kernel will reject your current strict policy" so operators
  can decide deliberately rather than discover at first sandbox start.

The `chore/gvisor-degraded-netns` branch is preserved as the spike that
proves the design. It will be retired once this RFC lands.

## Alternatives considered

- **Detect outer sandbox at runtime and degrade automatically**.
  Rejected because misconfigured hosts (e.g. `runsc` is installed but
  the workload is on `runc`) would silently weaken without operator
  consent.
- **A single `policy.degraded_mode: true` flag**. Considered but
  rejected: the three subsystems degrade independently, and bundling
  them obscures which protection the operator is actually giving up.
  The Landlock side already has its own `compatibility` knob; we should
  match that granularity.
- **Add a new `NetworkMode::ProxyDegraded` enum variant**. Cleaner
  separation in the type system but multiplies call sites (the proxy
  setup branches at four places). A boolean opt-in on the existing
  `Proxy` variant keeps the diff small and the policy shape stable.
- **Leave it as `chore/gvisor-degraded-netns`**. Works for the Agent
  Substrate integration but does not generalize. Other OpenShell users
  (CI matrix, hardened containers) hit the same problem and have no
  visible path.

## Open questions

- Should we emit OCSF events on every sandbox boot in degraded mode, or
  only the first per process? Landlock's current implementation emits
  once per process via `Once::call_once`; the new findings should
  probably match.
- Does `policy.seccomp.compatibility` belong inside the existing
  `policy.landlock` struct (since they're both `compatibility`-keyed)
  or as a sibling `policy.seccomp` block? Sibling block is consistent
  with the existing top-level subsystem layout but adds one more
  policy section.
- How should the gateway surface degraded mode to the user/admin? The
  policy record is gateway-managed; the gateway could refuse to mint a
  token if degraded mode is requested but the operator has not
  acknowledged the trade-off (`--allow-degraded` flag on the gateway?).
- Should there be a per-subsystem outcome field on the response from
  `openshell-doctor` so CI can assert "degraded mode is or is not in
  effect for sandbox X"?

## Implementation status

The spike branch `chore/gvisor-degraded-netns` proves all four
subsystems can be degraded independently and the supervisor reaches
steady state inside Substrate's gVisor actor (verified
2026-05-22 via `examples/agent-substrate-demo/dry-run.sh`). The
remaining work to land this RFC is mechanical: replace each unconditional
fall-through with the policy-driven branch and add tests that cover both
strict and best_effort paths for each subsystem.
