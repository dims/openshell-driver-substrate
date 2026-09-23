# Helpdesk example

A Python agent running inside a real OpenShell sandbox on a Substrate micro-VM
actor. It shows the three things the integration is for: OpenShell's isolation
holds (Landlock, seccomp-mediated egress, default deny), the agent is reachable
from outside the actor, and the actor's memory survives a suspend and resume —
the agent's uptime counter and chat history live in process memory and are
still there afterwards.

It needs steps 1 to 6 of the [root README](../../README.md): a cluster with
the patched Substrate, the micro-VM backend, the OpenShell images, minted
credentials, and a worker pool.

## What runs

```
actor
├─ sandbox     openshell-sandbox + python3 + agent.py   (the workload's filesystem)
├─ supervisor  openshell-supervisor                     (policy, OPA, proxy, CA)
└─ relay       relay.py                                 (plain container; ingress)
```

The workload is a child of the sandbox, so `python3` has to be in the sandbox
image, not the supervisor's. `openshell-sandbox` is a static musl binary, so
any base works; `Dockerfile` uses `python:3.12-slim`.

## Build

From this directory, with `out/` the credential directory of root README
step 4:

```sh
cp <openshell>/deploy/docker/.build/prebuilt-binaries/amd64/openshell-sandbox .
cp ../../out/bootstrap.tar .        # left there by package-credentials.sh
docker build -t <registry>/helpdesk-sandbox:dev .

mkdir -p files/supervisor
cp ../../out/runtime-descriptor.json ../../out/auth.json policy.rego data.yaml files/supervisor/
printf 'FROM scratch\nCOPY supervisor/ /supervisor/\n' > files/Dockerfile
docker build -t <registry>/openshell-bootstrap-files:dev files

docker push <registry>/helpdesk-sandbox:dev
docker push <registry>/openshell-bootstrap-files:dev
export SANDBOX_IMAGE=$(docker inspect --format '{{index .RepoDigests 0}}' <registry>/helpdesk-sandbox:dev)
export BOOTSTRAP_FILES_IMAGE=$(docker inspect --format '{{index .RepoDigests 0}}' <registry>/openshell-bootstrap-files:dev)
```

Templates reference images by digest; a bare tag fails atelet's pull cache.
`SUPERVISOR_IMAGE` comes from root README step 3.

## Run

```sh
export ATESPACE=... BUCKET_NAME=...        # as in root README step 6
../../harness/scripts/render.sh template.yaml.tmpl | kubectl-ate create actor-template -f -
kubectl-ate create actor hd-1 --atespace "${ATESPACE}" --template helpdesk
kubectl-ate resume actor -a "${ATESPACE}" hd-1
```

Later:

```sh
kubectl-ate suspend actor -a "${ATESPACE}" hd-1    # -> ACTOR_STATE_SUSPENDED
kubectl-ate resume  actor -a "${ATESPACE}" hd-1    # -> ACTOR_STATE_RUNNING
```

In the worker pod log, `Isolation boundary attached`, `Landlock ruleset built`
and `PROC:LAUNCH python3` say the supervisor reached the sandbox and started
the agent.

## Reach the agent

`network_broker` refuses an `accept(2)` from a non-loopback peer, so the agent
is not reachable from outside the sandbox on its own. The `relay` container
accepts on the actor's address and connects to the agent over loopback. It
stands in for the gateway's loopback service endpoint, which needs the gateway
path.

Substrate reaches a non-default actor port with an HTTP `CONNECT` tunnel
through atenet-router's tunnel listener (service port 8081):

```sh
kubectl port-forward -n ate-system svc/atenet-router 8001:8081 &
curl -p -x http://127.0.0.1:8001 \
  --proxy-header "ate-target-actor: ${ATESPACE}/hd-1" \
  http://hd-1:8081/status
```

`/status` returns `{"turns": N, "uptime_seconds": S, ...}`. Suspend and
resume the actor and call it again: `uptime_seconds` keeps counting from where
it was.

## Egress

`policy.rego` is OpenShell's default sandbox policy at the pinned rev,
unmodified (`crates/openshell-supervisor-network/data/sandbox-policy.rego`).
`data.yaml` sets `network_policies: {}`, so every outbound connection is
denied; `/egress?url=...` reports `"reached": false` and the sandbox logs a
`network_broker` denial. To allow a host, add it under `network_policies`
with the binary that may reach it; the shape is in `data.yaml`.

## A model for `/chat`

The agent reads `OPENAI_BASE_URL` and `HELPDESK_MODEL` from its environment.
The bootstrap carries the workload's environment, so set them when minting
(root README step 4) and allow the model host in `data.yaml`:

```sh
BOOTSTRAP_CHILD_ENV="OPENAI_BASE_URL=http://172.18.0.1:11434/v1,HELPDESK_MODEL=qwen2.5:0.5b" \
  cargo run --manifest-path harness/bootstrap-gen/Cargo.toml -- out/
```

On a kind cluster `172.18.0.1` is the host; an `ollama serve` bound to
`0.0.0.0:11434` there answers. Without a model `/chat` returns 503.

Substrate has its own egress control and denies every destination until the
actor has an `EgressPolicy` (atenet-egress answers 403; the agent sees
`RemoteDisconnected`). `kubectl-ate` has no verb for it, so use the API:

```sh
TOKEN=$(cat creds/token)     # root README step 8
grpcurl -cacert creds/ctb.crt -authority api.ate-system.svc \
  -H "authorization: Bearer ${TOKEN}" \
  -import-path <substrate>/pkg/proto/ateapipb -proto ateapi.proto \
  -d '{"actor":{"atespace":"'"${ATESPACE}"'","name":"hd-1"},
       "egress_policy":{"metadata":{"atespace":"'"${ATESPACE}"'","name":"default"},
                        "rules":[{"cidrs":{"cidrs":["172.18.0.1/32"]}}]}}' \
  127.0.0.1:8443 ateapi.Control/CreateActorEgressPolicy
```

Then `/chat` returns the model's reply and a `turns` count that keeps
growing across a suspend and resume: the history is process memory and comes
back with the snapshot.

## The supervisor's three requirements

| Missing | Error in the golden warm-up log |
|---|---|
| `--policy-rules` / `--policy-data` | `Sandbox policy required. Provide one of: ...` |
| `OPENSHELL_ADMITTED_ISOLATION_BACKEND=openshell-sandbox` | `runtime descriptor supplied without an admitted isolation backend` |
| a writable `/run/openshell-supervisor-ca` | `create supervisor CA directory ...: Permission denied` |

The CA directory is where the supervisor installs the TLS-interception CA the
workload trusts. A durable-dir volume works because those are `0777`.
