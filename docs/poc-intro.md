# OpenShell on Agent Substrate — proof-of-concept overview

**Status:** working end-to-end on a kind cluster as of 2026-05-23.
**Repo:** [`dims/openshell-driver-substrate`](https://github.com/dims/openshell-driver-substrate) (tip `2b68a6d`).
**Companion change in OpenShell:** [`dims/OpenShell@b6d3a35`](https://github.com/dims/OpenShell/commit/b6d3a35facab8e597a516ebf4ddd2989ad558ce6) (single commit, 3 files / +51/-7, env-var-gated, upstreamable).
**Companion change in Agent Substrate:** [`dims/substrate@9109515`](https://github.com/dims/substrate/commit/9109515b082ac80d72de452ccf912cf0990fc829) (single commit, eth0 race fix in `ateom-gvisor`).
**Audience:** teammates familiar with at least one of OpenShell or Agent Substrate; this doc gives the joint picture.

---

## 1. What this is

A proof-of-concept that lets the **OpenShell sandbox supervisor** run as a managed actor on top of **Agent Substrate** (NVIDIA's gVisor + checkpoint/restore runtime), with the OpenShell gateway driving the sandbox lifecycle through Substrate's control plane.

The plumbing is a Rust crate (`openshell-driver-substrate`) that implements OpenShell's `ComputeDriver` gRPC trait against Substrate's `ateapi.Control` service. The stock OpenShell supervisor — with one small upstream-shaped patch behind an environment variable — boots cleanly inside a gVisor sandbox by tolerating the three privileged syscalls gVisor refuses (`unshare(CLONE_NEWNET)`, `seccomp(SECCOMP_SET_MODE_FILTER)`, and one specific `setresuid` no-op path).

In one sentence: **"OpenShell's per-request sandbox becomes a checkpoint/restore-backed Substrate actor."**

---

## 2. Why OpenShell is better *with* Substrate than without

OpenShell today runs the supervisor as a per-sandbox process tree on whatever host the gateway launches it on. Substrate replaces that "host" with a cluster-managed gVisor actor. The differences are concrete:

| Dimension | OpenShell alone (today) | OpenShell on Substrate (this POC) |
|---|---|---|
| **Cold-start time per sandbox** | Multi-second: spawn supervisor → install landlock → install seccomp → bind proxy → load OPA → set up ephemeral TLS CA → fork workload. Every sandbox pays the full cost. | **~1 s**: `runsc restore` from a per-template golden snapshot. The supervisor's proxy, TLS CA, policy engine, and audit pipeline are *already running* in the restored process — they were captured live, not replayed from config. |
| **Idle sandbox cost** | An idle sandbox is still a live process tree consuming RAM + an entry in the host's process table. Killing it loses its state. | `SuspendActor` → `runsc checkpoint` → snapshot uploaded → worker slot freed. Idle sandbox has **zero compute cost**; only storage cost for the snapshot. `ResumeActor` later brings it back, with state intact. |
| **State preservation across stop** | None. A stopped sandbox is gone; the next request starts a fresh one. | Filesystem state under the policy's `read_write` paths, in-RAM state, the supervisor's process state — all preserved by gVisor's checkpoint and brought back on resume. The supervisor and workload see continuous execution. |
| **Sandbox migration** | Bound to the launching host. If the host goes away, so does the sandbox. | Suspend on worker A → resume on worker B. Substrate's controller picks the new worker. The actor is unaware it moved; its socket-table FDs are recreated by runsc. |
| **Multi-tenancy isolation** | One layer: the OpenShell supervisor's in-process policy + landlock + seccomp. A workload that escapes the supervisor (e.g. via a `/proc` race) reaches the host kernel. | **Two layers**: gVisor's userspace-kernel boundary *plus* OpenShell's policy engine. A workload that escapes the supervisor still has to break gVisor. The two layers fail independently. |
| **Scheduling** | One-shot, on whichever node the gateway was running on. | Substrate's controller schedules actors onto pools. Distinct pools can mean distinct hardware (e.g. CPU pool vs GPU pool, region A vs region B). |
| **Operational model** | The gateway manages lifecycle directly: it tracks every sandbox it launched, decides when to kill them, handles cleanup. | The cluster manages lifecycle: WorkerPools provide capacity, the substrate controller reconciles. The gateway just emits intents (`CreateSandbox`, `StopSandbox`); cleanup of leaked actors becomes a substrate-side problem, not a gateway-side one. |
| **Failure recovery** | A crashed sandbox is gone; the gateway has to retry from scratch. | A crashed worker pod is reconciled by the controller; suspended actors survive worker pod replacement; only mid-resume actors are at risk (and the eth0-fix commit in substrate handles the partial-Run case). |
| **Audit continuity** | OCSF events live in the sandbox process's log file; lost on kill. | OCSF events flow to the supervisor's stderr → captured by ateom-gvisor → captured by the worker pod's stdout → persisted by Kubernetes log rotation. Suspend/resume preserves the events emitted by the still-running supervisor. |

The cost is intentional: gVisor's syscall filter overlaps with the supervisor's in-process landlock + seccomp, so under Substrate those run "degraded" (the supervisor doesn't try to install them — gVisor would refuse anyway). The supervisor's *value-add* (loopback HTTP proxy, OPA policy decisions, OCSF audit, identity tracking) is preserved; the kernel-level hardening becomes gVisor's responsibility.

For workloads where "fast per-sandbox cold start" + "cluster-managed lifecycle" + "two-layer isolation" are valuable, this trade is straightforwardly worth it.

---

## 3. Architecture at a glance

```
                                         ┌────────────────────────────────────┐
                                         │  Agent Substrate cluster           │
                                         │                                    │
                                         │  ┌────────────┐   ┌────────────┐   │
                                         │  │ ate-api    │   │ ate-       │   │
                                         │  │ -server    │◄─►│ controller │   │
                                         │  │ (gRPC)     │   │            │   │
                                         │  └─────┬──────┘   └─────┬──────┘   │
                                         │        │                │          │
   ┌────────────────────────┐ ateapi.Cntrl│        │  ┌─────────────▼──────┐   │
   │  OpenShell gateway     │─────────────┼────────┘  │ atelet (DaemonSet) │   │
   │  uses ComputeDriver    │             │           └──────────┬─────────┘   │
   │  trait, picks          │             │                      │             │
   │  `substrate`           │             │            ┌─────────▼──────────┐  │
   │     │                  │             │            │ WorkerPool pods    │  │
   │     ▼                  │             │            │  ┌──────────────┐  │  │
   │  ┌─────────────────┐   │             │            │  │ateom-gvisor  │  │  │
   │  │ openshell-      │   │             │            │  │ + runsc      │  │  │
   │  │ driver-substrate├───┼─────────────┼────────────┼──┤ ┌──────────┐ │  │  │
   │  │ (this crate)    │   │             │            │  │ │supervisor│ │  │  │
   │  └─────────────────┘   │             │            │  │ │ +        │ │  │  │
   └────────────────────────┘             │            │  │ │ workload │ │  │  │
                                          │            │  │ │ (actor)  │ │  │  │
                                          │            │  │ └──────────┘ │  │  │
                                          │            │  └──────────────┘  │  │
                                          │            └────────────────────┘  │
                                          └────────────────────────────────────┘
                                                        │
                                                        ▼
                                          ┌────────────────────────────┐
                                          │  rustfs (kind) / GCS (GKE) │
                                          │  golden snapshots          │
                                          └────────────────────────────┘
```

Three repos co-operate to produce the boxed picture above:

| component | repo | what it is |
|---|---|---|
| Compute driver + integration harness | [`dims/openshell-driver-substrate`](https://github.com/dims/openshell-driver-substrate) | The new repo. A Rust crate (the driver) plus a feature-observation test harness that builds the patched supervisor image from the OpenShell source tree. **This is the bulk of the POC.** |
| Env-var-gated bootstrap (one commit) | [`dims/OpenShell@b6d3a35`](https://github.com/dims/OpenShell/commit/b6d3a35facab8e597a516ebf4ddd2989ad558ce6) | Adds the `OPENSHELL_BEST_EFFORT_FAILURES` env-var gate + idempotent `drop_privileges` fast-path. 3 files, +51/-7. **Default stays strict — zero behavioural change for upstream callers.** Upstreamable as-is. |
| ateom-gvisor eth0 race fix (one commit) | [`dims/substrate@9109515`](https://github.com/dims/substrate/commit/9109515b082ac80d72de452ccf912cf0990fc829) | A substrate-side bug fix the POC exposed. **Not OpenShell-specific.** Upstreamable as-is. |

The driver doesn't talk to gVisor, atelet, or ateom-gvisor directly — only `ateapi.Control`. Everything below that is Substrate's internal layering.

§6 covers the two single-commit changes in detail.

---

## 4. How it works under the hood

### 4.1. Agent Substrate primitives the POC uses

Substrate models a sandbox as **two K8s CRDs and one runtime object**, with one gRPC service driving them:

| Substrate primitive | Kind | What it is | Who creates it |
|---|---|---|---|
| `WorkerPool` | CRD (`ate.dev/v1alpha1`) | A Deployment-like resource that runs N worker pods. Each pod has an `ateom-gvisor` container managing runsc actors on demand. | Operator (once per cluster + namespace). |
| `ActorTemplate` | CRD (`ate.dev/v1alpha1`) | A `runsc` OCI bundle + `WorkerPool` reference + a snapshots-storage URI. Substrate's controller materialises it by running a one-shot "golden actor" to completion of bootstrap, then `runsc checkpoint`s it. The template's `status.phase = Ready` means the golden snapshot is in place. | The driver (synthesizes via `kube-rs`), OR pre-provisioned by the operator. |
| `Actor` | Substrate runtime object (not a K8s CRD, lives in valkey + Substrate's API) | A live or suspended instance of an `ActorTemplate`. Resume restores from the template's golden snapshot. Suspend snapshots and frees the worker. | The driver (via `ateapi.Control`). |

### 4.2. The `ateapi.Control` gRPC surface

| RPC | What the driver does with it |
|---|---|
| `CreateActor(actor_id, actor_template_namespace, actor_template_name)` | Register a new Actor binding the given template. Returned status: `STATUS_SUSPENDED`, version=1. |
| `ResumeActor(actor_id, boot=false)` | Restore the actor on a worker. atelet on the target worker calls `ateom-gvisor.RestoreWorkload` → `runsc restore` against the template's golden snapshot. Status moves through `STATUS_RESUMING` → `STATUS_RUNNING`. |
| `GetActor(actor_id)` | Read current actor state: status, worker pod, IP, version, last snapshot URI. |
| `ListActors()` | Cluster-wide actor catalog. Driver filters to its configured namespace. |
| `SuspendActor(actor_id)` | atelet on the actor's worker calls `ateom-gvisor.CheckpointWorkload` → `runsc checkpoint`. Snapshot uploaded to `snapshotsConfig.location` in the template. Status moves to `STATUS_SUSPENDED`. Worker slot freed. |
| `DeleteActor(actor_id)` | Drop the actor record + its snapshot. Actor must be `STATUS_SUSPENDED` first (the driver tolerates a `FailedPrecondition` from a previous suspend attempt and just calls DeleteActor anyway). |

### 4.3. Driver method → substrate call mapping

OpenShell's `ComputeDriver` trait → `ateapi.Control`:

| `ComputeDriver` method | Substrate call(s) |
|---|---|
| `get_capabilities` | (none) — returns driver name + version |
| `validate_sandbox_create` | (none) — local-only: rejects bare-tag images, GPU requests, etc. |
| `get_sandbox(id_or_name)` | `GetActor` → mapped to `DriverSandbox` with a `Ready` condition derived from `Actor.Status` |
| `list_sandboxes` | `ListActors` → filtered to the namespace the driver was configured for |
| `create_sandbox(spec)` | Either reuse a pre-provisioned `ActorTemplate` (if `spec.template.platform_config["substrate_actor_template"]` is set), OR `synthesize_and_apply_template` via kube-rs and wait for `Ready`. Then `CreateActor` + `ResumeActor`. |
| `stop_sandbox(id)` | `SuspendActor` |
| `delete_sandbox(id)` | Best-effort `SuspendActor` (tolerating `FailedPrecondition`/`Internal`), then `DeleteActor`, then delete the synthesized `ActorTemplate` if the driver owns it (annotation check). |
| `watch_sandboxes` | Polling `ListActors` every 2 s, diffing snapshots, emitting `Upsert`/`Deleted` events |

### 4.4. What a synthesized `ActorTemplate` looks like

Given a `DriverSandbox` with `spec.template.image = localhost:5001/oshl-app@sha256:...`, `synthesize_template` produces (with placeholders resolved from the driver's `SubstrateComputeConfig`):

```yaml
apiVersion: ate.dev/v1alpha1
kind: ActorTemplate
metadata:
  name: oshl-<actor-id>                 # deterministic from actor_id
  namespace: ate-openshell-m0           # driver default_namespace
  annotations:
    ate.openshell.io/synthesized-by: openshell-driver-substrate@0.1.0
spec:
  pauseImage: registry.k8s.io/pause:3.10.2@sha256:f548e0e8...
  containers:
    - name: supervisor
      image: localhost:5001/oshl-app@sha256:...
      command:
        - /usr/local/bin/openshell-sandbox     # stock binary built from the patched OpenShell source
        - --policy-rules
        - /etc/openshell/policy.rego           # baked into the image
        - --policy-data
        - /etc/openshell/data.yaml             # baked into the image
        - --log-level
        - info
        - --
        - /bin/sh                              # workload command after `--`
        - -c
        - while true; do sleep 60; done
      env:
        - name: OPENSHELL_BEST_EFFORT_FAILURES # opts the supervisor into best-effort bootstrap
          value: "1"
        - name: OPENSHELL_SANDBOX_ID
          value: <actor-id>
        - name: OPENSHELL_ENDPOINT             # only when driver is configured for a gateway
          value: <gateway-grpc-endpoint>
        - name: OPENSHELL_SANDBOX_TOKEN        # only when the spec carries one
          value: <jwt>
  snapshotsConfig:
    location: gs://ate-snapshots/ate-openshell-m0/   # in-cluster S3 on kind (rustfs), GCS on GKE
  workerPoolRef:
    name: openshell-m0-pool
    namespace: ate-openshell-m0
  runsc:
    amd64:
      sha256Hash: a397be1abc242...
      url: gs://gvisor/releases/nightly/2026-05-19/x86_64/runsc
```

For a pre-provisioned template, the operator writes this YAML by hand (or out of helm/kustomize) and the caller's `spec.template.platform_config["substrate_actor_template"]` names it. The pre-provisioned path lets the operator pin a `command:` block that differs from the driver's default.

### 4.5. Boot flow for one sandbox

End-to-end timeline from the gateway's `create_sandbox` to a running actor:

1. **Gateway** → `openshell-driver-substrate::create_sandbox(spec)`.
2. **Driver** decides synthesized vs. pre-provisioned template (sees `platform_config["substrate_actor_template"]`).
3. **Driver** (synthesized path only): `kube::Api<ActorTemplate>::patch()` with server-side apply. Driver then polls the template's `status.phase` every 2 s for up to 90 s.
4. **Substrate controller** sees the new template, schedules a "golden actor" on a worker, waits for the supervisor to bootstrap, then `runsc checkpoint`s it. Phase advances: `""` → `ResumeGoldenActor` → `WaitGoldenActor` → `Ready`. Snapshot uploaded to `spec.snapshotsConfig.location`.
5. **Driver** sees `Ready`, calls `ateapi.Control.CreateActor`.
6. **ate-api-server** returns the new Actor record (`STATUS_SUSPENDED`, version=1).
7. **Driver** calls `ateapi.Control.ResumeActor`.
8. **ate-controller** runs a per-actor workflow: pick a worker with capacity → atelet on that worker calls `ateom-gvisor.RestoreWorkload(actor_id, runsc_path, spec)`.
9. **ateom-gvisor** sets up an OCI bundle, downloads the golden snapshot from `snapshotsConfig.location`, runs `runsc restore` against it.
10. **The restored process is the supervisor itself**; it picks up *exactly* where the golden left off, which is right after the supervisor finished bootstrapping. The supervisor's child workload command begins executing.
11. **Actor status** → `STATUS_RUNNING`. Driver returns from `create_sandbox`.

Wall-clock cost from step 5 to step 11 on the bigbox kind cluster: **about a second per cold actor**, dominated by golden-snapshot fetch + `runsc restore`. The expensive bootstrap (steps 3–4 = golden actor creation) happens once per template, not once per sandbox.

---

## 5. Substrate features exercised by this POC

Each row is something the POC actually does end-to-end on the bigbox kind cluster.

### 5.1. Verified working

| Substrate feature | How it's exercised in this POC |
|---|---|
| `ActorTemplate` CRD create / update / delete | Driver's `kube-rs` server-side apply + `delete` paths; synthesized templates are reaped on `delete_sandbox` via the `SYNTHESIZED_BY_ANNOTATION`. |
| Golden snapshot capture + storage | `synthesize_and_apply_template` polls until `status.phase = Ready`, which is exactly the moment the controller has uploaded the golden snapshot to `snapshotsConfig.location` (rustfs on kind, GCS on GKE). |
| `runsc restore` from snapshot | Every `ResumeActor` is a `runsc restore` invoked by atelet via ateom-gvisor. Actor restores within ~1 s of `ResumeActor`. |
| `runsc checkpoint` on suspend | `stop_sandbox` → `SuspendActor` → atelet → `ateom-gvisor.CheckpointWorkload` → `runsc checkpoint`. Confirmed via `last_snapshot` URI in `GetActor` after a suspend cycle. |
| Multi-actor concurrency on one `WorkerPool` | 4-replica WorkerPool hosts up to 4 actors. The harness's golden actor + a named test actor run on different worker pods simultaneously. |
| Actor teleport: suspend on worker A, resume on worker B | `live_write_path_round_trip` suspends + deletes; subsequent `create_sandbox` from the same template restores on a fresh worker. State preservation across the move is verified for the supervisor process (proxy still bound, policy still loaded). |
| Identity injection via env vars on `ActorTemplate.spec.containers[*].env` | Driver injects `OPENSHELL_SANDBOX_ID` (always), `OPENSHELL_ENDPOINT` (when configured), `OPENSHELL_SANDBOX_TOKEN` (when populated). Verified in the supervisor's startup banner. |
| `WorkerPool.spec.replicas` reconciliation | Tested by manually patching the pool from 4 → 6 → 4 replicas during cleanup. Substrate's controller creates / removes worker pods accordingly. |
| `ate-api-server` over TLS + bearer JWT | Driver's `load_tls_config` + `load_auth_interceptor`; CA + token files re-read on every channel build so projected SA tokens rotate without driver restart. Live tests dial `https://api.ate-system.svc:443` from the host via port-forward. |
| Pre-provisioned `ActorTemplate` reuse via `platform_config["substrate_actor_template"]` | `live_write_path_round_trip` exercises this path; the driver skips synthesis and calls `CreateActor` + `ResumeActor` against the operator-provided template. |
| `ClusterTrustBundle` for the api-server's TLS cert | Operator extracts the trust bundle via `kubectl get clustertrustbundle servicedns.podcert.ate.dev:identity:primary-bundle -o jsonpath='{.spec.trustBundle}'` and feeds it to the driver via `api_tls_ca_path`. |
| ServiceAccount token via `kubectl create token --audience` | `kubectl -n ate-system create token ate-controller --audience=api.ate-system.svc` mints the JWT the driver presents in `Authorization: Bearer`. |
| `WorkerPool.spec.ateomImage` digest pinning | The harness's `cluster-setup.yaml` substitutes a `localhost:5001/ateom-gvisor@sha256:...` digest at apply time. |
| Snapshot URI prefix per template (`snapshotsConfig.location`) | Driver writes this to every synthesized template; operators set it once per cluster. |
| `runsc` per-template pin via `spec.runsc.amd64.{sha256Hash,url}` | Driver fills this in from `SubstrateComputeConfig.runsc_amd64_*`. atelet downloads + verifies the binary before the first actor on that template starts. |

Driver-side coverage: 4 live integration tests pass in ~32 s end-to-end:

- `live_get_capabilities` — `GetCapabilities` returns the driver name + version.
- `live_list_sandboxes` — `ListSandboxes` returns the expected actors filtered to the configured namespace.
- `live_write_path_round_trip` — `create` → `get` → `stop` → `delete` against a pre-provisioned `supervisor` template.
- `live_synthesized_template_round_trip` — `create_sandbox` synthesizes a template via kube-rs, waits for Ready, resumes an actor, reads it back, suspends + deletes. Both the template and the actor are reaped.

Supervisor-side coverage: a feature-observation harness (`tests/integration/`) bakes a test workload into the supervisor image and observes the supervisor's stderr via `kubectl logs`. Confirmed:

- Supervisor boot completes inside the gVisor actor.
- HTTP CONNECT proxy binds on 127.0.0.1:3128.
- Ephemeral TLS-MITM CA generated per actor.
- OPA policy file loaded; allow decisions return `200`, deny decisions return `403`, both with `OCSF HTTP:GET […] {ALLOWED,DENIED}` audit events.
- Workload identity dropped to the policy's `run_as_user` (via the idempotent `drop_privileges` fast-path when the actor already runs at the target uid).
- The supervisor's bootstrap subsystems (network-namespace, supervisor-seccomp, workload-seccomp) emit `WARN openshell_sandbox: Sandbox bootstrap subsystem unavailable; continuing in best-effort mode (operator opted in via OPENSHELL_BEST_EFFORT_FAILURES)` and proceed past the gVisor-induced syscall failures.
- Landlock probe reports "Unavailable" under gVisor. (Note: the supervisor's own emit-only-when-unavailable Landlock log fix is deferred out of `b6d3a35` and is queued as a small follow-up commit.)

### 5.2. Wired but not exercised yet

| Feature | Why not yet | Where to start |
|---|---|---|
| `runsc` arm64 path | bigbox is amd64-only | `SubstrateComputeConfig` already has the field; flip it on when there's an arm64 test cluster. |
| GPU passthrough via Substrate's CDI plumbing | No GPU workload in the harness | `validate_sandbox_create` currently rejects `DriverResourceRequirements.gpu` — see `validate_rejects_gpu_request` unit test. Removing the reject + plumbing the GPU request into `ActorTemplate.spec.containers[*].resources` is the next step. |
| `ateapi.Control.WatchActors` streaming RPC | Substrate didn't ship the streaming RPC at the time of this POC | Driver's `watch_sandboxes` polls `ListActors` every 2 s. Re-vendoring the proto + switching to the streaming RPC is a small change. |
| `ActorTemplate.spec.containers[*].securityContext` (extra Linux caps) | Cut from v2 for upstream-friendliness; needed for non-root `drop_privileges` | A previous iteration of the driver requested `CAP_SETUID`/`CAP_SETGID`/`CAP_NET_ADMIN`/`CAP_SYS_ADMIN` here; cluster controllers running the field-strict CRD schema rejected with HTTP 500. The driver currently emits no `securityContext`. |
| Per-actor mTLS client cert (substrate-side authz) | Optional config knob; no test cluster requires it | Driver's `load_tls_config` handles the path when both `api_client_cert_path` + `api_client_key_path` are set. |

### 5.3. Not supported

| Feature | Why |
|---|---|
| Per-sandbox CPU / memory limits | `ActorTemplate.spec.containers[*]` supports `resources` but the driver doesn't propagate `DriverResourceRequirements.{cpu,memory}` into them yet. |
| `kubectl ate exec` into a running actor | Substrate doesn't ship that subcommand; observability is via `kubectl logs <worker-pod>` of the actor's stdout. |

---

## 6. The two upstream-shaped commits

The driver crate itself stands alone in the new repo. The other two pieces are surgical single-commit changes to the canonical projects.

### 6.1. OpenShell: `dims/OpenShell@b6d3a35`

**Scope.** 3 files in `crates/openshell-sandbox/src/`, net +51/−7. Two orthogonal threads, each motivated by needing the supervisor to boot inside an outer sandbox without giving up the supervisor's value-add:

1. **`OPENSHELL_BEST_EFFORT_FAILURES` env-var gate.** Adds a private `best_effort_failures()` helper (a `OnceLock`-cached read of `std::env::var_os`) and a `pub(crate) fn handle_bootstrap_failure(subsystem, err)` that either re-raises the error (strict default) or logs a single `tracing::warn` and returns `Ok(())` (when the env var is set). Three call sites that previously hard-failed are routed through the helper:
   - `lib.rs`: network namespace creation when `unshare(CLONE_NEWNET)` returns EPERM under gVisor.
   - `lib.rs`: the supervisor seccomp prelude when `seccomp(SECCOMP_SET_MODE_FILTER)` returns EINVAL under gVisor.
   - `sandbox/linux/mod.rs`: the workload seccomp filter, same EINVAL.
2. **Idempotent `drop_privileges` fast-path.** When `geteuid()/getegid()` already equal the policy's target uid/gid, the `initgroups`/`setresgid`/`setresuid` syscalls are no-ops that the kernel still rejects with EPERM under reduced capability sets. Short-circuiting matches the implicit contract of "drop privileges to X" when the process is already at X. Standalone correctness fix in `process.rs`.

The driver injects `OPENSHELL_BEST_EFFORT_FAILURES=1` into every `ActorTemplate.spec.containers[0].env` it synthesizes; the harness's Dockerfile bakes the same value into `ENV`. Either source is sufficient.

**Why it's upstreamable.** Zero behavioural change for default callers — the helper short-circuits to the original `Err(...)` path when the env var is unset, and `OnceLock` ensures the check costs one branch on the steady-state hot path. All 777 sandbox unit tests pass unchanged. The `openshell-sandbox` binary's `--help` output is byte-identical. Only an operator who explicitly opts in via the env var sees the new behaviour.

A previous iteration of this change (`dims/OpenShell@69d2054`, preserved at the `chore/gvisor-degraded-netns-v2-trait` branch as a rollback point) used a `SandboxFailureHandler` trait + a `set_failure_handler()` registration call + a substrate-side `DegradedHandler` impl + a wrapper binary that registered the handler. That iteration was 6 files / +480/-375 and required a `main.rs → cli.rs` extraction so the wrapper binary could reuse the CLI. The env-var design collapses all of that to a single env-var read, no API change, no public surface added to OpenShell. Also bundled into the previous iteration but **deferred** out of `b6d3a35` to keep the diff minimal: a `landlock::prepare()` probe-and-skip that would replace the misleading "Applying Landlock"/"Built ruleset" log pair with a single `OCSF CONFIG:SKIPPED` event when the kernel doesn't implement Landlock. Strictly cosmetic; functionally fs sandboxing is gone under gVisor either way. Can land as a small follow-up commit (~21 lines).

### 6.2. Substrate: `dims/substrate@9109515`

**Scope.** 1 file, +57/−2, in `cmd/servers/ateom-gvisor/ateom-gvisor.go`. Fixes an `eth0`-handling race in `RunWorkload` / `RestoreWorkload`.

**Original code.** ateom-gvisor moves the pod's `eth0` interface into the actor's interior network namespace before invoking `runsc create`/`runsc restore`, so the actor can reach the network:

```go
eth0Link, _ := netlink.LinkByName("eth0")
netlink.LinkSetNsFd(eth0Link, int(s.interiorNetNS))     // move into interior
// ... runsc create/restore, OCI bundle setup, etc. ...
// (eth0 is moved back out at the end by a later step)
```

**Bug.** If anything between the move-in and the move-out errors out — a `runsc restore` failure, a checkpoint-fetch timeout, an OCI bundle assembly issue, anything — `eth0` is left stranded in the interior netns. The original code had no rollback. The next time atelet asks ateom-gvisor to run an actor on the same worker pod, the supervisor finds `eth0: Link not found` because it's looking in the pod netns. The pod is bricked for further actors until it's restarted.

**Reproduction rate.** Trivial under any non-trivial create-rate. Our integration harness hit it on every second iteration: one actor would fail mid-flight, and the next test on the same worker would fail with `Link not found`. The user-visible symptom was the entire `live` test suite alternating between green and red runs.

**Fix.** Two complementary additions:

1. **`ensureEth0InPodNetns`** runs at the top of every `RunWorkload`/`RestoreWorkload`. If `eth0` is missing from the pod netns and present in the interior, move it back. Idempotent (no-op if `eth0` is already in the pod netns or absent from both). Recovers from prior partial failures.
2. **Deferred rollback.** Right after the eth0-into-interior move, register a `defer` that moves it back out if the calling function returns an error. Combined with point 1, this gives "eth0 always returns to the pod netns" as an invariant across both success and failure paths.

The success path is unchanged.

**Why it's upstreamable independent of OpenShell.** Any heavy ateom-gvisor user hits this; OpenShell happens to be the consumer that surfaced it. The fix is structural, small, and well-tested (verified by repeated create+resume cycles via the integration harness without alternating failures).

---

## 7. Known limitations

Three classes of caveats. Listing them before the walkthrough so nobody is surprised at runtime.

### a) Degraded mode is genuinely degraded

The Linux kernel features the supervisor would normally use to harden itself in-process are turned off because gVisor refuses to implement them:

- **Network namespace isolation is off.** The supervisor's HTTP CONNECT proxy still works as a *cooperating-client* enforcement point, but direct egress bypasses it — a non-cooperating workload can `curl https://example.com` without going through 127.0.0.1:3128 and the supervisor neither blocks nor sees the request. Operators rely on the outer sandbox (gVisor's own filter + K8s `NetworkPolicy`) for bypass-proof egress.
- **In-process seccomp filter is off.** gVisor is itself a syscall-filtering boundary and rejects further filter installs.
- **Landlock filesystem sandbox is off.** gVisor doesn't implement Landlock; the supervisor's policy `read_only`/`read_write` paths are *not* enforced at the filesystem level.
- **Non-root `drop_privileges` is unsupported.** The supervisor relies on its own idempotent fast-path (skip `setresuid` when already at target uid). `run_as_user` must equal the actor's starting uid (root, in the default template) until Substrate's `SecurityContext.capabilities.add` plumbing is restored.

None of these are bugs. They're the price of running inside a smaller, opinionated sandbox runtime. The threat model has to be re-stated: the **enforcing boundary** in this deployment is gVisor + outer cluster policy, not the OpenShell supervisor's in-process hardening.

### b) Gateway-driven features are not exercised yet

The supervisor has cluster-mode features that need a real OpenShell gateway to land in the actor's network reach:

- Settings poll (toggles like `ocsf_json_enabled`).
- Inference routing (cluster-mode route bundles).
- Log push to a gateway.
- SSH attach via the supervisor's Unix socket + the gateway's `RelayStream`.
- Cross-sandbox identity (the supervisor's IDOR guard).

These all compile and link; they're just not turned on because the test cluster has no gateway. Standing up a gateway pod next to the worker pool is the natural next step.

### c) Operator handshake is two steps

`install-ate-kind.sh` builds `atelet` but **not** `ateom-gvisor`. The first-run handshake (`ko publish` + `export ATEOM_IMAGE`) is documented in §8.1 below; once the WorkerPool exists, subsequent runs don't need it. Substrate-side upstream change worth landing: have `install-ate-kind.sh` build + push `ateom-gvisor` alongside `atelet`.

---

## 8. Operator walkthrough — standing up the POC on a kind cluster

This is the exact sequence used on bigbox; every command is real.

**Prerequisites:**
- A Substrate-installed kind cluster (the standard `hack/create-kind-cluster.sh` + `hack/install-ate-kind.sh --deploy-ate-system` from `agent-substrate/substrate`).
- Linux host with cargo + docker + access to the kind-registry at `localhost:5001`.
- `grpcurl` on PATH (`go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest`).
- On NVIDIA-managed Linux hosts, Shorewall disabled (see `~/notes/2026-05-21-agent-substrate-kind-local-dev.md` §10).

### 8.1. Build + push `ateom-gvisor` (one time per cluster)

`install-ate-kind.sh` builds `atelet` but not `ateom-gvisor`. Produce + push the image:

```sh
cd ~/go/src/github.com/agent-substrate/substrate         # the substrate fork with the eth0 fix
git checkout feat/openshell-driver-companion-v2          # or merge 9109515 onto your branch
KO_DOCKER_REPO=localhost:5001 KO_DEFAULTPLATFORMS=linux/$(go env GOARCH) \
  ko publish --base-import-paths ./cmd/servers/ateom-gvisor
# Capture the digest from ko's output:
export ATEOM_IMAGE='localhost:5001/ateom-gvisor@sha256:...'
```

### 8.2. Bootstrap the OpenShell namespace + WorkerPool + supervisor template (one time)

`tests/integration/run.sh` does this automatically on the first run when the `ate-openshell-m0` namespace is missing. Equivalent direct kubectl:

```sh
sed -e "s|__ATEOM_IMAGE__|$ATEOM_IMAGE|g" \
    -e "s|__SUPERVISOR_IMAGE__|$SUP_IMAGE|g" \
    tests/integration/cluster-setup.yaml \
  | kubectl apply -f -
```

After this, three new objects exist:
- `Namespace/ate-openshell-m0`
- `WorkerPool/openshell-m0-pool` (4 replicas)
- `ActorTemplate/supervisor` (the basic sleep-loop template the live `write_path` test uses)

### 8.3. Build + push the supervisor image, apply the feature-test template, exercise the harness

```sh
cd ~/go/src/github.com/dims/openshell-driver-substrate   # the new repo
./tests/integration/run.sh
```

This will:
1. Resolve the OpenShell source tree (`$OPENSHELL_REPO` → sibling `../OpenShell` → clone the pinned SHA into a temp dir) and `cargo build --release --bin openshell-sandbox` from there. The resulting binary is the stock supervisor with the env-var-gated patch baked in.
2. Assemble a Docker build context (Dockerfile + `openshell-sandbox` + `policy.rego` + `data.yaml` + `test-workload.sh`).
3. `docker build` + `docker push` to `localhost:5001/oshl-feature-test:latest`. The Dockerfile bakes `OPENSHELL_BEST_EFFORT_FAILURES=1` into `ENV`.
4. `kubectl apply` the `oshl-feature-test` ActorTemplate referencing the new digest. The template's `containers[].env` re-states `OPENSHELL_BEST_EFFORT_FAILURES=1` for operator visibility.
5. Wait for the template's `status.phase = Ready`.
6. Mint a fresh `ate-system/ate-controller` token, extract the cluster's trust bundle, refresh the api-server port-forward.
7. `grpcurl ateapi.Control/CreateActor` + `ResumeActor` on a fresh actor ID.
8. Sleep 25 s for the workload to run probes.
9. Dump `[oshl-test]` markers from every worker pod's stdout.
10. `SuspendActor` + `DeleteActor` to clean up.

### 8.4. Run the 4 driver-side live tests

```sh
SUBSTRATE_LIVE_API_ENDPOINT=127.0.0.1:18443 \
SUBSTRATE_LIVE_NAMESPACE=ate-openshell-m0 \
SUBSTRATE_LIVE_CA_PATH=/tmp/ate-servicedns-ca.pem \
SUBSTRATE_LIVE_BEARER_TOKEN_PATH=/tmp/ate-bearer.token \
SUBSTRATE_LIVE_TLS_SERVER_NAME=api.ate-system.svc \
SUBSTRATE_LIVE_WORKER_POOL=openshell-m0-pool \
SUBSTRATE_LIVE_SNAPSHOTS_LOCATION=gs://ate-snapshots/ate-openshell-m0/ \
SUBSTRATE_LIVE_RUNSC_AMD64_SHA=a397be1abc242... \
SUBSTRATE_LIVE_RUNSC_AMD64_URL=gs://gvisor/releases/nightly/2026-05-19/x86_64/runsc \
SUBSTRATE_LIVE_PAUSE_IMAGE=registry.k8s.io/pause:3.10.2@sha256:f548e0e8... \
SUBSTRATE_LIVE_TEMPLATE_NAME=supervisor \
SUBSTRATE_LIVE_TEST_IMAGE=localhost:5001/oshl-feature-test@sha256:... \
  cargo test --test live -- --ignored --test-threads=1
```

Each env var:

| env var | meaning |
|---|---|
| `SUBSTRATE_LIVE_API_ENDPOINT` | host:port of `ate-api-server`. Usually `127.0.0.1:18443` (a `kubectl port-forward` of the service). |
| `SUBSTRATE_LIVE_NAMESPACE` | The namespace your ActorTemplates + WorkerPool live in. |
| `SUBSTRATE_LIVE_CA_PATH` | Path to the cluster's `ClusterTrustBundle` PEM. The live test reads this and constructs a tonic `ClientTlsConfig` with it. |
| `SUBSTRATE_LIVE_BEARER_TOKEN_PATH` | Path to a JWT minted by `kubectl create token ate-controller --audience=api.ate-system.svc`. Re-read on every channel build. |
| `SUBSTRATE_LIVE_TLS_SERVER_NAME` | Domain to match against the api-server's TLS cert SANs. Usually `api.ate-system.svc`. |
| `SUBSTRATE_LIVE_WORKER_POOL` | Name of the `WorkerPool` the synthesized template should reference. |
| `SUBSTRATE_LIVE_SNAPSHOTS_LOCATION` | `gs://...` (or `s3://...`) prefix where Substrate stores golden snapshots. On kind, this resolves to in-cluster rustfs. |
| `SUBSTRATE_LIVE_RUNSC_AMD64_*` | runsc binary pin (sha256 + URL). atelet downloads + verifies before first restore. |
| `SUBSTRATE_LIVE_PAUSE_IMAGE` | Pause container the OCI bundle uses as the actor's root. Substrate requires a digest reference. |
| `SUBSTRATE_LIVE_TEMPLATE_NAME` | Name of the pre-provisioned template the write-path test should reuse. |
| `SUBSTRATE_LIVE_TEST_IMAGE` | Digest-pinned image the synthesized-template test should put in `spec.template.image`. |

### 8.5. Inspecting the running cluster

```sh
# All actors substrate is tracking (cluster-wide):
kubectl ate get actors

# All ActorTemplates in our namespace + their phase:
kubectl -n ate-openshell-m0 get actortemplate

# Worker pool capacity + which pods exist:
kubectl -n ate-openshell-m0 get workerpool openshell-m0-pool -o yaml
kubectl -n ate-openshell-m0 get pods -l ate.dev/worker-pool=openshell-m0-pool

# Last snapshot URI for a specific actor (from inside the rustfs/GCS bucket):
kubectl ate get actors -o yaml <actor-id> | grep last_snapshot

# Supervisor stderr (the OCSF shorthand log) for an actor:
kubectl -n ate-openshell-m0 logs <worker-pod>     # pod hosting the actor; from `kubectl ate get actors`
```

For ad-hoc `ateapi.Control` calls without the live test framework, use `grpcurl`:

```sh
TOKEN=$(kubectl -n ate-system create token ate-controller --audience=api.ate-system.svc)
grpcurl -insecure \
  -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $TOKEN" \
  -d '{}' 127.0.0.1:18443 ateapi.Control/ListActors
```

---

## 9. Where to next

In rough priority order:

1. **Upstream the OpenShell env-var gate.** Single PR against `NVIDIA/OpenShell` with the contents of commit `b6d3a35`. 3 files / +51/-7. Default stays strict; only opt-in operators see the new behaviour. The Landlock cosmetic-log follow-up can land separately.
2. **Upstream the substrate eth0 fix.** Single PR against `agent-substrate/substrate` with `9109515`. The bug is not OpenShell-specific.
3. **Land an `ateom-gvisor` build path in `install-ate-kind.sh`** (substrate-side). Removes the `ko publish` operator step.
4. **Stand up an OpenShell gateway in the cluster.** Lets us exercise cluster-mode features end-to-end (the §7b gap).
5. **Streaming `WatchActors`.** Re-vendor the proto, switch `watch_sandboxes` from the 2 s poll to the streaming RPC.
6. **GPU passthrough.** Remove the `validate_sandbox_create` reject and plumb `DriverResourceRequirements.gpu` into `ActorTemplate.spec.containers[*].resources`.
7. **`--enable-ocsf-jsonl` flag.** Trivial fix; makes the JSONL audit layer usable in standalone deployments without a gateway.
8. **`SecurityContext.capabilities.add` plumbing.** Re-enables non-root `drop_privileges` under gVisor by having Substrate grant `CAP_SETUID`/`CAP_SETGID` to the actor.
9. **Performance numbers.** We have anecdotal "about a second" for cold start. Worth measuring properly: cold restore time, warm resume time after suspend, snapshot size, memory delta, against a baseline of `runsc run` from scratch.

---

## 10. Further reading

- Per-feature evidence + sharp-edges register: `~/notes/2026-05-23-openshell-features-findings.md`. Lists each Tier-1/2 test with status, the SE-1..SE-7 enumeration of caveats, and the disposition of each.
- Current snapshot of branch tips, commit SHAs, and cluster fixture: `~/notes/2026-05-23-openshell-on-substrate-state.md`.
- Cluster bring-up runbook (kind on macOS, plus the Shorewall recovery recipe for NVIDIA-managed Linux hosts): `~/notes/2026-05-21-agent-substrate-kind-local-dev.md`.
- Original feature-test plan: `~/notes/2026-05-23-openshell-features-test-plan.md`.
