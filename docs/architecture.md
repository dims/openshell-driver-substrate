# Architecture

How an OpenShell sandbox becomes an Agent Substrate actor, what runs where,
and what each piece trusts.

## In one paragraph

OpenShell's gateway creates and drives sandboxes through a `ComputeDriver`
gRPC contract. This repository is one such driver, running as its own process
on a Unix socket. It maps each gateway call onto Substrate's `ateapi.Control`
API: a sandbox is an `Actor`, a workload is one `ActorTemplate`, and starting a
sandbox is a restore of that template's golden snapshot into a micro-VM. Inside
the VM, stock OpenShell binaries run unmodified: `openshell-sandbox` holds the
workload, `openshell-supervisor` enforces policy and audits, and a small relay
container makes the workload reachable through Substrate's ingress. Nothing in
OpenShell, Substrate's guest kernel, or kata is forked. Six Substrate commits
are required, and a seventh matters on gVisor only;
[`upstream-branches.md`](upstream-branches.md) lists them.

## The big picture

```mermaid
flowchart TB
  classDef stock fill:#ecfdf5,stroke:#059669,color:#064e3b
  classDef ours fill:#dbeafe,stroke:#1d4ed8,color:#1e3a8a,stroke-width:2px
  classDef sub fill:#fff7ed,stroke:#ea580c,color:#7c2d12
  classDef guest fill:#fdf4ff,stroke:#a21caf,color:#701a75
  classDef store fill:#f1f5f9,stroke:#475569,color:#0f172a
  classDef edge fill:#f0fdf4,stroke:#16a34a,color:#14532d

  cli["openshell CLI, grpcurl, curl"]
  kate["kubectl-ate"]

  subgraph openshell["NVIDIA OpenShell (stock)"]
    gw["openshell-gateway<br/>--compute-driver substrate<br/>--compute-driver-socket"]
  end

  subgraph repo["this repository"]
    driver["openshell-driver-substrate<br/>ComputeDriver gRPC server on a Unix socket<br/>a sandbox is an Actor, a workload is one ActorTemplate<br/>named by the hash of its own contents"]
  end

  subgraph substrate["Substrate control plane"]
    direction LR
    ctl["ate-controller<br/>WorkerPool → Deployment of worker pods<br/>syncer: pod ↔ Worker, drain,<br/>release the actors of a dead pod"]
    api["ate-api-server<br/>ateapi.Control, gRPC over TLS 1.3 + bearer token<br/>ActorTemplate, Actor, Worker, EgressPolicy, Tag<br/>workflows: resume, suspend, revert, delete"]
    pg[("postgres")]
    recon["template reconciler<br/>golden actor: boot, wait 20 s,<br/>checkpoint, tag"]
    ctl --> api
    api --- pg
    api --> recon
  end

  router["atenet router, ingress<br/>CONNECT tunnel on :8081<br/>ate-target-actor: atespace/name"]

  atelet["atelet, DaemonSet, one per node, versioned<br/>pulls images by digest, builds OCI specs, durable dirs<br/>AteomHerder: Run, Checkpoint, Restore"]

  subgraph pod["worker pod"]
    ateom["ateom-microvm<br/>cloud-hypervisor, virtiofsd, kata-agent"]
  end

  subgraph vm["micro-VM"]
    direction LR
    relay["relay container, plain<br/>0.0.0.0:8081 → 127.0.0.1:8080"]
    sbx["sandbox container<br/>openshell-sandbox → python3 agent.py<br/>uid 65532, no capabilities, no_new_privs<br/>seccomp broker, Landlock, DNS relay"]
    sup["supervisor container<br/>openshell-supervisor, isolation-backend role<br/>OPA policy, egress proxy, TLS CA, OCSF audit"]
    relay -- "loopback" --> sbx
    sbx <-- "boundary, TLS on 127.0.0.1:17777" --> sup
  end

  store[("object store, rustfs in kind, GCS on GKE<br/>golden and per-actor snapshots")]
  egress["atenet egress<br/>terminates the actor's CONNECT<br/>allows the actor's EgressPolicy only"]
  model["model endpoint, ollama on the host<br/>the one allow-listed destination"]

  cli --> gw
  gw -- "compute_driver.proto" --> driver
  driver -- "CreateActorTemplate, CreateActor, ResumeActor,<br/>SuspendActor, DeleteActor, GetActor, ListActors" --> api
  kate -- "ateapi.Control" --> api
  api -- "Restore, Checkpoint" --> atelet
  ctl -. "creates, replaces" .-> pod
  atelet -- "RunWorkload, RestoreWorkload, CheckpointWorkload" --> ateom
  ateom -- "restore ≈ 350 ms" --> sbx
  ateom <-- "snapshot files" --> store
  cli -- "CONNECT through a port-forward" --> router
  router --> relay
  sup -- "allowed connections, atunnel" --> egress
  egress --> model

  class gw stock
  class driver ours
  class api,pg,recon,ctl,atelet,ateom sub
  class router,egress edge
  class sbx,sup,relay guest
  class store,model store
```

Colors: green is stock OpenShell, blue is this repository, orange is
Substrate, purple is inside the guest, light green is Substrate's network edge.

Three trust domains meet here:

| Domain | Runs | Trusts |
|---|---|---|
| OpenShell gateway and this driver | outside the cluster, or as a pod | `ate-api-server` over TLS with a bearer token whose audience is `api.ate-system.svc` |
| Substrate control plane and node agents | namespace `ate-system`, one atelet per node, one ateom per worker pod | Kubernetes RBAC, image digests, the object store |
| The guest | one micro-VM per actor | its own kernel and cloud-hypervisor; inside it, the sandbox trusts only the key embedded in its bootstrap |

## Components

### openshell-gateway, stock

With `--compute-driver-socket`, the gateway binds the name given to
`--compute-driver` to that socket, whether or not a built-in driver has the
same name. The gateway keeps
the sandbox catalog, mints the per-sandbox launch credentials, and derives a
sandbox's phase from the conditions the driver reports (`derive_phase`):
`Ready=True` is Ready, `Suspended=True` without `Ready=True` is Stopped, a
`Ready=False` with a reason on its transient allowlist is Provisioning, and
one with any other reason is a sticky Error. It also times out provisioning at
five minutes (`ProvisioningTimedOut`).

### openshell-driver-substrate, this repository

`src/main.rs` parses flags (or `SUBSTRATE_*` and `OPENSHELL_*` environment
variables), binds the socket, and serves `ComputeDriverServer`.

`src/lib.rs` implements the trait:

| ComputeDriver call | What the driver does |
|---|---|
| `get_capabilities` | Names itself `substrate`; `gateway_manages_lifecycle`, `supports_sandbox_authentication` and `driver_reports_runtime_readiness` are false. |
| `validate_sandbox_create` | Requires `spec.template.image` with an `@sha256:` digest. A bare tag fails atelet's pull cache later, so it is refused here. |
| `create_sandbox` | Names the template, `ensure_ready` (below), `CreateActor` (an `AlreadyExists` is a retried create and is fine), `ResumeActor`. |
| `start_sandbox` | `ResumeActor`. |
| `stop_sandbox` | `SuspendActor`: checkpoint, free the worker, keep the snapshot. |
| `delete_sandbox` | `DeleteActor` with `any_state: true`. The template stays; other actors may share it. |
| `get_sandbox`, `list_sandboxes` | `GetActor`, `ListActors` walked to the last page (1000 per page). |
| `watch_sandboxes` | Substrate has no watch RPC. The driver polls `ListActors` every 2 s, diffs against the last snapshot, and emits changed and deleted sandboxes. It stops when the gateway drops the stream. |
| `ensure_workspace` | `CreateAtespace` for the one configured atespace. The trait carries workspace identity only on create, so one atespace serves every workspace. |
| `delete_workspace` | No-op. |
| `authenticate_sandbox` | Unimplemented. |

One `ateapi.Control` channel is dialed lazily and reused. An interceptor adds
`Authorization: Bearer` from a token file read on every client build, so a
rotated projected token needs no restart. TLS is on when a CA bundle is
configured; a client certificate is optional and must come as a pair.

Conditions are the driver's only voice to the gateway, so `actor_state_conditions`
is written against the gateway's phase rules and pinned by a table test:

| Actor state | Conditions reported | Gateway phase |
|---|---|---|
| RUNNING | `Ready=True` (ContainerRunning) | Ready |
| RESUMING, REVERTING | `Bootstrapping=True`, `Ready=False` (ContainerStarting) | Provisioning; `Bootstrapping=True` is what lets a Stopped sandbox start |
| SUSPENDING, PAUSING | `Suspended=False` only | Provisioning |
| SUSPENDED, PAUSED | `Suspended=True`, `Ready=False` (ContainerStopped) | Stopped |
| CRASHED | `Ready=False` (ContainerExited) | Error, on purpose: it needs `RevertActor` |
| DELETING | `Ready=False` (ContainerStopped); the status `deleting` flag is set | Deleting |

### Template synthesis, `src/template.rs`

Everything the golden snapshot depends on goes into the template, and nothing
else: the image, the command, the environment (`spec.environment`, then
`spec.template.environment` over it, then `OPENSHELL_ENDPOINT` over both so a
caller cannot redirect the gateway address), the sandbox config name, and the
snapshot location. The template is encoded with prost, its metadata blanked,
and hashed; the name is `oshl-` plus the first eight bytes of the SHA-256. Two
sandboxes with the same inputs share one template and one golden snapshot.
Any change to what the template contains gets a new one.

Nothing per sandbox may be in a template. A snapshot freezes process memory,
so a per-sandbox value baked into it would come back identical in every actor
restored from it. The sandbox id and the gateway-minted token are left out for
that reason, and a test asserts it. That is also why the gateway path cannot
reach Ready today; see [Credentials](#credentials).

The synthesized shape is one container: capabilities dropped (`openshell-sandbox`
refuses to run with any capability in its bounding set), a durable-dir volume
at `/tmp` (its Landlock probe writes there and the stock image has no `/tmp`),
full snapshots on pause and commit, cold boot on first resume, sandbox class
micro-VM. The helpdesk example uses a hand-written three-container template
instead, because it needs a supervisor and a relay; the two shapes differ only
in what they contain, not in how Substrate treats them.

`ensure_ready` creates the template, tolerates `AlreadyExists`, then polls
`GetActorTemplate` every 2 s until `status.goldenSnapshotStatus.goldenTag` is
set, or `error_message` is set (FailedPrecondition), or 180 s pass
(DeadlineExceeded).

### Substrate control plane

- **ate-api-server** owns the resources (`Atespace`, `ActorTemplate`, `Actor`,
  `Worker`, `EgressPolicy`, `Tag`) in postgres and runs the workflows: resume
  picks a Worker and asks its node's atelet to restore; suspend asks for a
  checkpoint and clears the assignment; revert puts a crashed or running actor
  back at its last external snapshot; delete tears down from any state when
  asked. It speaks gRPC over TLS with the `servicedns` trust bundle and a
  bearer token; it needs no client certificate.
- **template reconciler**, inside ate-api-server, gives a new template its
  golden snapshot: it creates one golden actor, resumes it from cold boot,
  waits a fixed 20 s (`goldenSnapshotWarmup`), suspends it, and tags the
  snapshot in atespace `ate-golden`. Whatever the VM looked like at 20 s is
  what every actor starts from.
- **ate-controller** turns a `WorkerPool` into a Deployment of worker pods and
  runs the syncer: a new pod registers as a Worker, a terminating pod marks
  its Worker `DRAINING`, a vanished pod deletes its Worker, which releases the
  actors bound to it into `CRASHED`.
- **atelet** is a DaemonSet, one per node and versioned per node. It pulls
  images by digest into a cache, builds the OCI specs (capabilities as the
  template says; with the commits in [`upstream-branches.md`](upstream-branches.md),
  also the image's `USER` and, for micro-VM, `no_new_privileges` and the
  low-port sysctl), lays out durable dirs, and drives the node's ateoms
  through `AteomHerder`: Run, Checkpoint, Restore.
- **ateom-microvm** is the worker pod's process: it launches cloud-hypervisor,
  serves the rootfs layers over virtiofsd, talks to the kata-agent in the
  guest to start containers, and checkpoints or restores the whole VM. A
  worker hosts as many actors as its capacity fits; the helpdesk template asks
  for a worker's whole CPU, so its pool of two gives two concurrent actors.
- **object store**: rustfs in kind, GCS on GKE. A snapshot is `base-id`,
  `config.json`, `memory-ranges`, `rootfs-upper.tar`, `durable-dir.tar`,
  `state.json`. A restore is about 350 ms.
- **atenet** is one binary in two roles. The router is ingress: an HTTP
  `CONNECT` on service port 8081 with an `ate-target-actor: atespace/name`
  header opens a TCP tunnel to any port on the actor. The egress terminates
  the actor's outbound `CONNECT`s and allows only what the actor's
  `EgressPolicy` names, 403 otherwise.

### Inside the micro-VM

One guest, one network namespace, three containers from the helpdesk template:

- **sandbox**: `openshell-sandbox` is PID 1 of the workload's container. It
  reads its bootstrap and unlinks it, refuses to run unless it is non-root,
  capability-free and `no_new_privs`, and probes the kernel for Landlock,
  seccomp user notification, socket virtualization and a DNS relay bind.
  Then it listens for its supervisor on `127.0.0.1:17777` over TLS. It forks
  the workload under seccomp and Landlock and brokers the workload's network
  calls.
- **supervisor**: `openshell-supervisor --role isolation-backend` finds the
  boundary through its runtime descriptor, authenticates with its token pair,
  loads the Rego policy and data into Open Policy Agent (OPA), installs a
  TLS-interception certificate authority (CA) the workload trusts, starts the
  workload through the sandbox, decides every connection, proxies the allowed
  ones, and writes Open Cybersecurity Schema Framework (OCSF) audit lines.
- **relay**: plain `asyncio` in a plain container. The sandbox's broker refuses
  `accept(2)` from a non-loopback peer, so Substrate's router cannot reach the
  agent directly. The relay accepts on the actor's address and connects over
  loopback. It stands in for the gateway's loopback service endpoint.

The boundary channel is TCP on loopback, not a Unix socket: containers of one
actor share the guest network namespace, but a Unix socket on a shared
durable dir is not shared across them (`connect(2)` gives `ECONNREFUSED`).

```mermaid
sequenceDiagram
  autonumber
  participant A as ateom-microvm
  participant K as kata-agent
  participant SB as sandbox container<br/>openshell-sandbox
  participant SU as supervisor container<br/>openshell-supervisor
  participant RL as relay container
  participant WL as workload<br/>python3 agent.py

  A->>K: create containers from the OCI specs
  K->>SB: start /openshell-sandbox --bootstrap /.openshell/channel/sandbox/bootstrap.json
  K->>SU: start openshell-supervisor --role isolation-backend<br/>--backend-descriptor-file, --auth-bundle-file, --policy-rules, --policy-data
  K->>RL: start python3 relay.py
  SB->>SB: read bootstrap.json, then unlink it
  Note over SB,SU: qualification gates: non-root uid+gid, empty capability sets,<br/>no_new_privs, task-memory probe, Landlock allow/deny under /tmp,<br/>seccomp notification, socket virtualization + DNS relay bind
  SB->>SB: listen TLS on 127.0.0.1:17777 · "Boundary control listener ready"
  SU->>SU: read runtime-descriptor.json and auth.json (JWT pair, 1 h)
  SU->>SB: dial the boundary, present the sandbox token
  SB-->>SU: verified against the embedded key · "Isolation boundary attached"
  SU->>SU: load policy.rego + data.yaml into OPA
  SU->>SU: create CA in /run/openshell-supervisor-ca · "Landlock ruleset built"
  SU->>SB: start_agent(python3 /opt/helpdesk/agent.py, child_env)
  SB->>WL: fork, apply seccomp + Landlock, exec · "PROC:LAUNCH python3"
  WL->>WL: listen 0.0.0.0:8080 inside the sandbox
  RL->>RL: listen 0.0.0.0:8081, forward to 127.0.0.1:8080
  Note over A,WL: the template reconciler checkpoints this whole VM at 20 s,<br/>and every actor is a restore of it, with booted set before the snapshot
```

## Lifecycle

```mermaid
stateDiagram-v2
  direction LR
  [*] --> SUSPENDED: CreateActor, holds the golden snapshot
  SUSPENDED --> RESUMING: ResumeActor (create_sandbox, start_sandbox)
  RESUMING --> RUNNING: RestoreWorkload done
  RUNNING --> SUSPENDING: SuspendActor (stop_sandbox)
  SUSPENDING --> SUSPENDED: CheckpointWorkload done, worker freed
  RUNNING --> CRASHED: worker pod gone, syncer releases the actor
  CRASHED --> REVERTING: RevertActor (kubectl-ate revert)
  REVERTING --> SUSPENDED: back at the last external snapshot
  RUNNING --> DELETING: DeleteActor any_state (delete_sandbox)
  SUSPENDED --> DELETING: DeleteActor
  CRASHED --> DELETING: DeleteActor
  DELETING --> [*]

  note right of RUNNING
    gateway phase Ready
    Ready=True, reason ContainerRunning
  end note
  note right of SUSPENDED
    gateway phase Stopped
    Suspended=True and Ready=False (ContainerStopped)
  end note
  note left of RESUMING
    gateway phase Provisioning
    Bootstrapping=True and Ready=False (ContainerStarting)
  end note
  note right of CRASHED
    gateway phase Error
    Ready=False (ContainerExited)
    resume is refused, revert or delete
  end note
```

An actor is born `SUSPENDED`, holding its template's golden snapshot. Resume
and suspend move it between `RUNNING` and `SUSPENDED` through a restore or a
checkpoint. If its worker pod disappears it is `CRASHED`; from there only
`RevertActor`, which puts it back at its last external snapshot, or delete are
allowed. The driver never issues a revert; the gateway sees `CRASHED` as Error
and an operator uses `kubectl-ate revert actor`.

## Sequences

### Creating a sandbox

```mermaid
sequenceDiagram
  autonumber
  participant GW as openshell-gateway
  participant D as openshell-driver-substrate
  participant API as ate-api-server
  participant R as template reconciler
  participant W as atelet → ateom-microvm
  participant S as object store

  GW->>D: ValidateSandboxCreate(sandbox)
  D-->>GW: ok, or FailedPrecondition when the image has no @sha256 digest
  GW->>D: CreateSandbox(sandbox)
  Note over D,API: name = "oshl-" + sha256(encoded template)[:8]<br/>inputs: image, command, env,<br/>gateway endpoint, sandbox config
  D->>API: CreateActorTemplate(name, spec)
  alt template is new
    API->>R: reconcile
    R->>API: create golden actor, ResumeActor
    API->>W: Restore from cold boot
    W->>W: boot the micro-VM, start the containers
    Note over R,W: sandbox clears its 7 gates, supervisor attaches,<br/>workload launched
    R->>R: wait 20 s (goldenSnapshotWarmup)
    R->>API: SuspendActor
    API->>W: Checkpoint
    W->>S: memory-ranges, rootfs-upper.tar, durable-dir.tar, state.json
    R->>API: tag the snapshot in atespace ate-golden
  else template exists
    API-->>D: AlreadyExists, same name means same inputs
  end
  loop every 2 s, up to template_ready_timeout_secs (180)
    D->>API: GetActorTemplate(name)
    API-->>D: status.goldenSnapshotStatus
  end
  D->>API: CreateActor(name = sandbox id, template)
  Note over D,API: actor starts SUSPENDED,<br/>holding the golden snapshot
  D->>API: ResumeActor
  API->>API: pick a free Worker
  API->>W: Restore
  W->>S: fetch snapshot
  W-->>API: RUNNING (≈ 350 ms)
  D-->>GW: CreateSandboxResponse
  loop watch_sandboxes: ListActors every 2 s, diffed
    D-->>GW: DriverSandbox with conditions
    Note over GW,D: derive_phase: Ready=True → Ready, Suspended=True → Stopped,<br/>Ready=False with a transient reason → Provisioning
  end
```

The first `create_sandbox` for a workload pays for the golden snapshot: about
30 s in all, a few seconds of boot, the fixed 20 s warm-up, and the checkpoint. Every later one, and every later
actor of the same template, is a restore. The gateway's five-minute
provisioning ceiling is comfortably above this.

### Reaching the agent, and the two egress layers

```mermaid
sequenceDiagram
  autonumber
  participant C as curl
  participant RT as atenet router
  participant RL as relay
  participant WL as agent.py<br/>in the sandbox
  participant SB as sandbox broker<br/>seccomp notify
  participant SU as supervisor<br/>OPA + proxy
  participant EG as atenet egress
  participant M as model

  C->>RT: CONNECT alice:8081, proxy-header ate-target-actor: atespace/alice
  RT->>RL: TCP to the actor's address :8081
  Note over RT,WL: the broker refuses accept(2) from a non-loopback peer,<br/>so the relay connects to the agent over 127.0.0.1
  RL->>WL: POST /chat
  WL->>SB: connect(2) to 172.18.0.1:11434 · intercepted
  SB->>SU: who, where: /usr/local/bin/python3.12 → host:port
  alt not in data.yaml network_policies
    SU-->>SB: deny
    SB-->>WL: EACCES · "network_broker: denied (syscall=42)"
  else allowed for this binary, host and port
    SU-->>SB: allow · OCSF NET:OPEN ALLOWED [policy:model engine:opa]
    SU->>EG: HTTP CONNECT 172.18.0.1:11434 (atunnel)
    alt no EgressPolicy on the actor
      EG-->>SU: 403 · the agent sees RemoteDisconnected
    else EgressPolicy allows 172.18.0.1/32
      EG->>M: TCP tunnel
      SU->>M: POST /v1/chat/completions · OCSF HTTP:POST ALLOWED
      M-->>WL: reply
    end
  end
  WL-->>C: {"reply": ..., "turns": n}
```

Ingress is Substrate's router, the relay, then loopback into the sandbox.
Egress is decided twice, in order. First OpenShell: the sandbox intercepts
the workload's `connect(2)` with seccomp user notification and asks the
supervisor, whose shipped policy allows a connection only when `data.yaml`
names the destination host, port, and the binary making the call; a denied
call fails with `EACCES` inside the sandbox and the broker logs it. Then
Substrate: the supervisor's allowed connection leaves the actor as an HTTP
`CONNECT` to atenet egress, which allows only the CIDRs or hostnames in the
actor's `EgressPolicy` and answers 403 otherwise. Without both allowances
the model is unreachable, which is the point: neither the workload nor the
supervisor can widen its own reach.

### Suspend and resume

```mermaid
sequenceDiagram
  autonumber
  participant OP as kubectl-ate or gateway
  participant API as ate-api-server
  participant AT as atelet
  participant AM as ateom-microvm
  participant S as object store

  OP->>API: SuspendActor(alice)
  Note over OP,API: RUNNING → SUSPENDING
  API->>AT: Checkpoint
  AT->>AM: CheckpointWorkload
  AM->>AM: pause the VM, dump guest memory, tar rootfs upper and durable dirs
  AM->>S: base-id, config.json, memory-ranges, rootfs-upper.tar, durable-dir.tar, state.json
  AM->>AM: tear the VM down · "Actor checkpointed"
  AT->>AT: resetActorDirs (needs CAP_DAC_OVERRIDE for files uid 65532 created)
  API->>API: SUSPENDED · worker assignment cleared, worker shows 0/1
  Note over OP,S: nothing runs, the snapshot is the only copy
  OP->>API: ResumeActor(alice)
  API->>API: SUSPENDED → RESUMING · pick a free Worker, maybe another pod
  API->>AT: Restore
  AT->>AM: RestoreWorkload
  AM->>S: fetch the snapshot
  AM->>AM: cloud-hypervisor restore, virtiofs lowers, tap · "Actor restored in ≈ 350 ms"
  API->>API: RUNNING
  Note over OP,S: process memory is back: turns and booted as they were
```

A checkpoint pauses the VM, writes guest memory plus the rootfs upper layer
and the durable dirs to the object store, and tears the VM down. The worker is
free at once. A resume picks any free worker, possibly on a different pod,
fetches the snapshot, and restores in about 350 ms. Process memory is exactly
as it was: the agent's chat history and its `booted` timestamp included.

### A host dies

```mermaid
sequenceDiagram
  autonumber
  participant OP as operator
  participant K8S as kube-apiserver
  participant CTL as ate-controller syncer
  participant API as ate-api-server
  participant DEP as worker Deployment
  participant AM as new ateom-microvm

  OP->>K8S: delete pod --grace-period=0 --force
  Note over OP,CTL: a plain delete gives 3600 s of grace and ateom waits<br/>for the guest, so the actor stays RUNNING on a DRAINING worker
  K8S-->>CTL: pod gone
  CTL->>API: DeleteWorker(worker)
  API->>API: drain, then release bound actors:<br/>"Releasing actor from a worker whose pod is gone"
  Note over CTL,API: alice RUNNING → CRASHED, assignment cleared<br/>bob on the other worker is untouched
  K8S->>DEP: replica count short
  DEP->>AM: create the replacement pod
  AM->>CTL: register
  CTL->>API: new Worker ACTIVE (about a second)
  OP->>API: RevertActor(alice)
  API->>API: CRASHED → REVERTING → SUSPENDED, holding the last external snapshot
  OP->>API: ResumeActor(alice)
  Note over OP,API: ResourceExhausted if it beats the registration, so retry
  API->>AM: Restore on the new worker
  API-->>OP: RUNNING · turns back at the last suspend
```

A worker pod that vanishes takes its actor to `CRASHED`, and the Deployment
replaces the pod within seconds. A plain `kubectl delete pod` does not model
this: the pool's grace period is an hour and ateom waits for the guest
workloads, so the actor stays `RUNNING` on a `DRAINING` worker. Recovery is
revert then resume, and everything since the last suspend is lost. A gateway
that suspends idle sandboxes bounds that loss to the idle window.

## Credentials

```mermaid
flowchart LR
  classDef file fill:#fef9c3,stroke:#ca8a04,color:#713f12
  classDef img fill:#eff6ff,stroke:#2563eb,color:#1e3a8a
  classDef vol fill:#fdf4ff,stroke:#a21caf,color:#701a75

  key["signing.key.pem<br/>Ed25519, any key is a trust anchor"]:::file
  gen["bootstrap-gen<br/>BOOTSTRAP_CHILD_ENV → workload env"]
  bs["bootstrap.json · server.crt · server.key<br/>BoundaryConfig: listener 127.0.0.1:17777,<br/>verification key, child_env"]:::file
  rd["runtime-descriptor.json<br/>SandboxRuntimeDescriptor: where the boundary is,<br/>its TLS trust anchor"]:::file
  ab["auth.json<br/>SupervisorAuthBundle: gateway and sandbox JWTs,<br/>expire in one hour"]:::file
  pkg["build.sh<br/>dirs 0777, files 0666 → bootstrap.tar"]
  simg["helpdesk-sandbox image<br/>python:3.12-slim + /openshell-sandbox<br/>+ agent.py, relay.py + bootstrap.tar"]:::img
  fimg["openshell-bootstrap-files image<br/>/supervisor/{runtime-descriptor.json, auth.json,<br/>policy.rego, data.yaml}"]:::img
  sbx["sandbox container<br/>reads and unlinks its bootstrap,<br/>so it must sit on the writable rootfs"]:::vol
  sup["supervisor container<br/>image volume at /.openshell, read-only is fine"]:::vol
  note["Substrate discards image file ownership,<br/>so the tree is world-writable instead of owned by 65532"]

  key --> gen
  gen --> bs
  gen --> rd
  gen --> ab
  bs --> pkg --> simg --> sbx
  rd --> fimg
  ab --> fimg
  fimg --> sup
  pkg -.- note
```

OpenShell's sandbox and supervisor authenticate to each other with a pair of
Ed25519-signed JWTs and TLS material bound to one session id. In a normal
deployment the gateway mints them per sandbox and refreshes them. Here
`harness/bootstrap-gen` mints them with a local key, valid for one hour,
OpenShell's maximum, and the helpdesk images carry them: the sandbox's half
baked into the writable rootfs (it consumes its bootstrap), the supervisor's
half as a read-only image volume. `SandboxLaunchAuthentication` validates
against the key it embeds, so any key is an accepted trust anchor.

This is where the gateway path stops. The gateway's credentials are per
sandbox, a template is shared, and a snapshot freezes memory, so there is no
place in Substrate today to hand an actor a per-sandbox secret before its
first boot. The synthesized one-container template therefore has no supervisor,
no supervisor session forms, and the gateway holds the sandbox at Provisioning.
Two ways out, both outside this repository: a per-actor secret or file source
in Substrate, written before first boot and not into the golden snapshot; or
an OpenShell bootstrap that fetches its own per-sandbox credentials after
restore, so the snapshot holds nothing per sandbox.

## What a snapshot is, and what it is not

A snapshot is the whole VM: guest memory, the rootfs upper layer, the durable
dirs. It is not a health check. A container that died inside the VM before the
snapshot comes back dead in every restore, and Substrate reports the actor
`RUNNING` all the same. The golden snapshot is taken at a fixed 20 s, whether
or not the supervisor has attached by then. Read the worker pod's log:
`Boundary control listener ready`, `Isolation boundary attached` and
`PROC:LAUNCH` are the signs of a healthy golden actor.

Templates and their golden snapshots are never garbage-collected. The
driver's content-hash naming keeps their number equal to the number of
distinct workloads; the helpdesk's `run.sh` names its template from a hash of
the rendered template, image digests included, for the same reason.
