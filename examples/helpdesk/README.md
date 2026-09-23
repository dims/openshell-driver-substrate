# Helpdesk example

A Python workload running inside a real OpenShell sandbox on a Substrate
micro-VM actor. Chat history lives in a Python list, so a suspend/resume shows
the actor kept its memory.

## What runs

```
actor
├─ sandbox     openshell-sandbox + python3 + agent.py   (the workload's filesystem)
└─ supervisor  openshell-supervisor                     (policy, OPA, proxy, CA)
```

The workload is a child of the sandbox, so `python3` has to be in the **sandbox**
image, not the supervisor's. `openshell-sandbox` is a static musl binary, so any
base works; `Dockerfile` uses `python:3.12-slim`.

Verified on micro-VM:

```
OCSF CONFIG:BUILT  Landlock ruleset built [rules_applied:14 skipped:0]
OCSF PROC:LAUNCH   python3(52)
openshell_supervisor: Isolation boundary agent started
OCSF NET:LISTEN    127.0.0.1:3128
network_broker: notification denied (tid=52, syscall=42): Permission denied
```

Landlock ABI v7 as a hard requirement, the workload launched, the supervisor's
proxy listening, and the workload's `connect(2)` mediated and denied because
`network_policies` is empty.

## Build

```sh
# sandbox image: openshell-sandbox + python3 + agent.py + the baked bootstrap
cp <openshell>/deploy/docker/.build/prebuilt-binaries/amd64/openshell-sandbox .
# bootstrap.tar as in the root README step 6, then
docker build -t <registry>/helpdesk-sandbox:dev .

# supervisor files image: credentials AND policy
mkdir -p files/supervisor
cp out/runtime-descriptor.json out/auth.json policy.rego data.yaml files/supervisor/
printf 'FROM scratch\nCOPY supervisor/ /supervisor/\n' > files/Dockerfile
docker build -t <registry>/openshell-bootstrap-files:dev files
```

## Run

```sh
export ATESPACE=... BUCKET_NAME=... SANDBOX_IMAGE=... SUPERVISOR_IMAGE=... BOOTSTRAP_FILES_IMAGE=...
../../harness/scripts/render.sh template.yaml.tmpl | kubectl-ate create actor-template -f -
kubectl-ate create actor hd-1 --atespace "${ATESPACE}" --template helpdesk
kubectl-ate resume actor -a "${ATESPACE}" hd-1
```

## Three things the supervisor needs, and fails clearly without

Each of these stops the workload from ever starting. The message is in the
golden-snapshot warmup logs, not the running actor's.

| Missing | Error |
|---|---|
| `--policy-rules` / `--policy-data` | `Sandbox policy required. Provide one of: ...` |
| `OPENSHELL_ADMITTED_ISOLATION_BACKEND=openshell-sandbox` | `runtime descriptor supplied without an admitted isolation backend` |
| a writable `/run/openshell-supervisor-ca` | `create supervisor CA directory ...: Permission denied` |

The CA directory is where the supervisor installs the TLS-interception CA the
workload trusts. A durable-dir volume works because those are `0777`.

## Reaching the workload

`network_broker` mediates `accept(2)` and refuses a non-loopback peer, so the
agent's port is **not** reachable through atenet-router — a request there
returns 502 even though the agent is listening. Ingress is meant to go through
the gateway's loopback service endpoints
(`enable_loopback_service_http`, `CreateSandboxServiceEndpoint`). That path is
not wired up here yet.

## Egress

`data.yaml` sets `network_policies: {}`, so every outbound connection is denied
and the denial is visible as a `network_broker` notification. Allowing a host
means adding it there. The agent reads its model endpoint from the environment
the supervisor injects and never holds a credential; the supervisor's proxy
attaches one.

`GET /egress?url=...` reports whether a request left and how it failed, which
is the quickest way to see a policy change take effect.
