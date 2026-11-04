# Helpdesk example

A Python helpdesk agent inside a real OpenShell sandbox, on a Substrate
micro-VM actor. Two agents are restored from one snapshot. One is suspended,
resumed, loses its host, and comes back from its last snapshot. The other is
never touched. After each step, `run.sh` prints what Substrate and OpenShell
logged for it, so the mechanics are on screen and not taken on faith.

OpenShell's isolation holds (Landlock, seccomp-mediated egress, a policy DNS
that answers for one host), the agent is reachable from outside the actor, and
its memory survives a suspend and resume. The chat history is a Python list in
process memory; the `turns` count in every reply is the evidence.

## What runs

```
actor  (one micro-VM, one guest network namespace, address 169.254.17.2)
├─ sandbox     openshell-sandbox + python3 + agent.py   (the workload's filesystem)
├─ supervisor  openshell-supervisor                     (policy, OPA, proxy, CA; listens on :17777)
└─ relay       relay.py                                 (plain container; ingress)
```

The workload is a child of the sandbox, so `python3` lives in the sandbox
image, not the supervisor's. `openshell-sandbox` is a static musl binary, so
any base works; `Dockerfile` puts it on `python:3.12-slim`. The supervisor
listens on the actor's address and the sandbox dials it: OpenShell refuses a
boundary listener on loopback and a supervisor peer on the sandbox's own
address, and the two containers share one network namespace.

## The ten beats

| # | Beat | What you see | Under the covers |
|---|---|---|---|
| 1 | Template and golden snapshot | One `ActorTemplate` names the three containers. Substrate boots it once, waits 20 s, and snapshots the whole VM. | The supervisor logs `Runtime-initiated Sandbox Protocol listener ready` and `Isolation boundary attached`; the sandbox logs `Landlock ruleset built` and `PROC:LAUNCH python3`. ateworker writes the memory ranges, the rootfs uppers and the durable dirs to the bucket. |
| 2 | Two agents from that snapshot | `create actor` and `resume` bring up alice and bob, each on its own worker: a worker hosts as many actors as fit, and each helpdesk actor asks for the whole worker's CPU. | `Actor restored (overlay rootfs) in ~330 ms`. Nothing boots: both are the same frozen process image, so both start at `turns: 0`. Each actor also gets a Substrate `EgressPolicy` for the model host. |
| 3 | Egress is an allow-list | `https://example.com/` is refused. The model host answers. | The supervisor's policy DNS refuses the name (`NET:REFUSE ... policy_dns_ineligible`) and denies the connect; for the model host it maps the name (`Policy DNS mapped ...`) and logs `NET:OPEN ALLOWED /usr/local/bin/python3.12 -> ollama...:11434 [policy:model engine:opa]`. |
| 4 | alice answers | `/chat` returns the model's reply, `turns: 1`. | `OCSF HTTP:POST ALLOWED POST http://.../v1/chat/completions`. The request went through the supervisor's proxy, which is where a provider credential would be attached. The agent never holds one. |
| 5 | Suspend alice | `ACTOR_STATE_SUSPENDED`. Her worker shows `0/1000` actors. | `Actor checkpointed` with the snapshot's file list; `--mode sync` returns once the snapshot is in the bucket. The VM is gone; only the snapshot remains. |
| 6 | Resume alice | `/status` still says `turns: 1`. The follow-up question is answered from history, `turns: 2`. | `Actor restored ... in ~330 ms`. The history came back inside the memory image. |
| 7 | bob | `turns: 0`. | Nothing happened to him. |
| 8 | alice's host dies | Her worker pod is force-deleted. alice goes `ACTOR_STATE_CRASHED` with worker `<none>`; bob stays `RUNNING`. | The syncer sees the pod go and deletes the Worker; ate-api-server logs `Releasing actor from a worker whose pod is gone`. The Deployment replaces the pod and a new Worker registers within seconds. |
| 9 | Revert and resume | `revert actor` puts alice at `SUSPENDED`, holding her last snapshot. `resume` puts her `RUNNING` on the new worker. `/status` says `turns: 1`. | Restore in ~340 ms. Her last snapshot is beat 5's, so beat 6's turn is gone: a crash loses everything since the last suspend. |
| 10 | Delete alice | Only bob is listed. The template stays. bob answers. | Templates are never garbage-collected. |

## Prerequisites

A cluster from `harness/scripts/kind-up.sh` (root README step 1): Substrate
from upstream `main` with the micro-VM class, the pool of two workers, the
atespace, and Ollama serving the model in that atespace as
`ollama.<atespace>.svc.cluster.local`. Substrate's egress rules name hosts,
not addresses, which is why the model runs in the cluster.

Two free workers. Each helpdesk actor asks for a worker's whole CPU so alice
and bob land on different workers, and the pool has two, so no other actor
may be running; `run.sh` checks and refuses otherwise.

On PATH: `docker`, `cargo`, `go`, `kubectl`, `kubectl-ate` (the one
`kind-up.sh` built, ahead of any older copy), `jq`, `curl`, `envsubst`,
`openssl`, `sha256sum`.

## Build

```sh
REGISTRY=localhost:5001 examples/helpdesk/build.sh
```

This mirrors the release's `sandbox` and `supervisor` images from
`ghcr.io/nvidia/openshell` into the registry, mints a credential set with the
model endpoint in the workload's environment, bakes it into the sandbox image,
fetches OpenShell's stock policy at the pinned rev, builds and pushes the two
images, and writes their digests to `out/helpdesk.env` for `run.sh`:

| Image | Contents |
|---|---|
| `helpdesk-sandbox` | `openshell-sandbox` from the release image, `python:3.12-slim`, `agent.py`, `relay.py`, `sandbox-entry.sh`, the baked bootstrap |
| `openshell-bootstrap-files` | `runtime-descriptor.json`, `auth.json`, OpenShell's stock `sandbox-policy.rego` as `policy.rego`, the rendered `data.yaml`; mounted read-only as an image volume |

Knobs: `ATESPACE` (default `ate-openshell-microvm`), `MODEL_HOST` (default
`ollama.<atespace>.svc.cluster.local`), `MODEL_PORT` (`11434`), `MODEL_NAME`
(`qwen2.5:1.5b`), `OPENSHELL_VERSION` (`0.1.7`).

The tokens do not expire. `build.sh` reuses the credentials in `out/` while
the model settings are unchanged, so a rebuild keeps the same digests and the
same template. `rm -rf out` mints again, and the new digests give the template
a new name and a new golden snapshot in object storage. Templates are never
garbage-collected; see [Cleanup](#cleanup).

## Run

```sh
examples/helpdesk/run.sh
examples/helpdesk/soak.sh 10    # ten runs in a row; stops at the first failure
```

About a minute with a fresh template, 20 to 30 seconds when it exists.
In a second terminal:

```sh
watch -n2 'kubectl-ate get actors -a ate-openshell-microvm; echo; kubectl-ate get workers'
```

Knobs: `ATESPACE` (default `ate-openshell-microvm`). The template is named
`helpdesk-<hash>` from the rendered template, so a rerun with the same images
and template reuses it and beat 1 is instant. On success `run.sh` deletes alice
and bob and leaves the template. On failure it keeps both for inspection and
prints the delete command.

## Expected output

Trimmed. The indented lines under each beat are what `run.sh` pulled from the
worker pods and ate-api-server for that beat.

```
== 1  Template and golden snapshot  (+2s)
ATESPACE                NAME                GOLDEN TAG                             ERROR   AGE
ate-openshell-microvm   helpdesk-c734f33f   1c6a0b2e-...                                   26s
  golden/supervisor   INFO openshell_sandbox_backend::runtime_initiated: Runtime-initiated Sandbox Protocol listener ready
  golden/supervisor   INFO openshell_supervisor: Isolation boundary attached
  golden/sandbox      OCSF CONFIG:BUILT [INFO] Landlock ruleset built [rules_applied:14 skipped:0]
  golden/sandbox      OCSF PROC:LAUNCH [INFO] python3(56)
  ateworker           Actor checkpointed  base-id config.json durable-dir-socket-dir.tar durable-dir-supervisor-ca.tar memory-ranges rootfs-upper-relay.tar ...

== 2  Two agents restored from that one snapshot  (+38s)
ate-openshell-microvm   alice   .../helpdesk-c734f33f   ACTOR_STATE_RUNNING   .../openshell-microvm-c764cc79-brq82   10.244.0.23
ate-openshell-microvm   bob     .../helpdesk-c734f33f   ACTOR_STATE_RUNNING   .../openshell-microvm-c764cc79-q7h4m   10.244.0.24
  ateworker           Actor restored (overlay rootfs) in 350 ms
  ateworker           Actor restored (overlay rootfs) in 323 ms

== 3  Egress is an allow-list: the model host, from python, and nothing else  (+42s)
{"url": "https://example.com/", "reached": false, "error": "URLError: <urlopen error [Errno 13] Permission denied>"}
{"url": "http://ollama.ate-openshell-microvm.svc.cluster.local:11434/api/tags", "reached": true, "http_status": 200, "bytes": 438}
  alice/supervisor    OCSF NET:REFUSE [MED] DENIED example.com [reason:policy_dns_ineligible]
  alice/supervisor    OCSF NET:OPEN [MED] DENIED /usr/local/bin/python3.12(0) -> example.com:443 [reason:transparent_tcp_policy_denied]
  alice/supervisor    OCSF CONFIG:PUBLISHED [INFO] Policy DNS mapped ollama.ate-openshell-microvm.svc.cluster.local resolved=10.96.237.191 ...
  alice/supervisor    OCSF NET:OPEN [INFO] ALLOWED /usr/local/bin/python3.12(0) -> ollama.ate-openshell-microvm.svc.cluster.local:11434 [policy:model engine:opa]
  alice/supervisor    OCSF HTTP:GET [INFO] ALLOWED GET http://ollama.ate-openshell-microvm.svc.cluster.local:11434/api/tags

== 4  alice answers through the supervisor's proxy  (+43s)
{"reply": "Certainly! Here's a three-step triage checklist ...", "turns": 1}
  alice/supervisor    OCSF HTTP:POST [INFO] ALLOWED POST http://ollama.ate-openshell-microvm.svc.cluster.local:11434/v1/chat/completions

== 5  Suspend alice: a snapshot is written and her worker is free  (+47s)
  ateworker           Actor checkpointed  base-id config.json durable-dir-socket-dir.tar ...

== 6  Resume alice: her memory comes back with the snapshot  (+49s)
{"turns": 1, "uptime_seconds": 32.1, "model": "qwen2.5:1.5b", "inference_base": "http://ollama.ate-openshell-microvm.svc.cluster.local:11434/v1"}
{"reply": "The user reported that their database is timing out.", "turns": 2}
  ateworker           Actor restored (overlay rootfs) in 312 ms

== 7  bob was never involved  (+51s)
{"turns": 0, "uptime_seconds": 33.6, ...}

== 8  alice's host dies: alice crashes, bob does not  (+51s)
ate-openshell-microvm   alice   .../helpdesk-c734f33f   ACTOR_STATE_CRASHED   <none>
ate-openshell-microvm   bob     .../helpdesk-c734f33f   ACTOR_STATE_RUNNING   .../openshell-microvm-c764cc79-q7h4m   10.244.0.24
  ateapi              Releasing actor from a worker whose pod is gone  alice

== 9  Revert alice to her last snapshot; she resumes on the new worker  (+52s)
ate-openshell-microvm   alice   .../helpdesk-c734f33f   ACTOR_STATE_RUNNING   .../openshell-microvm-c764cc79-pcn2s   10.244.0.26
{"turns": 1, "uptime_seconds": 36.2, ...}
  ateworker           Actor restored (overlay rootfs) in 360 ms

== 10 Delete alice; bob and the template stay  (+54s)
ate-openshell-microvm   bob    .../helpdesk-c734f33f   ACTOR_STATE_RUNNING   .../openshell-microvm-c764cc79-q7h4m   10.244.0.24
{"reply": "A helpdesk triage agent prioritizes and manages customer issues ...", "turns": 1}
```

Beat 9 says `turns: 1` where beat 6 said `turns: 2`. That is the point of
the beat, not a bug: a restore is the last snapshot, and alice's last snapshot
is beat 5's suspend. The turn she took after it died with her host. It is the
one place the difference between a snapshot and a live process is visible.

`uptime_seconds` counts from the golden actor's boot, because `booted` was
set before the snapshot and every actor inherits it. `turns` is the evidence
of memory, not uptime.

## What's in this folder

| File | Purpose |
|---|---|
| `build.sh` | Mirrors the release images, mints and bakes the credentials, fetches the policy, builds and pushes the two images, writes `out/helpdesk.env`. |
| `run.sh` | The ten beats. Prints the matching Substrate and OpenShell log lines after each. |
| `soak.sh` | `run.sh` N times in a row, one log per run, stops at the first failure. |
| `agent.py` | The workload. `/status`, `/egress?url=`, `/chat`. History in a Python list. Reads `OPENAI_BASE_URL` and `HELPDESK_MODEL` from its environment. |
| `relay.py` | Accepts on the actor's address, connects to the agent over loopback. See below. |
| `sandbox-entry.sh` | The sandbox container's first process, already uid 65532 with no capability: points the resolver at the sandbox's DNS relay, waits for the supervisor's listener, then starts `openshell-sandbox` under `setpriv --no-new-privs`. |
| `Dockerfile` | The sandbox image: `openshell-sandbox` from the release image, python, the agent, the entry script, the baked bootstrap. |
| `data.yaml.tmpl` | The policy data: filesystem rules, Landlock as a hard requirement, uid 65532, one network policy for the model host from python. |
| `egress-policy.yaml.tmpl` | Substrate's egress gate for one actor: the model host by name, port `MODEL_PORT`. |
| `template.yaml.tmpl` | The `ActorTemplate`: three containers, three volumes, memory snapshots, the micro-VM pool by label. |
| `ollama.yaml.tmpl` | The model endpoint in the atespace: a Deployment that pulls the model and a Service. `kind-up.sh` applies it. |

## How the pieces fit

**Ingress needs a loopback peer.** The sandbox's network broker refuses an
`accept(2)` from a non-loopback address, so atenet-router alone gets a 502.
The relay is a plain container in the same actor: it accepts on the actor's
address and connects to the agent over loopback, which the broker allows. It
stands in for the gateway's loopback service endpoint, which needs the gateway
path. atenet-router takes mTLS only; `run.sh` reaches the relay with
`kubectl-ate proxy actor`, which obtains the client certificate and forwards a
local port:

```sh
kubectl-ate proxy actor -a "${ATESPACE}" alice 18081:8081 &
curl http://127.0.0.1:18081/status
```

**Two egress layers, in order.** OpenShell decides first: the shipped
`policy.rego`'s `egress_authorization` rule allows a connection only if
`data.yaml` names the host, the port, and the binary making it, and its policy
DNS answers only for those hosts. Then Substrate's atenet-egress answers 403
until the actor has an `EgressPolicy`. `run.sh` creates one per actor from
`egress-policy.yaml.tmpl`:

```sh
kubectl-ate create egress-policy -a "${ATESPACE}" alice -f egress-policy.yaml
```

**DNS.** The sandbox answers the workload's DNS at `127.0.0.53` and the
broker refuses any other nameserver. Substrate writes the actor gateway into
`/etc/resolv.conf` in the rootfs base layer and keeps the actor's own edits,
so `sandbox-entry.sh` replaces the file with `nameserver 127.0.0.53` on the
golden boot and every restore keeps it. The image's `/etc` belongs to the
sandbox user so that replacement needs no root; the workload's Landlock
policy keeps `/etc` read-only for the workload itself.

**The model endpoint rides the bootstrap.** `build.sh` passes
`OPENAI_BASE_URL` and `HELPDESK_MODEL` to `bootstrap-gen` as
`BOOTSTRAP_CHILD_ENV`; the sandbox puts them in the workload's environment.
Nothing per sandbox is in there, so one snapshot serves every actor.

**Three things the pair requires.**

| Missing | Error in the golden warm-up log |
|---|---|
| the supervisor's `--policy-rules` / `--policy-data` | `Sandbox policy required. Provide one of: ...` |
| the supervisor's `OPENSHELL_ADMITTED_ISOLATION_BACKEND=openshell-sandbox` | `runtime descriptor supplied without an admitted isolation backend` |
| a writable `/run/openshell-supervisor-ca` in the sandbox container | `create supervisor CA directory ...: Permission denied` |

The sandbox creates that directory, and the supervisor installs the
TLS-interception CA the workload trusts into it. A durable-dir volume works
because those are world-writable.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Beat 1 never gets a golden tag and the template's `errorMessage` says `GoldenActorCrashed ... MANIFEST_UNKNOWN` | A template names a digest the registry does not have. | `build.sh` asks the registry for every digest; do not take digests from `docker inspect`. |
| `/chat` returns 502 `RemoteDisconnected` | No Substrate `EgressPolicy`: atenet-egress answered 403. | `run.sh` creates one per actor; by hand, the `kubectl-ate create egress-policy` above. |
| Beat 3 says `Temporary failure in name resolution` for every host | The workload's resolver is not `127.0.0.53`. | `sandbox-entry.sh` writes it; check the sandbox container's `/etc/resolv.conf`. |
| `run.sh` stops at beat 1 with `never logged 'Isolation boundary attached' and 'PROC:LAUNCH'`, or at beat 2 with `never answered /status` | The sandbox did not clear its qualification gates or never started; the container is dead behind a live actor. | `kubectl logs -n "${ATESPACE}" <worker-pod> \| grep -i openshell` and the gates table in the root README. |
| `sandbox-entry: no supervisor listener on 169.254.17.2:17777` | The supervisor did not start or did not bind the actor address. | The supervisor container's log lines in the worker pod log. |
| `run.sh` stops before beat 1 with `did not answer a chat ... within 120 s` | The Ollama pod is not ready, or the model is not pulled. | `kubectl -n "${ATESPACE}" get pods`; `kind-up.sh` waits for the rollout. |
| `/egress` to the model host says `reached: false` | The host is not in `data.yaml`, or the binary path is not `/usr/local/bin/python3.12`. | `data.yaml.tmpl`, then `build.sh`. |
| `create actor-template` fails with `FailedPrecondition ... persistence` | The atespace does not exist. | `kind-up.sh` creates it. |
| `delete actor-template` says `Aborted: another operation is in progress` | Its golden warm-up is running. | Retry after it tags. |
| Beat 8: alice stays `RUNNING` on a `DRAINING` worker | The pod was deleted without `--force`. The pool's grace period is an hour and ateworker waits for the guest workloads. | `kubectl delete pod --grace-period=0 --force`, as `run.sh` does. |

## Cleanup

`run.sh` deletes alice and bob when it succeeds. Templates stay, and each
one holds a golden snapshot in the bucket:

```sh
kubectl-ate get actor-template -a ate-openshell-microvm
kubectl-ate delete actor-template -a ate-openshell-microvm helpdesk-<hash>
```

The pool, the atespace and the model endpoint are `kind-up.sh`'s.
