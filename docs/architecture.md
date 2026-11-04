# Architecture

How an OpenShell sandbox becomes an Agent Substrate actor, what runs where,
and what each piece trusts.

## In one paragraph

OpenShell's gateway creates and drives sandboxes through a `ComputeDriver`
gRPC contract. This repository is one such driver, running as its own process
on a Unix socket. It maps each gateway call onto Substrate's `Control` API
(`substrate.ateapi.v1`): a sandbox is an `Actor`, a workload is one
`ActorTemplate`, and starting a sandbox is a restore of that template's golden
snapshot into a micro-VM. Inside the VM, stock OpenShell binaries run
unmodified: `openshell-sandbox` holds the workload and `openshell-supervisor`
enforces policy and audits. A launcher in the image fetches each sandbox's
gateway-minted credentials from the driver after the restore, so one golden
snapshot serves every sandbox and a stock gateway takes one to `Ready`.
Nothing in OpenShell, Substrate, the guest kernel or kata is forked or
patched; the one gap the launcher works around, `no_new_privs`, is in
[shortcomings.md](shortcomings.md) with the rest.

## The big picture

```mermaid
%%{init: {"flowchart": {"wrappingWidth": 260, "nodeSpacing": 30, "rankSpacing": 35}}}%%
flowchart TB
  classDef stock fill:#ecfdf5,stroke:#059669,color:#064e3b
  classDef ours fill:#dbeafe,stroke:#1d4ed8,color:#1e3a8a,stroke-width:2px
  classDef sub fill:#fff7ed,stroke:#ea580c,color:#7c2d12
  classDef guest fill:#fdf4ff,stroke:#a21caf,color:#701a75
  classDef store fill:#f1f5f9,stroke:#475569,color:#0f172a
  classDef edge fill:#f0fdf4,stroke:#16a34a,color:#14532d

  cli["openshell CLI"]
  kate["kubectl-ate"]

  subgraph openshell["NVIDIA OpenShell, stock"]
    gw["openshell-gateway<br/>--compute-driver substrate"]
  end

  subgraph repo["this repository"]
    driver["openshell-driver-substrate<br/>ComputeDriver on a Unix socket<br/>launch endpoint for the bundles"]
  end

  subgraph substrate["Substrate control plane"]
    ctl["ate-controller<br/>WorkerPool → worker pods, syncer"]
    api["ate-api-server<br/>Control: ActorTemplate, Actor,<br/>Worker, EgressPolicy"]
    pg[("postgres")]
    recon["template reconciler<br/>golden actor and snapshot"]
    ctl --> api
    api --- pg
    api --> recon
  end

  atelet["atelet, one per node<br/>images, OCI specs, durable dirs<br/>Run, Checkpoint, Restore"]

  subgraph pod["worker pod"]
    ateom["ateworker-microvm<br/>cloud-hypervisor, virtiofsd,<br/>kata-agent<br/>wakeup probes"]
  end

  subgraph vm["one actor, one micro-VM, 169.254.17.2"]
    sbx["sandbox container<br/>launcher → openshell-sandbox → workload<br/>uid 65532, no capabilities, Landlock, seccomp"]
    sup["supervisor container<br/>launcher → openshell-supervisor<br/>boundary on :17777, OPA, policy DNS, proxy, audit"]
    sbx -- "boundary, TLS" --> sup
  end

  store[("object store<br/>golden and actor snapshots")]
  egress["atenet egress<br/>the actor's EgressPolicy only"]
  model["model endpoint, optional<br/>ollama, the one allowed host"]

  cli --> gw
  gw -- "compute_driver.proto" --> driver
  driver -- "Control RPCs" --> api
  kate -- "Control" --> api
  api -- "Restore, Checkpoint" --> atelet
  ctl -. "creates, replaces" .-> pod
  atelet -- "RunWorkload, RestoreWorkload,<br/>CheckpointWorkload" --> ateom
  ateom -- "restore ≈ 330 ms" --> vm
  ateom <-- "snapshot files" --> store
  sup -- "allowed connections, atunnel" --> egress
  egress --> model
  sup -. "GET /v1/launch, supervisor session, exec" .-> egress
  egress -. "launch request + actor JWT" .-> driver
  egress -. "supervisor session" .-> gw

  class gw stock
  class driver ours
  class api,pg,recon,ctl,atelet,ateom sub
  class egress edge
  class sbx,sup guest
  class store,model store
```

Colors: green is stock OpenShell, blue is this repository, orange is
Substrate, purple is inside the guest, light green is Substrate's network edge.

Three trust domains meet here:

| Domain | Runs | Trusts |
|---|---|---|
| OpenShell gateway and this driver | one pod in the atespace (`harness/scripts/gateway-up.sh`), or the driver alone on a host | `ate-api-server` over TLS with a bearer token whose audience is `api.ate-system.svc`; the IdP's JWKS for the actor JWTs on the launch endpoint |
| Substrate control plane and node agents | namespace `ate-system`, one atelet per node, one ateworker per worker pod | Kubernetes RBAC, image digests, the object store |
| The guest | one micro-VM per actor | its own kernel and cloud-hypervisor; inside it, the sandbox trusts only the gateway keys in its bootstrap and the actor uid Substrate writes under `/run/ate`, and the supervisor the gateway's CA from its bundle |

## Components

### openshell-gateway, stock

With `--compute-driver-socket`, the gateway binds the name given to
`--compute-driver` to that socket, whether or not a built-in driver has the
same name. The gateway keeps
the sandbox catalog, mints the per-sandbox launch credentials, and derives a
sandbox's phase from the conditions the driver reports (`derive_phase`):
`Ready=True` is Ready, `Suspended=True` without `Ready=True` is Stopped, a
`Ready=False` with a reason on its transient allowlist is Provisioning, and
one with any other reason is a sticky Error. Provisioning has 1800 s to
reach the supervisor's first report and 300 s after it to reach Ready
(`ProvisioningTimedOut`); after Ready there is no deadline.

### openshell-driver-substrate, this repository

`src/main.rs` parses flags (or `SUBSTRATE_*` and `OPENSHELL_*` environment
variables), loads the configuration file (`--config`: the ActorTemplate
fragment, the egress rules, the `[launch]` endpoint and the `[gateway]` the
supervisors dial; `src/configfile.rs`, protojson YAML decoded in
`src/fragment.rs`), binds the socket, serves `ComputeDriverServer` with
reflection, and the launch endpoint.

`src/lib.rs` implements the trait:

| ComputeDriver call | What the driver does |
|---|---|
| `get_capabilities` | Checks the gateway's extension metadata, names itself `substrate` and advertises `openshell.compute.suspend-resume`; `gateway_manages_lifecycle`, `supports_sandbox_authentication` and `driver_reports_runtime_readiness` are false; CPU and memory limits are supported, GPUs are not. |
| `validate_sandbox_create` | Requires `spec.template.image` with an `@sha256:` digest (ate-api-server rejects an unpinned image; the check saves a round trip) and resource limits that parse as Kubernetes quantities; rejects GPU requests and `user_namespaces`. |
| `create_sandbox` | Names the template, `ensure_ready` (below), `CreateActor` (an `AlreadyExists` for an actor of the same template is a retried create and is fine), `CreateActorEgressPolicy` with the configured rules (before the first resume: the egress caches a missing policy), the bundle from `spec.launch_authentication`, bound to the actor's uid, into the store, `ResumeActor`. Refused without launch authentication. |
| `start_sandbox` | The bundle of the new generation into the store, the stopped mark cleared, `ResumeActor`, which is a no-op on an actor Substrate already resumed. |
| `stop_sandbox`, `suspend_sandbox` | The stopped mark set, a `.stopped` file next to the bundle, then `SuspendActor` with synchronous durability: checkpoint, upload, free the worker, keep the snapshot. A `CRASHED` actor gets `RevertActor` instead, back to its last snapshot. |
| `delete_sandbox` | `DeleteActor`; Substrate deletes from any state. The bundle and the stopped mark are dropped. The template stays; other actors may share it. |
| `get_sandbox`, `list_sandboxes` | `GetActor`; `ListActors` walked to the last page (1000 per page), keeping the actors of `oshl-` templates. The gateway's name for a sandbox comes from its bundle; the actor is named by the sandbox id. |
| `watch_sandboxes` | Substrate has no watch RPC. The driver polls `ListActors` every 2 s, diffs against the last snapshot, and emits changed and deleted sandboxes. A running actor the gateway stopped, which Substrate resumed on its own, is suspended again from here. It stops when the gateway drops the stream. |
| `ensure_workspace` | `GetAtespace`, then `CreateAtespace` when it is missing, for the one configured atespace. The trait carries workspace identity only on create, so one atespace serves every workspace. |
| `delete_workspace` | No-op. |
| `authenticate_sandbox` | Unimplemented. |

One `Control` channel is dialed lazily and reused. An interceptor adds
`Authorization: Bearer` from a token file read on every client build, so a
rotated projected token needs no restart. TLS is on when a CA bundle is
configured; a client certificate is optional and must come as a pair. A stop,
a start and the suspend of a resumed actor take one lock per actor. At
startup the driver drops the bundles of actors that no longer exist.

Conditions are the driver's only voice to the gateway, so `actor_state_conditions`
is written against the gateway's phase rules and pinned by a table test:

| Actor state | Conditions reported | Gateway phase |
|---|---|---|
| RUNNING, not stopped by the gateway | `Ready=True` (ContainerRunning) | Ready |
| RESUMING, REVERTING, not stopped by the gateway | `Bootstrapping=True`, `Ready=False` (ContainerStarting) | Provisioning; `Bootstrapping=True` is what lets a Stopped sandbox start |
| SUSPENDING, after the gateway's stop or suspend | `Suspended=False` (PodTerminating, the reason the gateway reads as a stop in flight) | Provisioning |
| RUNNING, RESUMING, REVERTING, SUSPENDED, after the gateway's stop or suspend and before its next start | `Suspended=True`, `Ready=False` (ContainerStopped) | Stopped, or Suspended after the gateway's own suspend, which its lifecycle controller keeps. A resume Substrate did on its own is undone within a watch tick |
| SUSPENDING, SUSPENDED, by Substrate alone | `Ready=False` (DependenciesNotReady) | Provisioning: the gateway keeps the credentials, and the supervisor's return after a Substrate resume makes it Ready again |
| CRASHED | `Ready=False` (ContainerExited) | Error, on purpose: it needs `RevertActor` |
| DELETING | `Ready=False` (ContainerStopped); the status `deleting` flag is set | Deleting |

The stopped mark goes on before `SuspendActor` and off before `ResumeActor`,
and lives as a `.stopped` file next to the bundle, so a stop cut short and a
driver restart keep it.

### Template synthesis, `src/template.rs`

Everything the golden snapshot depends on goes into the template, and nothing
else: the fragment, the image, the worker selector, and the resource limits
(the sandbox template's `cpu_limit` and `memory_limit` as Kubernetes
quantities, the fragment's, or the driver's defaults). The request's
environment stays out: the bundle carries it to the workload. The template
is encoded with prost, its metadata blanked, and hashed; the name is `oshl-`
plus the first eight bytes of the SHA-256. Two sandboxes with the same inputs
share one template and one golden snapshot. Any change to what the template
contains gets a new one.

Nothing per sandbox may be in a template. A snapshot freezes process memory,
so a per-sandbox value baked into it would come back identical in every actor
restored from it. The sandbox id and the gateway-minted token are left out for
that reason, and a test asserts it. The per-sandbox part arrives after the
restore; see [Credentials](#credentials).

The shape comes from the configuration file's fragment: its containers, the
one named `sandbox` with the request's image, and its volumes, snapshot
settings and limits when it sets them. A fragment may not name the template,
pick the pool or add a capability, and must have a `sandbox` container. The
harness fragment is two containers (sandbox, supervisor), two durable dirs
and the identity volume. Every container drops all capabilities
(`openshell-sandbox` refuses to run with any capability in its bounding set),
the template takes memory snapshots, and a worker selector names the pool
that decides the sandbox class (micro-VM in the harness). Substrate mounts a
tmpfs at `/tmp`, where the Landlock probe writes.

`ensure_ready` creates the template, tolerates `AlreadyExists`, then polls
`GetActorTemplate` every 2 s until `status.goldenSnapshotStatus.goldenTag` is
set, or `error_message` is set (FailedPrecondition; the template is deleted so
the next create rebuilds it), or 180 s pass (DeadlineExceeded).

### Substrate control plane

- **ate-api-server** owns the resources (`Atespace`, `ActorTemplate`, `Actor`,
  `Worker`, `EgressPolicy`, `Tag`) in postgres and runs the workflows: resume
  picks a Worker and asks its node's atelet to restore; suspend asks for a
  checkpoint and clears the assignment; revert puts a crashed or running actor
  back at its last external snapshot; delete tears down from any state. It
  speaks gRPC over TLS with the `servicedns` trust bundle and a bearer token;
  it needs no client certificate.
- **template reconciler**, inside ate-api-server, gives a new template its
  golden snapshot: it creates one golden actor, resumes it from cold boot,
  waits until every container's wakeup probe answers 200 (a fixed 20 s when
  a container has none), suspends it, and tags the snapshot in atespace
  `ate-golden`. Whatever the VM looked like then is what every actor starts
  from.
- **ate-controller** turns a `WorkerPool` into a Deployment of worker pods and
  runs the syncer: a new pod registers as a Worker, a terminating pod marks
  its Worker `DRAINING`, a vanished pod deletes its Worker, which releases the
  actors bound to it into `CRASHED`.
- **atelet** is a DaemonSet, one per node. It pulls images by digest into a
  cache, builds the OCI specs (capabilities as the template says, the image's
  `USER` and `WORKDIR`, and on micro-VM the sysctl that lifts the guest's
  privileged-port limit), writes the actor's `/etc/resolv.conf` and
  `/etc/hosts` into the rootfs base layer, lays out durable dirs, and drives
  the node's workers: Run, Checkpoint, Restore.
- **ateworker-microvm** is the worker pod's process: it launches
  cloud-hypervisor, serves the rootfs layers over virtiofsd, talks to the
  kata-agent in the guest to start containers, and checkpoints or restores the
  whole VM. A worker hosts as many actors as its capacity fits; the helpdesk
  template asks for a worker's whole CPU, so its pool of two gives two
  concurrent actors.
- **object store**: rustfs in kind, GCS on GKE. A snapshot is `base-id`,
  `config.json`, `memory-ranges`, one rootfs upper tar per container, one tar
  per durable dir, and `state.json`. A restore is about 330 ms.
- **atenet** is one binary in two roles. The egress terminates the actor's
  outbound `CONNECT`s and allows only the hosts and ports the actor's
  `EgressPolicy` names, 403 otherwise. The router, Substrate's mTLS ingress,
  has no way into a sandbox: the sandbox's network broker refuses `accept(2)`
  from a non-loopback peer, so the workload is reached through the gateway.

### Inside the micro-VM

One guest, one network namespace with the address `169.254.17.2`, two
containers from the harness fragment (`harness/gateway/template.yaml.tmpl`):
the request's image in the sandbox container, the runtime image (`runtime/`)
in the supervisor's. `launcher.py` is the first process of both, uid 65532
with no capability, and works from the bundle it fetches at `LAUNCH_URL`.
Each launcher answers Substrate's wakeup probe on its own port (17901,
17902), so an activation completes only when both are up, and the golden
snapshot is taken as soon as they are.

- **sandbox**: the launcher points `/etc/resolv.conf` at the sandbox's DNS
  relay (the image's `/etc` belongs to that user), waits for the supervisor's
  listener and for the generation the supervisor published, copies that
  generation's bootstrap and TLS pair into `/.openshell/channel/sandbox`, and
  starts `openshell-sandbox` under `setpriv --no-new-privs`, a `prctl` any
  process may make for itself. `openshell-sandbox` reads its bootstrap and
  unlinks it, checks the bundle's `substrate.actor_uid` claim against
  `/run/ate/actor-uid`, refuses to run unless it is non-root, capability-free
  and `no_new_privs`, and probes the kernel for Landlock, seccomp user
  notification, socket virtualization, loopback confinement and a DNS relay
  bind. Then it dials its supervisor at `169.254.17.2:17777` and serves TLS
  over that connection. It forks the workload under seccomp and Landlock and
  brokers the workload's network calls. A new generation restarts it.
- **supervisor**: the launcher reads the actor's name and atespace from the
  `systemInfo` mount on every poll (the files are right after a restore from
  the shared snapshot), long-polls the driver's launch endpoint, writes the
  runtime descriptor, the auth bundle and the gateway CA, and starts
  `openshell-supervisor` on them with `OPENSHELL_ENDPOINT`, the sandbox id
  and name, the main-process spec, the SSH relay socket (on the container's
  own filesystem: a durable dir takes no Unix socket) and
  `OPENSHELL_ADMITTED_ISOLATION_BACKEND=openshell-sandbox`.
  The supervisor listens where its descriptor says, authenticates the
  sandbox's token pair, opens its session to the gateway, fetches the policy
  from it and loads it into Open Policy Agent (OPA), answers the workload's
  DNS for the hosts the policy names, installs a TLS-interception certificate
  authority (CA) the workload trusts, starts the workload through the sandbox,
  decides every connection, proxies the allowed ones, serves `exec` and
  `connect` over its SSH relay, and writes Open Cybersecurity Schema
  Framework (OCSF) audit lines.

The boundary channel runs from the sandbox to the supervisor. OpenShell
refuses a boundary listener on a loopback address and a supervisor peer whose
address is the sandbox's own, and the two containers share one network
namespace, so the supervisor is the listener, on the actor's address, and the
sandbox dials it (`RuntimeInitiatedTlsTcp`). A Unix socket on a shared durable
dir is no alternative: it is not shared across the containers (`connect(2)`
gives `ECONNREFUSED`).

```mermaid
%%{init: {"sequence": {"wrap": true}}}%%
sequenceDiagram
  autonumber
  participant A as ateworker-microvm
  participant K as kata-agent
  participant SB as sandbox container
  participant SU as supervisor container
  participant WL as workload

  A->>K: create the containers from the OCI specs
  K->>SB: start launcher.py sandbox
  K->>SU: start launcher.py supervisor
  Note over A,WL: the wakeup probes poll GET /healthz on 17902 and 17901 until each answers 200, which the launchers do as soon as they poll
  Note over SB,SU: the supervisor's launcher fetches the bundle first (see Credentials)
  SU->>SU: start openshell-supervisor
  Note over SU,WL: on the descriptor and the auth bundle, with OPENSHELL_ENDPOINT and the sandbox id
  SU->>SU: listen on 169.254.17.2:17777
  Note over SU,WL: "Runtime-initiated Sandbox Protocol listener ready"
  SB->>SB: nameserver 127.0.0.53, wait for the listener
  SB->>SB: setpriv --no-new-privs openshell-sandbox, uid 65532 from the image
  SB->>SB: read bootstrap.json and unlink it, check /run/ate/actor-uid against the claim
  Note over SB,SU: qualification gates: non-root uid and gid, empty capability sets, no_new_privs, task-memory probe, Landlock allow and deny under /tmp, seccomp notification, socket virtualization, DNS relay bind, loopback confinement
  SB->>SU: dial the boundary, serve TLS on the connection
  SU->>SB: present the sandbox token
  SB-->>SU: verified against the embedded key, "Isolation boundary attached"
  SU->>SU: load the policy into OPA, from the gateway
  SB->>SB: /run/openshell-supervisor-ca for the CA, "Landlock ruleset built"
  SU->>SB: start_agent(python3 /opt/helpdesk/agent.py, child_env)
  SB->>WL: fork, apply seccomp and Landlock, exec, "PROC:LAUNCH python3"
  WL->>WL: listen on 0.0.0.0:8080
  A-->>A: both probes answered: the actor is RUNNING
  Note over A,WL: the golden actor holds two launchers still polling, and all of this happens per actor after the restore
```

## Lifecycle

```mermaid
stateDiagram-v2
  direction TB
  [*] --> SUSPENDED: CreateActor
  SUSPENDED --> RESUMING: ResumeActor (create_sandbox, start_sandbox)
  RESUMING --> RUNNING: restored
  RUNNING --> SUSPENDING: SuspendActor (stop_sandbox, suspend_sandbox)
  SUSPENDING --> SUSPENDED: checkpointed, worker freed
  RUNNING --> CRASHED: worker pod gone
  CRASHED --> REVERTING: RevertActor (stop_sandbox)
  REVERTING --> SUSPENDED: last external snapshot
  RUNNING --> DELETING: DeleteActor (delete_sandbox)
  SUSPENDED --> DELETING: DeleteActor
  CRASHED --> DELETING: DeleteActor
  DELETING --> [*]

  note right of RUNNING
    gateway phase Ready
    Ready=True (ContainerRunning)
    after a gateway stop: Stopped, and suspended again
  end note
  note left of SUSPENDED
    Stopped after the gateway's stop, Suspended after its suspend
    Suspended=True, Ready=False (ContainerStopped)
    by Substrate alone: Provisioning
    Ready=False (DependenciesNotReady), credentials kept
  end note
  note left of RESUMING
    gateway phase Provisioning
    Bootstrapping=True, Ready=False (ContainerStarting)
  end note
  note right of CRASHED
    gateway phase Error
    Ready=False (ContainerExited)
    revert or delete only
  end note
```

An actor is born `SUSPENDED`, holding its template's golden snapshot. Resume
and suspend move it between `RUNNING` and `SUSPENDED` through a restore or a
checkpoint. If its worker pod disappears it is `CRASHED`; from there only
`RevertActor`, which puts it back at its last external snapshot, or delete are
allowed. The gateway sees `CRASHED` as Error; its `stop` makes the driver
revert the actor, and the next `start` restores it.

The gateway's own suspend (`openshell sandbox suspend`) goes through
`suspend_sandbox` and shows as phase Suspended; `resume` and `start` mint a
new generation and restore the actor. Inside, the launcher replaces the
supervisor and the sandbox, so the workload restarts: OpenShell v0.1.7 gives a
new-generation supervisor no way onto a running sandbox (the sandbox keeps its
first TLS identity and session id in memory, and `openshell-sandbox` does not
implement `SuspendBoundary`). The restore itself is still a restore, not a
boot. A suspend from the Substrate side (`kubectl-ate suspend`) keeps the
whole VM, workload memory included. The driver reports it as a wait, not a
stop: the gateway shows Provisioning, keeps the sandbox's credentials and
admits only `delete`, and is Ready again, memory intact, when Substrate
resumes the actor and the supervisor reconnects. A stop would retire the
credentials and force a new generation. A resume from the Substrate side of
a sandbox the gateway stopped is undone: the driver keeps reporting Stopped
and suspends the actor again within a watch tick; a `start` in that window
hands the running actor a new generation without a restore.

## Sequences

### Creating a sandbox

```mermaid
%%{init: {"sequence": {"wrap": true}}}%%
sequenceDiagram
  autonumber
  participant GW as openshell-gateway
  participant D as driver
  participant API as ate-api-server
  participant R as template reconciler
  participant W as atelet → ateworker-microvm
  participant S as object store

  GW->>D: ValidateSandboxCreate
  D-->>GW: ok, or InvalidArgument when the image has no @sha256 digest
  GW->>D: CreateSandbox(sandbox, launch_authentication)
  Note over D,API: name = "oshl-" + sha256(encoded template)[:8], inputs: the fragment, image, worker selector, limits
  D->>API: CreateActorTemplate(name, spec)
  alt template is new
    API->>R: reconcile
    R->>API: create the golden actor, ResumeActor
    API->>W: Restore from cold boot
    W->>W: boot the micro-VM, start the containers
    Note over R,W: the golden actor holds two launchers polling for a bundle they never get
    R->>R: wait until every wakeup probe answers 200
    R->>API: SuspendActor
    API->>W: Checkpoint
    W->>S: memory ranges, rootfs uppers, durable dirs, state.json
    R->>API: tag the snapshot in atespace ate-golden
  else template exists
    API-->>D: AlreadyExists, same name means same inputs
  end
  loop every 2 s, up to 180 s
    D->>API: GetActorTemplate(name)
    API-->>D: status.goldenSnapshotStatus
  end
  D->>API: CreateActor(name = sandbox id, template)
  Note over D,API: the actor starts SUSPENDED, holding the golden snapshot
  D->>API: GetActor, for the uid the bundle is bound to
  D->>API: CreateActorEgressPolicy(the configured rules)
  Note over GW,API: the bundle from launch_authentication, bound to the actor's uid, kept on disk
  D->>API: ResumeActor
  API->>API: pick a free Worker
  API->>W: Restore
  W->>S: fetch the snapshot
  W-->>API: RUNNING once the wakeup probes answer, restore ≈ 330 ms
  D-->>GW: CreateSandboxResponse
  Note over D,W: the launcher in the actor fetches the bundle and starts the supervisor and the sandbox, and the supervisor's session takes the gateway to Ready (see Credentials)
  loop watch_sandboxes: ListActors every 2 s, diffed
    D-->>GW: DriverSandbox with conditions
    Note over GW,D: derive_phase: Ready=True → Ready, Suspended=True → Stopped, Ready=False with a transient reason → Provisioning
  end
```

The first `create_sandbox` for a workload pays for the golden snapshot: a few
seconds of boot, the wakeup probes, and the checkpoint. Every later one, and
every later actor of the same template, is a restore,
and `Ready` follows about two seconds later, once the launcher has the bundle
and the supervisor's session is up. The gateway's provisioning ceiling is far
above this.

### Reaching the agent, and the two egress layers

```mermaid
%%{init: {"sequence": {"wrap": true}}}%%
sequenceDiagram
  autonumber
  participant C as openshell sandbox exec
  participant SU as supervisor
  participant X as agent.py chat, in the sandbox
  participant WL as agent.py
  participant SB as sandbox broker
  participant EG as atenet egress
  participant M as model

  C->>SU: exec, through the gateway and the supervisor's SSH relay
  SU->>X: start the client inside the sandbox
  X->>WL: POST /chat on 127.0.0.1:8080
  Note over X,SB: a loopback peer, which the broker allows
  Note over WL,SU: the model is ollama.<atespace>.svc.cluster.local, the one host data.yaml allows
  WL->>SB: resolve the model's name at 127.0.0.53
  SB->>SU: the query
  alt the host is not in data.yaml network_policies
    SU-->>SB: refused, OCSF NET:REFUSE [policy_dns_ineligible]
    SB-->>WL: the name does not resolve, the connect fails with EACCES
  else the host is allowed
    SU-->>SB: the address, "Policy DNS mapped ..."
    WL->>SB: connect(2) to it, intercepted
    SB->>SU: who and where: /usr/local/bin/python3.12 → host:11434
    SU-->>SB: allow, OCSF NET:OPEN ALLOWED [policy:model engine:opa]
    SU->>EG: HTTP CONNECT to the model's name, port 11434 (atunnel)
    alt no EgressPolicy on the actor
      EG-->>SU: 403, the agent sees RemoteDisconnected
    else EgressPolicy allows http to that host on 11434
      EG->>M: TCP tunnel
      SU->>M: POST /v1/chat/completions, OCSF HTTP:POST ALLOWED
      M-->>WL: reply
    end
  end
  WL-->>X: {"reply": ..., "turns": n}
  X-->>C: printed, the exit code is the client's
```

Ingress is the gateway: `openshell sandbox exec` runs the agent's own client
inside the sandbox, over the supervisor's SSH relay, and that client talks
to the agent over loopback. Substrate's router has no path in; see atenet
above. Egress is decided twice, in order. First OpenShell: the supervisor's policy
DNS answers only for the hosts `data.yaml` names, the sandbox intercepts the
workload's `connect(2)` with seccomp user notification and asks the
supervisor, whose shipped policy allows a connection only when `data.yaml`
names the destination host, port, and the binary making the call; a denied
call fails with `EACCES` inside the sandbox and the broker logs it. Then
Substrate: the supervisor's allowed connection leaves the actor as an HTTP
`CONNECT` to atenet egress, which allows only the hosts and ports in the
actor's `EgressPolicy` (DNS names, never addresses) and answers 403 otherwise.
Without both allowances the model is unreachable, which is the point: neither
the workload nor the supervisor can widen its own reach.

### Suspend and resume

```mermaid
%%{init: {"sequence": {"wrap": true}}}%%
sequenceDiagram
  autonumber
  participant OP as kubectl-ate or gateway
  participant API as ate-api-server
  participant AT as atelet
  participant AM as ateworker-microvm
  participant S as object store

  OP->>API: SuspendActor(alice)
  Note over OP,API: RUNNING → SUSPENDING
  API->>AT: Checkpoint
  AT->>AM: CheckpointWorkload
  Note over AT,S: pause the VM, dump guest memory, tar the rootfs uppers and the durable dirs
  AM->>S: base-id, config.json, memory-ranges, rootfs-upper-*.tar, durable-dir-*.tar, state.json
  AM->>AM: tear the VM down, "Actor checkpointed"
  Note over AT,AM: resetActorDirs, with CAP_DAC_OVERRIDE for the files uid 65532 created
  Note over OP,AT: SUSPENDED, the worker assignment cleared, the worker shows 0/1000
  Note over OP,S: --mode sync returns after the upload, the default once the node holds the snapshot
  OP->>API: ResumeActor(alice)
  Note over OP,AT: SUSPENDED → RESUMING, a free Worker, maybe another pod
  API->>AT: Restore
  AT->>AM: RestoreWorkload
  AM->>S: fetch the snapshot
  Note over AT,S: cloud-hypervisor restore, virtiofs lowers, tap, "Actor restored" in about 330 ms
  AM->>AM: wakeup probes until each answers 200
  API->>API: RUNNING
  Note over OP,S: process memory is back: turns and booted as they were
  Note over OP,S: through the gateway a new generation follows. The launcher fetches its bundle and restarts the supervisor and the sandbox, so the workload starts again
```

A checkpoint pauses the VM, writes guest memory plus the rootfs upper layers
and the durable dirs to the object store, and tears the VM down. The worker is
free at once. A resume picks any free worker, possibly on a different pod,
fetches the snapshot, and restores in about 330 ms. Process memory is exactly
as it was: the agent's chat history and its `booted` timestamp included. When
the gateway drives the resume, its new generation then replaces the
supervisor and the sandbox inside the restored VM, and the workload starts
again; see [Lifecycle](#lifecycle).

### A host dies

```mermaid
%%{init: {"sequence": {"wrap": true}}}%%
sequenceDiagram
  autonumber
  participant OP as operator
  participant K8S as kube-apiserver
  participant CTL as ate-controller syncer
  participant API as ate-api-server
  participant DEP as worker Deployment
  participant AM as new ateworker-microvm

  OP->>K8S: delete pod --grace-period=0 --force
  Note over OP,CTL: a plain delete gives 3600 s of grace and ateworker waits for the guest, so the actor stays RUNNING on a DRAINING worker
  K8S-->>CTL: pod gone
  CTL->>API: DeleteWorker(worker)
  Note over CTL,API: drain, then release the bound actors: "Releasing actor from a worker whose pod is gone"
  Note over CTL,API: alice RUNNING → CRASHED, assignment cleared. bob on the other worker is untouched
  K8S->>DEP: replica count short
  DEP->>AM: create the replacement pod
  AM->>CTL: register
  CTL->>API: new Worker ACTIVE, about a second
  OP->>API: RevertActor(alice)
  Note over API,DEP: CRASHED → REVERTING → SUSPENDED, holding the last external snapshot
  OP->>API: ResumeActor(alice)
  Note over OP,API: ResourceExhausted if it beats the registration, so retry
  API->>AM: Restore on the new worker
  API-->>OP: RUNNING, turns back at the last suspend
```

A worker pod that vanishes takes its actor to `CRASHED`, and the Deployment
replaces the pod within seconds. A plain `kubectl delete pod` does not model
this: the pool's grace period is an hour and ateworker waits for the guest
workloads, so the actor stays `RUNNING` on a `DRAINING` worker. Recovery is
`RevertActor` then a resume, which through the gateway is Error, `stop`,
`start`; everything since the last snapshot is lost, and the new generation
restarts the workload. A gateway that suspends idle sandboxes bounds that
loss to the idle window.

## Credentials

OpenShell's sandbox and supervisor authenticate to each other with a pair of
Ed25519-signed JWTs and TLS material bound to one session id, and the
supervisor authenticates to the gateway with a token from the same bundle.
The gateway mints a bundle per sandbox and per start (`launch_authentication`
on `CreateSandbox` and `StartSandbox`), with a new runtime generation and
auth epoch each time. A template is shared and a snapshot freezes memory, so
none of this may be in the golden snapshot. It reaches the actor after the
restore:

```mermaid
%%{init: {"sequence": {"wrap": true}}}%%
sequenceDiagram
  autonumber
  participant GW as openshell-gateway
  participant D as driver
  participant EG as atenet egress
  participant L as launcher (supervisor container)
  participant SU as supervisor
  participant SB as sandbox container

  GW->>D: CreateSandbox or StartSandbox: the launch authentication, the generation
  Note over GW,EG: the bundle: auth.json and runtime-descriptor.json for the supervisor, bootstrap.json and a fresh TLS pair for the sandbox, the gateway endpoint and CA, the main-process spec, the actor uid as a resource claim. On disk, mode 0600
  Note over D,EG: CreateActorEgressPolicy on the first create, then ResumeActor
  Note over L,SU: restored from the golden snapshot, the launcher still polling
  L->>L: read /run/ate/actor-name and atespace, rewritten on every restore
  L->>EG: GET /v1/launch, a long poll
  Note over D,L: http://openshell-driver:17790/v1/launch/<atespace>/<name>?generation=<current>
  EG->>D: the same request with Authorization: Bearer <actor JWT>
  Note over D,EG: the EgressPolicy rule replaces the header with the actor JWT, audience = the driver
  Note over GW,EG: the driver verifies the JWT against the IdP's JWKS: issuer, audience, sub = actor/<atespace>/<name>, and the bundle's actor uid
  D-->>L: the bundle, or 204 after a 20 s long poll when nothing is newer
  L->>SU: write the files, stop the previous supervisor, start the new one
  Note over L,SU: OPENSHELL_ENDPOINT, OPENSHELL_TLS_CA, the sandbox id and name, the main-process spec
  SU->>GW: ConnectSupervisor through the egress (tlsPassthrough rule), "supervisor session: accepted"
  L->>SB: publish the generation
  Note over SU,SB: the sandbox launcher copies bootstrap.json, server.crt and server.key into /.openshell/channel/sandbox and restarts openshell-sandbox, which checks the claim against /run/ate/actor-uid
  SB->>SU: dial 169.254.17.2:17777, serve TLS, "Isolation boundary attached"
  SU->>GW: configuration report, Ready
```

Four things make this safe. The egress mints the actor JWT only for that
actor's own request, so the driver knows who asks without a shared secret in
any image. The driver hands out only that actor's bundle, and only to a
caller with that actor's token. The bundle files are mode 0600 on the
writable rootfs or a durable dir, never in the golden snapshot; the sandbox
consumes its bootstrap on read. And the bundle is bound to the actor's uid:
the launch endpoint refuses a token whose `actor.ate.dev` uid is not the
bundle's, and the sandbox refuses a bundle whose `substrate.actor_uid` claim
does not match `/run/ate/actor-uid`, so material for an earlier actor of the
same name is never accepted. The limits of this, the plain HTTP hop, the
bearer token, one uid in both containers and the credentials inside per-actor
snapshots, are in [shortcomings.md](shortcomings.md).

## What a snapshot is, and what it is not

A snapshot is the whole VM: guest memory, the rootfs upper layers, the durable
dirs. It is not a health check. A container that died inside the VM before the
snapshot comes back dead in every restore, and Substrate reports the actor
`RUNNING` all the same. The golden snapshot is taken when both launchers'
wakeup probes answer, which they do as soon as they poll, so the golden actor
holds two launchers and no OpenShell process. Read the worker pod's log after
a restore: `Runtime-initiated Sandbox Protocol listener ready`, `Isolation
boundary attached` and `PROC:LAUNCH` are the signs of a healthy actor.

Templates and their golden snapshots are never garbage-collected. The
driver's content-hash naming keeps their number equal to the number of
distinct workloads.
