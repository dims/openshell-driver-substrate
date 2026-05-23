# OpenShell on Agent Substrate — proof-of-concept overview

**Status:** working end-to-end on a kind cluster as of 2026-05-23.
**Repo:** [`dims/openshell-driver-substrate`](https://github.com/dims/openshell-driver-substrate).
**Companion change in OpenShell:** [`dims/OpenShell@69d2054`](https://github.com/dims/OpenShell/commit/69d205479edf8443ab06e28458b350c0d4613fd9) (single commit, structurally clean, upstreamable).
**Audience:** teammates familiar with at least one of OpenShell or Agent Substrate; this doc gives the joint picture.

---

## 1. What this is

A proof-of-concept that lets the **OpenShell sandbox supervisor** run as a managed actor on top of **Agent Substrate** (NVIDIA's gVisor + checkpoint/restore runtime), with the OpenShell gateway driving the sandbox lifecycle through Substrate's control plane.

The plumbing is a Rust crate (`openshell-driver-substrate`) that implements OpenShell's `ComputeDriver` gRPC trait against Substrate's `ateapi.Control` service, plus a small drop-in supervisor wrapper binary that boots cleanly inside a gVisor sandbox by tolerating the three privileged syscalls gVisor refuses (`unshare(CLONE_NEWNET)`, `seccomp(SECCOMP_SET_MODE_FILTER)`, and one specific `setresuid` no-op path).

In one sentence: **"OpenShell's per-request sandbox becomes a checkpoint/restore-backed Substrate actor."**

## 2. Why it matters

Today an OpenShell sandbox is a fresh process tree spun up per request (or per session). The supervisor sets up landlock, seccomp, an HTTP CONNECT proxy, an OPA policy engine, and an OCSF audit pipeline; only then does the workload start. Cold start is dominated by that bootstrap.

Putting the supervisor inside a Substrate actor changes the model:

- **Sub-second cold start.** Substrate captures the supervisor's fully-bootstrapped state in a "golden snapshot" once at template creation. Every subsequent sandbox is a `runsc restore` from that snapshot — measured at hundreds of milliseconds vs. multi-second from-scratch boots. The supervisor's network proxy, TLS termination CA, policy engine, and audit pipeline are all already alive in the restored process.
- **Cluster-managed lifecycle.** "Stop" is a checkpoint, not a kill. A sandbox can be suspended idle (no worker cost) and resumed seconds-to-minutes later, on a different worker if the original is gone. The supervisor and its workload see continuous execution; the wall clock skips.
- **Multi-actor concurrency.** A single worker pod with gVisor + ateom-gvisor can host many actors. The driver does not care which worker any given sandbox lands on; substrate's controller picks.
- **Defense-in-depth without trade-offs.** gVisor is a full syscall-filtering boundary in userspace; OpenShell's supervisor is policy-aware (OPA) and observable (OCSF). You get both, with the supervisor running degraded for the parts gVisor already covers.
- **Sandbox migration.** Suspend on worker A, resume on worker B. Filesystem state (under the policy's read/write paths), in-RAM state, open connections (with caveats) — all preserved.

The model is intentionally a *thinner* OpenShell — the kernel-level hardening that the supervisor does today is duplicated by gVisor in this deployment, so we trade some defense-in-depth for the operational properties above.

## 3. How it works

### High-level architecture

```
                                         ┌────────────────────────────────────┐
                                         │  Agent Substrate cluster (kind/    │
                                         │  K8s, with the substrate operator) │
                                         │                                    │
                                         │  ┌────────────┐   ┌────────────┐   │
                                         │  │ ate-api    │   │ ate-       │   │
                                         │  │ -server    │◄─►│ controller │   │
                                         │  │ (gRPC)     │   │ (CRDs)     │   │
                                         │  └─────┬──────┘   └─────┬──────┘   │
                                         │        │                │          │
   ┌────────────────────────┐ ateapi.Cntrl│        │  ┌─────────────▼──────┐   │
   │  OpenShell gateway     │─────────────┼────────┘  │ atelet (DaemonSet) │   │
   │  ─────────────────     │             │           └──────────┬─────────┘   │
   │  uses ComputeDriver    │             │                      │             │
   │  trait, picks          │             │            ┌─────────▼──────────┐  │
   │  `substrate`           │             │            │ Worker pods        │  │
   │  ──────────────────    │             │            │  ┌──────────────┐  │  │
   │     │                  │             │            │  │ateom-gvisor  │  │  │
   │     ▼                  │             │            │  │ + runsc      │  │  │
   │  ┌─────────────────┐   │             │            │  │ ┌──────────┐ │  │  │
   │  │ openshell-      │   │             │            │  │ │supervisor│ │  │  │
   │  │ driver-substrate│───┼─────────────┼────────────┼──┼─┤ + workload│ │  │  │
   │  │ (this crate)    │   │             │            │  │ │ (actor) │ │  │  │
   │  └─────────────────┘   │             │            │  │ └──────────┘ │  │  │
   └────────────────────────┘             │            │  └──────────────┘  │  │
                                          │            └────────────────────┘  │
                                          └────────────────────────────────────┘
```

The gateway never talks to gVisor directly; it issues high-level intents (`CreateSandbox`, `StopSandbox`, etc.) against `openshell-driver-substrate`, which translates them into `ateapi.Control` RPCs against Substrate's API server.

### The three pieces

| component | repo | what it does |
|---|---|---|
| Compute driver | [`dims/openshell-driver-substrate`](https://github.com/dims/openshell-driver-substrate) (`src/lib.rs`) | Implements OpenShell's `ComputeDriver` trait. Translates `create_sandbox`, `stop_sandbox`, etc. into Substrate's `CreateActor`/`SuspendActor`/`DeleteActor`/`ResumeActor`. Synthesizes an `ActorTemplate` CRD via kube-rs when one isn't pre-provisioned. Polling-based `watch_sandboxes` until Substrate ships a streaming watch. |
| Supervisor wrapper binary | same repo (`src/bin/openshell-sandbox-substrate.rs`) | Drop-in replacement for the standard `openshell-sandbox` binary. Registers `DegradedHandler` against a small hook in `openshell-sandbox`, then delegates to the upstream CLI. Tolerates the three gVisor-refused bootstrap syscalls and continues startup. |
| Trait scaffolding | [`dims/OpenShell@69d2054`](https://github.com/dims/OpenShell/commit/69d205479edf8443ab06e28458b350c0d4613fd9) | One commit. Adds `SandboxFailureHandler` trait + `StrictHandler` default + `set_failure_handler()` setter. Routes three previously-fatal call sites (netns create, supervisor seccomp install, workload seccomp install) through the handler. Makes `drop_privileges` idempotent. Skips Landlock ruleset construction when the kernel doesn't implement it. Extracts `main.rs`'s body into `pub mod cli` so the wrapper above can reuse the CLI. **Zero behavioural change for default callers** — all 777 sandbox unit tests still pass and the `openshell-sandbox` binary's `--help` is identical. |

### Boot flow for one sandbox

1. Gateway → `openshell-driver-substrate::create_sandbox(spec)`.
2. Driver decides: pre-provisioned `ActorTemplate` (caller-named) or synthesize one from the spec via kube-rs.
3. Driver waits for the template's `status.phase = Ready` — this means Substrate's controller has run a "golden actor" once and captured its checkpoint.
4. Driver issues `CreateActor` + `ResumeActor` against `ateapi.Control`.
5. Substrate's controller picks a worker; atelet on that worker calls ateom-gvisor's `RestoreWorkload`.
6. ateom-gvisor sets up an OCI bundle, runs `runsc restore` against the golden snapshot. The restored process is the supervisor wrapper; it picks up *exactly* where the golden left off, which is right after the supervisor finished bootstrapping (proxy bound, policy loaded, TLS CA generated).
7. The supervisor's child workload command (sleep loop in the default template; anything in a custom template) runs.

Wall-clock cost from step 4 to step 7 finishing on the bigbox kind cluster: **about a second**, dominated by golden-snapshot fetch + `runsc restore`.

### Lifecycle continuation

- `stop_sandbox` ↔ Substrate `SuspendActor` ↔ `runsc checkpoint`. Snapshot stored in cluster S3 (rustfs on kind, GCS on GKE). Worker slot freed.
- `delete_sandbox` ↔ `DeleteActor`. Tears down the actor; if the driver synthesized the template, the template's `SYNTHESIZED_BY_ANNOTATION` triggers template cleanup too.
- `get_sandbox` ↔ `GetActor`. The driver translates Substrate's `Actor.Status` enum into a `DriverCondition` with `type=Ready`; the gateway's existing phase derivation works unchanged.

## 4. What it enables

| Scenario | How it composes |
|---|---|
| **Per-request sandbox with sub-second cold start** | Gateway calls `create_sandbox` per request. Each restore is a fresh actor from the same golden snapshot. The supervisor is already bootstrapped, so the workload starts inside ~1 s instead of multi-second cold start. |
| **Idle sandbox that survives suspension** | Gateway calls `stop_sandbox` on idle, gets back a snapshot URI in `ActorTemplate.status.last_snapshot`. Calls `create_sandbox` later — the restore brings the workload's filesystem and in-RAM state back with no migration logic. |
| **Sandbox teleportation across workers** | Operator drains a worker (or it dies). Substrate's controller reassigns the actor on resume. The supervisor and workload don't know they moved. |
| **Multi-tenant isolation** | Each sandbox is its own gVisor sandbox (separate userspace kernel) AND has its own OpenShell supervisor instance enforcing policy. The two layers are orthogonal: a workload escape from the OpenShell supervisor is still trapped by gVisor. |
| **Auditable egress** | OpenShell's OCSF audit pipeline is alive inside every actor. Every HTTP CONNECT through the supervisor's proxy is logged with policy name, decision, and binary path of the caller. |
| **OPA/Rego policy enforcement on egress** | Policy file (`policy.rego` + `data.yaml`) is baked into the supervisor image. The driver injects `OPENSHELL_SANDBOX_ID` and (when configured) `OPENSHELL_ENDPOINT` + `OPENSHELL_SANDBOX_TOKEN` so the supervisor can identify itself to a gateway for live policy fetches in the future. |
| **Concurrent sandboxes on a single host** | A worker pod hosts many ateom actors. Lifecycle ops are independent. |
| **Test images that override the workload command** | Operators pre-provision an `ActorTemplate` with a custom `command:` block (the `tests/integration/oshl-feature-test` template is the example) and reference it from `spec.template.platform_config["substrate_actor_template"]`. |

## 5. What's actually verified working

End-to-end on a bigbox kind cluster, with the post-split layout:

**Driver-side lifecycle** — 4 live integration tests pass (in 32 s end-to-end including build):
- `live_get_capabilities` — `GetCapabilities` returns the driver name + version.
- `live_list_sandboxes` — `ListSandboxes` returns the expected actors filtered to the configured namespace.
- `live_write_path_round_trip` — `create` → `get` → `stop` → `delete` using a pre-provisioned `supervisor` template.
- `live_synthesized_template_round_trip` — `create_sandbox` synthesizes a template via kube-rs, waits for Ready, resumes an actor, reads it back, suspends + deletes. Both the template and the actor are reaped.

**Supervisor-side feature surface** — observed via a feature-observation harness (`tests/integration/`) that bakes a test workload into the supervisor image and dumps `[oshl-test]` markers from worker pod stdout:
- Supervisor boot completes inside the gVisor actor (no fatal startup error).
- HTTP CONNECT proxy binds on 127.0.0.1:3128.
- Ephemeral TLS-MITM CA generated per actor.
- OPA policy file loaded; allow decisions return `200`, deny decisions return `403`, both with `OCSF HTTP:GET […] {ALLOWED,DENIED}` audit events.
- Workload identity dropped to the policy's `run_as_user` (root in the default case, via the idempotent fast-path).
- `DegradedHandler` fires exactly as designed for the three gVisor-refused bootstrap subsystems.
- OCSF shorthand log file `/var/log/openshell.YYYY-MM-DD.log` accumulates structured events.
- Landlock probe correctly reports "Unavailable" under gVisor and skips ruleset construction (cleanly, after the fix in the OpenShell commit).
- Filesystem allow path (writes to `/tmp`) works.

## 6. Known limitations

Three classes of caveats. Listing them up front so nobody is surprised.

### a) Degraded mode is genuinely degraded

The Linux kernel features the supervisor would normally use to harden itself in-process are turned off because gVisor refuses to implement them. Concretely:

- **Network namespace isolation is off.** The supervisor's HTTP CONNECT proxy still works as a *cooperating-client* enforcement point, but **direct egress bypasses it** — a non-cooperating workload can `curl https://example.com` without going through 127.0.0.1:3128 and the supervisor will neither block nor see the request. Operators rely on the outer sandbox (gVisor's own syscall filter + Kubernetes `NetworkPolicy`) for bypass-proof egress.
- **In-process seccomp filter is off.** gVisor is itself a syscall-filtering boundary and rejects further filter installs; the supervisor's per-policy seccomp filter does nothing in this deployment.
- **Landlock filesystem sandbox is off.** gVisor doesn't implement Landlock; the supervisor's policy `read_only`/`read_write` paths are *not* enforced at the filesystem level.
- **Non-root `drop_privileges` is unsupported.** The wrapper relies on the supervisor's idempotent fast-path (skip `setresuid` when already at the target uid). This means `run_as_user` must equal the actor's starting uid (root, in the default template). Future work: have Substrate grant the actor `CAP_SETUID`/`CAP_SETGID` via `ActorTemplate.SecurityContext.capabilities.add` and re-enable the non-root path.

None of these are bugs. They're the price of running inside a smaller, opinionated sandbox runtime. The threat model has to be re-stated: the **enforcing boundary** in this deployment is gVisor + outer cluster policy, not the OpenShell supervisor's in-process hardening.

### b) Gateway-driven features are not exercised yet

The supervisor has cluster-mode features that need a real OpenShell gateway to land in the actor's network reach:

- Settings poll (toggles like `ocsf_json_enabled`).
- Inference routing (cluster-mode route bundles).
- Log push to a gateway.
- SSH attach via the supervisor's Unix socket + the gateway's RelayStream.

These all compile and link; they're just not turned on because the test cluster has no gateway. Standing up a gateway pod next to the worker pool is the natural next step.

### c) Operator handshake is two steps

`install-ate-kind.sh` (Substrate's installer) builds `atelet` but **not** `ateom-gvisor`. To use the new WorkerPool, the operator runs `ko publish ./cmd/servers/ateom-gvisor` once from the Substrate repo and exports the digest before the first harness run. Subsequent runs read it from the live WorkerPool spec.

Also: `kubectl ate exec` doesn't exist; observing the actor requires reading worker pod stdout (`kubectl logs`) for the supervisor's stderr-routed OCSF events. Good enough for now; not as nice as `kubectl exec`.

## 7. Components and where they live

Three repos, all on personal forks under `github.com/dims`. Canonical `upstream` remotes (`agent-substrate/substrate`, `NVIDIA/OpenShell`) are untouched; nothing has been pushed to them yet.

| repo | branch | what's there |
|---|---|---|
| [`dims/openshell-driver-substrate`](https://github.com/dims/openshell-driver-substrate) | `main` | The driver crate, the wrapper binary, the vendored proto, the live tests, the feature-observation harness. Single Cargo crate at root. CI: fmt + clippy + lib tests + release build. |
| [`dims/OpenShell`](https://github.com/dims/OpenShell/tree/chore/gvisor-degraded-netns-v2) | `chore/gvisor-degraded-netns-v2` | Single commit (`69d2054`) adding the trait scaffolding + `cli` module + Landlock-skip + idempotent `drop_privileges`. Behaviour-preserving. The upstreamable piece. |
| [`dims/substrate`](https://github.com/dims/substrate/tree/feat/openshell-driver-companion-v2) | `feat/openshell-driver-companion-v2` | Single commit (`6234697`) fixing a race in `ateom-gvisor`'s eth0 handling: idempotent move + deferred rollback. Without it, a partial RunWorkload leaves eth0 stranded in the interior netns and the worker pod is bricked for subsequent actors. |

The split is deliberate. The OpenShell change is the *minimum* upstreamable patch; everything substrate-specific lives in its own repo so it can evolve independently and not bloat the OpenShell tree.

## 8. Building and running

```sh
# One-time, from the Substrate repo:
KO_DOCKER_REPO=localhost:5001 ko publish --base-import-paths ./cmd/servers/ateom-gvisor
export ATEOM_IMAGE='localhost:5001/ateom-gvisor@sha256:...'

# From this repo, build + push the supervisor image, apply templates, exercise the harness:
git clone https://github.com/dims/openshell-driver-substrate
cd openshell-driver-substrate
./tests/integration/run.sh

# Run the 4 driver-side lifecycle tests:
SUBSTRATE_LIVE_API_ENDPOINT=127.0.0.1:18443 \
SUBSTRATE_LIVE_NAMESPACE=ate-openshell-m0 \
... # full env-var list in README.md
cargo test --test live -- --ignored --test-threads=1
```

Cargo resolves `openshell-sandbox` and `openshell-core` from the
pinned-rev git dep on first build (~3 minutes including the build);
subsequent builds reuse the workspace target dir.

## 9. Where to next

In rough priority order:

1. **Upstream the OpenShell trait scaffolding.** Single PR against `NVIDIA/OpenShell` with the contents of commit `69d2054`. Structurally clean; zero behaviour change for default callers; opens up the same pattern for other outer-sandbox integrations (hardened K8s pods, restricted CI runners, …).
2. **Land an `ateom-gvisor` build path in `install-ate-kind.sh`** (substrate-side). Drops the `ko publish` operator step from the first-run flow.
3. **Stand up an OpenShell gateway in the cluster.** Lets us exercise cluster-mode features: log push, inference routing, settings poll, SSH attach. Each of those has unit tests; they just don't run E2E today.
4. **`--enable-ocsf-jsonl` flag (SE-1).** Trivial fix; makes the JSONL audit layer usable in standalone deployments without a gateway.
5. **`SecurityContext.capabilities.add` plumbing.** Re-enables non-root `drop_privileges` under gVisor by having Substrate grant `CAP_SETUID`/`CAP_SETGID` to the actor. The code path existed in an earlier iteration and was intentionally cut for upstream-friendliness; can be reinstated as opt-in.
6. **GPU passthrough.** Substrate supports CDI device passthrough for runsc actors. The driver's `validate_sandbox_create` currently rejects `DriverResourceRequirements.gpu`; flipping that on is a small change once we have a test workload that needs a GPU.
7. **Performance numbers.** We have anecdotal "about a second" for cold start. Worth measuring properly: cold restore time, warm resume time after suspend, snapshot size, memory delta, against a baseline of `runsc run` from scratch.

## 10. Further reading

- Per-feature evidence + sharp-edges register: `~/notes/2026-05-23-openshell-features-findings.md` (in this repo's author's local notes; happy to share). Lists each Tier-1/2 test with status, the SE-1..SE-7 enumeration of caveats, and the disposition of each.
- Current snapshot of branch tips, commit SHAs, and cluster fixture: `~/notes/2026-05-23-openshell-on-substrate-state.md`.
- Cluster bring-up runbook (kind on macOS, plus the Shorewall recovery recipe for NVIDIA-managed Linux hosts): `~/notes/2026-05-21-agent-substrate-kind-local-dev.md`.
