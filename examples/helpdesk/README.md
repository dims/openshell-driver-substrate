# Helpdesk example

A Python helpdesk agent inside a real OpenShell sandbox, on a Substrate
micro-VM actor. Two agents are restored from one snapshot. One is suspended,
resumed, loses its host, and comes back from its last snapshot. The other is
never touched. After each step, `run.sh` prints what Substrate and OpenShell
logged for it, so the mechanics are on screen and not taken on faith.

OpenShell's isolation holds (Landlock, seccomp-mediated egress, an allow-list
with one host), the agent is reachable from outside the actor, and its memory
survives a suspend and resume. The chat history is a Python list in process
memory; the `turns` count in every reply is the evidence.

## What runs

```
actor  (one micro-VM, one guest network namespace)
├─ sandbox     openshell-sandbox + python3 + agent.py   (the workload's filesystem)
├─ supervisor  openshell-supervisor                     (policy, OPA, proxy, CA)
└─ relay       relay.py                                 (plain container; ingress)
```

The workload is a child of the sandbox, so `python3` lives in the sandbox
image, not the supervisor's. `openshell-sandbox` is a static musl binary, so
any base works; `Dockerfile` puts it on `python:3.12-slim`.

## The ten beats

| # | Beat | What you see | Under the covers |
|---|---|---|---|
| 1 | Template and golden snapshot | One `ActorTemplate` names the three containers. Substrate boots it once, waits 20 s, and snapshots the whole VM. | The sandbox logs `Boundary control listener ready` (every qualification gate passed), `Landlock ruleset built`, `PROC:LAUNCH python3`. The supervisor logs `Isolation boundary attached`. ateom writes `memory-ranges`, `rootfs-upper.tar`, `durable-dir.tar`, `state.json` to the bucket. |
| 2 | Two agents from that snapshot | `create actor` and `resume` bring up alice and bob, each on its own worker: a worker hosts as many actors as fit, and each helpdesk actor asks for the whole worker's CPU. | `Actor restored (overlay rootfs) in ~350 ms`. Nothing boots: both are the same frozen process image, so both start at `turns: 0`. Each actor also gets a Substrate `EgressPolicy` for the model host. |
| 3 | Egress is an allow-list | `https://example.com/` fails name resolution. The model host answers. | The sandbox's network broker denies the `connect(2)` (`syscall=42`) with `EACCES`: the host is not in `data.yaml`. The supervisor's OCSF audit line for the allowed one is `NET:OPEN ALLOWED /usr/local/bin/python3.12 -> 172.18.0.1:11434 [policy:model engine:opa]`. |
| 4 | alice answers | `/chat` returns the model's reply, `turns: 1`. | `OCSF HTTP:POST ALLOWED POST http://.../v1/chat/completions`. The request went through the supervisor's proxy, which is where a provider credential would be attached. The agent never holds one. |
| 5 | Suspend alice | `ACTOR_STATE_SUSPENDED`. Her worker shows `0/1` actors. | `Actor checkpointed` with the snapshot's file list. The VM is gone; only the snapshot remains. |
| 6 | Resume alice | `/status` still says `turns: 1`. The follow-up question is answered from history, `turns: 2`. | `Actor restored ... in ~350 ms`. The history came back inside the memory image. |
| 7 | bob | `turns: 0`. | Nothing happened to him. |
| 8 | alice's host dies | Her worker pod is force-deleted. alice goes `ACTOR_STATE_CRASHED` with worker `<none>`; bob stays `RUNNING`. | The syncer sees the pod go and deletes the Worker; ate-api-server logs `Releasing actor from a worker whose pod is gone`. The Deployment replaces the pod and a new Worker registers within seconds. |
| 9 | Revert and resume | `revert actor` puts alice at `SUSPENDED`, holding her last snapshot. `resume` puts her `RUNNING` on the new worker. `/status` says `turns: 1`. | Restore in ~340 ms. Her last snapshot is beat 5's, so beat 6's turn is gone: a crash loses everything since the last suspend. |
| 10 | Delete alice | Only bob is listed. The template stays. bob answers. | `delete actor --any-state`. Templates are never garbage-collected. |

## Prerequisites

Steps 1 to 3 and 6 of the [root README](../../README.md): a cluster with the
patched Substrate, the micro-VM backend, the two OpenShell images in the
registry, and the worker pool and atespace. Steps 4 and 5, the credentials,
are done by `build.sh`.

An OpenAI-compatible model endpoint the kind node can reach. On the kind
host:

```sh
OLLAMA_HOST=0.0.0.0 ollama serve &
ollama pull qwen2.5:1.5b
```

A kind node reaches the host at `172.18.0.1`, the default `MODEL_HOST`, if
the host lets it: on a firewalled host the input chain needs an allowance for
the model port from the kind bridge. `qwen2.5:0.5b` also works but answers the
memory question in beat 6 badly.

Two free workers. Each helpdesk actor asks for a worker's whole CPU so alice
and bob land on different workers, and the pool in root README step 6 has
two, so no other actor may be running; `run.sh` checks and refuses otherwise.
Suspend or delete the others, or raise the pool's replicas.

On PATH: `docker`, `cargo`, `kubectl`, `kubectl-ate` (ahead of any older
copy), `jq`, `curl`, `grpcurl`, `envsubst`.

## Build

```sh
REGISTRY=localhost:5001 examples/helpdesk/build.sh
```

This mints a credential set with the model endpoint in the workload's
environment, bakes it, builds and pushes the two images, and writes their
digests to `out/helpdesk.env` for `run.sh`:

| Image | Contents |
|---|---|
| `helpdesk-sandbox` | `openshell-sandbox` from the stock image, `python:3.12-slim`, `agent.py`, `relay.py`, the baked bootstrap |
| `openshell-bootstrap-files` | `runtime-descriptor.json`, `auth.json`, `policy.rego`, the rendered `data.yaml`; mounted read-only as an image volume |

Knobs: `MODEL_HOST` (default `172.18.0.1`), `MODEL_PORT` (`11434`),
`MODEL_NAME` (`qwen2.5:1.5b`).

The tokens last one hour, OpenShell's maximum for a session token, so run
`run.sh` within the hour. `build.sh` reuses the credentials in `out/` while
they are under 30 minutes old and the model settings are unchanged, so a
rebuild within that window keeps the same digests and the same template.
Otherwise it mints again, and the new digests give the template a new name and
a new golden snapshot in object storage. Templates are never garbage-collected;
see [Cleanup](#cleanup).

## Run

```sh
examples/helpdesk/run.sh
```

About 55 seconds with a fresh template, about 25 when the template exists.
In a second terminal:

```sh
watch -n2 'kubectl-ate get actors -a ate-openshell-microvm; echo; kubectl-ate get workers'
```

Knobs: `ATESPACE` (default `ate-openshell-microvm`), `BUCKET_NAME`
(`ate-snapshots`). The template is named `helpdesk-<hash>` from the rendered
template, so a rerun with the same images and template reuses it and beat 1
is instant. On success `run.sh` deletes alice and bob and leaves the template. On
failure it keeps both for inspection and prints the delete command.

## Expected output

Trimmed. The indented lines under each beat are what `run.sh` pulled from the
worker pods and ate-api-server for that beat.

```
== 1  Template and golden snapshot  (+3s)
ATESPACE                NAME                SANDBOX CLASS           GOLDEN TAG                             ERROR   AGE
ate-openshell-microvm   helpdesk-c22183d2   SANDBOX_CLASS_MICROVM   7233a52e-4698-44da-bd3a-6100962de719           32s
  golden/sandbox      INFO openshell_sandbox::boundary_server::linux: Boundary control listener ready
  golden/supervisor   INFO openshell_supervisor: Isolation boundary attached
  golden/sandbox      OCSF CONFIG:BUILT [INFO] Landlock ruleset built [rules_applied:14 skipped:0]
  golden/sandbox      OCSF PROC:LAUNCH [INFO] python3(52)
  ateom               Actor checkpointed  base-id config.json durable-dir.tar memory-ranges rootfs-upper.tar state.json

== 2  Two agents restored from that one snapshot  (+37s)
ate-openshell-microvm   alice   .../helpdesk-c22183d2   ACTOR_STATE_RUNNING   .../openshell-microvm-f7c4cdbcb-z28v6   10.244.0.20
ate-openshell-microvm   bob     .../helpdesk-c22183d2   ACTOR_STATE_RUNNING   .../openshell-microvm-f7c4cdbcb-4hdkx   10.244.0.23
  ateom               Actor restored (overlay rootfs) in 341 ms
  ateom               Actor restored (overlay rootfs) in 361 ms

== 3  Egress is an allow-list: the model host, from python, and nothing else  (+40s)
{"url": "https://example.com/", "reached": false, "error": "URLError: <urlopen error [Errno -3] Temporary failure in name resolution>"}
{"url": "http://172.18.0.1:11434/api/tags", "reached": true, "http_status": 200, "bytes": 844}
  alice/sandbox       WARN openshell_sandbox::network_broker: sandbox network notification denied (tid=56, syscall=42): Permission denied (os error 13)
  alice/supervisor    OCSF NET:OPEN [INFO] ALLOWED /usr/local/bin/python3.12(0) -> 172.18.0.1:11434 [policy:model engine:opa]
  alice/supervisor    OCSF HTTP:GET [INFO] ALLOWED GET http://172.18.0.1:11434/api/tags

== 4  alice answers through the supervisor's proxy  (+41s)
{"reply": "Sure, here's a step-by-step triage checklist ...", "turns": 1}
  alice/supervisor    OCSF HTTP:POST [INFO] ALLOWED POST http://172.18.0.1:11434/v1/chat/completions

== 5  Suspend alice: a snapshot is written and her worker is free  (+47s)
4ba0de2b-...   openshell-microvm   WORKER_STATE_ACTIVE   1/1   1/2   1Gi/2Gi   .../openshell-microvm-f7c4cdbcb-4hdkx
16ad9e0b-...   openshell-microvm   WORKER_STATE_ACTIVE   0/1   0/2   0/2Gi     .../openshell-microvm-f7c4cdbcb-z28v6
  ateom               Actor checkpointed  base-id config.json durable-dir.tar memory-ranges rootfs-upper.tar state.json

== 6  Resume alice: her memory comes back with the snapshot  (+48s)
{"turns": 1, "uptime_seconds": 34.1, "model": "qwen2.5:1.5b", "inference_base": "http://172.18.0.1:11434/v1"}
{"reply": "The user is experiencing a timeout with their database.", "turns": 2}
  ateom               Actor restored (overlay rootfs) in 349 ms

== 7  bob was never involved  (+51s)
{"turns": 0, "uptime_seconds": 35.5, "model": "qwen2.5:1.5b", "inference_base": "http://172.18.0.1:11434/v1"}

== 8  alice's host dies: alice crashes, bob does not  (+51s)
pod "openshell-microvm-f7c4cdbcb-z28v6" force deleted from ate-openshell-microvm namespace
ate-openshell-microvm   alice   .../helpdesk-c22183d2   ACTOR_STATE_CRASHED   <none>
ate-openshell-microvm   bob     .../helpdesk-c22183d2   ACTOR_STATE_RUNNING   .../openshell-microvm-f7c4cdbcb-4hdkx   10.244.0.23
  ateapi              Releasing actor from a worker whose pod is gone  alice

== 9  Revert alice to her last snapshot; she resumes on the new worker  (+52s)
ate-openshell-microvm   alice   .../helpdesk-c22183d2   ACTOR_STATE_RUNNING   .../openshell-microvm-f7c4cdbcb-ls48x   10.244.0.24
ate-openshell-microvm   bob     .../helpdesk-c22183d2   ACTOR_STATE_RUNNING   .../openshell-microvm-f7c4cdbcb-4hdkx   10.244.0.23
{"turns": 1, "uptime_seconds": 38.2, "model": "qwen2.5:1.5b", "inference_base": "http://172.18.0.1:11434/v1"}
  ateom               Actor restored (overlay rootfs) in 337 ms

== 10 Delete alice; bob and the template stay  (+54s)
ate-openshell-microvm   bob     .../helpdesk-c22183d2   ACTOR_STATE_RUNNING   .../openshell-microvm-f7c4cdbcb-4hdkx   10.244.0.23
{"reply": "A helpdesk triage agent coordinates and executes tasks ...", "turns": 1}
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
| `build.sh` | Mints the credentials, builds and pushes the two images, writes `out/helpdesk.env`. |
| `run.sh` | The ten beats. Prints the matching Substrate and OpenShell log lines after each. |
| `agent.py` | The workload. `/status`, `/egress?url=`, `/chat`. History in a Python list. Reads `OPENAI_BASE_URL` and `HELPDESK_MODEL` from its environment. |
| `relay.py` | Accepts on the actor's address, connects to the agent over loopback. See below. |
| `Dockerfile` | The sandbox image: `openshell-sandbox` from the stock image, python, the agent, the baked bootstrap. |
| `policy.rego` | OpenShell's shipped `sandbox-policy.rego` at the pinned rev, unmodified. |
| `data.yaml.tmpl` | The policy data: filesystem rules, Landlock as a hard requirement, uid 65532, one network policy for the model host from python. |
| `template.yaml.tmpl` | The `ActorTemplate`: three containers, four volumes, micro-VM class, snapshots on pause and commit. |

## How the pieces fit

**Ingress needs a loopback peer.** The sandbox's network broker refuses an
`accept(2)` from a non-loopback address, so atenet-router alone gets a 502.
The relay is a plain container in the same actor: it accepts on the actor's
address and connects to the agent over loopback, which the broker allows. It
stands in for the gateway's loopback service endpoint, which needs the gateway
path. `run.sh` reaches it with an HTTP `CONNECT` through atenet-router's
tunnel listener, service port 8081:

```sh
kubectl port-forward -n ate-system svc/atenet-router 8001:8081 &
curl -p -x http://127.0.0.1:8001 --proxy-header "ate-target-actor: ${ATESPACE}/alice" http://alice:8081/status
```

**Two egress layers, in order.** OpenShell decides first: the shipped
`policy.rego`'s `egress_authorization` rule allows a connection only if
`data.yaml` names the host, the port, and the binary making it. A policy
without that rule denies everything. Then Substrate's atenet-egress answers
403 until the actor has an `EgressPolicy`; `kubectl-ate` has no verb for it,
so `run.sh` creates one per actor over gRPC:

```sh
grpcurl -cacert ca.crt -authority api.ate-system.svc -H "authorization: Bearer ${TOKEN}" \
  -import-path proto -proto ateapi.proto \
  -d '{"actor":{"atespace":"'"${ATESPACE}"'","name":"alice"},
       "egress_policy":{"metadata":{"atespace":"'"${ATESPACE}"'","name":"default"},
                        "rules":[{"cidrs":{"cidrs":["172.18.0.1/32"]}}]}}' \
  127.0.0.1:8443 ateapi.Control/CreateActorEgressPolicy
```

**The model endpoint rides the bootstrap.** `build.sh` passes
`OPENAI_BASE_URL` and `HELPDESK_MODEL` to `bootstrap-gen` as
`BOOTSTRAP_CHILD_ENV`; the sandbox puts them in the workload's environment.
Nothing per sandbox is in there, so one snapshot serves every actor.

**The supervisor's three requirements.**

| Missing | Error in the golden warm-up log |
|---|---|
| `--policy-rules` / `--policy-data` | `Sandbox policy required. Provide one of: ...` |
| `OPENSHELL_ADMITTED_ISOLATION_BACKEND=openshell-sandbox` | `runtime descriptor supplied without an admitted isolation backend` |
| a writable `/run/openshell-supervisor-ca` | `create supervisor CA directory ...: Permission denied` |

The CA directory is where the supervisor installs the TLS-interception CA the
workload trusts. A durable-dir volume works because those are `0777`.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Beat 1 never gets a golden tag; the worker log says `boundary unavailable ... timed out while waiting for remote boundary boot` about 90 s in | The tokens are older than an hour. Nothing says "expired". | `build.sh` again, then `run.sh`. |
| `/chat` returns 502 `RemoteDisconnected` | No Substrate `EgressPolicy`: atenet-egress answered 403. | `run.sh` creates one per actor; by hand, the `grpcurl` above. |
| `/chat` returns 503 `no inference endpoint injected` | The image was built without the model environment. | `build.sh` with `MODEL_*` set. |
| `/egress` to the model host says `reached: false` | The host is not in `data.yaml`, or the binary path is not `/usr/local/bin/python3.12`. | `data.yaml.tmpl`, then `build.sh`. |
| `create actor-template` fails with `FailedPrecondition ... persistence` | The atespace does not exist. | Root README step 6. |
| `delete actor-template` says `Aborted: another operation is in progress` | Its golden warm-up is running. | Retry after it tags. |
| Beat 8: alice stays `RUNNING` on a `DRAINING` worker | The pod was deleted without `--force`. The pool's grace period is an hour and ateom waits for the guest workloads. | `kubectl delete pod --grace-period=0 --force`, as `run.sh` does. |
| `kubectl-ate logs actor` prints nothing | Restored actors log through the worker pod. | `kubectl logs -n ${ATESPACE} <worker-pod>`, which is what `run.sh` filters. |

## Cleanup

`run.sh` deletes alice and bob when it succeeds. Templates stay, and each
one holds a golden snapshot in the bucket:

```sh
kubectl-ate get actor-template -a ate-openshell-microvm
kubectl-ate delete actor-template -a ate-openshell-microvm helpdesk-<hash>
```

The pool and atespace are the root README's.
