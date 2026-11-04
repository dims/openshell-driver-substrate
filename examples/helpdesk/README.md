# Helpdesk example

A Python helpdesk agent inside a real OpenShell sandbox, on a Substrate
micro-VM actor, driven by a stock `openshell-gateway` through the driver.
OpenShell's isolation holds (Landlock, seccomp-mediated egress, a policy DNS
that answers for one host), and the agent's memory survives a Substrate
suspend and resume: the chat history is a Python list in process memory, and
the `turns` count in every reply is the evidence.

`harness/scripts/gateway-test.sh` runs this workload through every lifecycle
step; the root README has the walkthrough.

## The agent

`agent.py` serves `/status`, `/chat` and `/egress?url=` on `:8080`. With
`OPENAI_BASE_URL` and `HELPDESK_MODEL` in its environment it forwards each
chat to that OpenAI-compatible endpoint; without them it answers on its own,
deterministically, so the lifecycle checks need no model. Run with an
argument it is a client for the server on loopback, which is how
`openshell sandbox exec` reaches it:

```sh
openshell sandbox exec alice -- python3 /opt/helpdesk/agent.py status
openshell sandbox exec alice -- python3 /opt/helpdesk/agent.py chat "my database is timing out"
openshell sandbox exec alice -- python3 /opt/helpdesk/agent.py egress https://example.com/
```

```
{"turns": 0, "uptime_seconds": 41.2, "model": "", "inference_base": ""}
{"reply": "turn 1: you said 'my database is timing out'", "turns": 1}
{"url": "https://example.com/", "reached": false, "error": "URLError: <urlopen error [Errno 13] Permission denied>"}
```

`uptime_seconds` counts from the golden actor's boot: `booted` was set before
the snapshot and every actor inherits it. `turns` is the evidence of memory.

## Build

```sh
REGISTRY=localhost:5001 examples/helpdesk/build.sh
```

Builds the runtime image (`runtime/build.sh`), then `helpdesk-sandbox`, the
runtime image plus `agent.py`, pushes it and writes the digests to
`out/helpdesk.env` as `SANDBOX_IMAGE` and `RUNTIME_IMAGE`. Knobs: `REGISTRY`
(required), `OPENSHELL_VERSION` (`0.1.7`).

## The model, optional

```sh
examples/helpdesk/model-up.sh                 # Ollama with qwen2.5:1.5b in the atespace
WITH_MODEL=1 harness/scripts/gateway-test.sh  # the chats go to it
```

The model runs in the cluster because a Substrate egress rule names hosts,
never addresses; the actors reach it as
`ollama.<atespace>.svc.cluster.local`. The sandbox policy
(`data.yaml.tmpl`) allows that one host, from python only, and the driver's
egress rules (`harness/gateway/egress-policy.yaml.tmpl`) allow it on the
Substrate side. A create with the model passes
`--env OPENAI_BASE_URL=http://<host>:<port>/v1 --env HELPDESK_MODEL=<name>`.
Knobs: `ATESPACE`, `MODEL_NAME` (`qwen2.5:1.5b`), `MODEL_PORT` (`11434`).

## What's in this folder

| File | Purpose |
|---|---|
| `agent.py` | The workload, and its own client. History in a Python list. Reads `OPENAI_BASE_URL` and `HELPDESK_MODEL` from its environment. |
| `Dockerfile` | The image: the runtime image plus the agent. |
| `build.sh` | Builds the runtime image and this one, writes `out/helpdesk.env`. |
| `data.yaml.tmpl` | The `--policy` for `openshell sandbox create`: filesystem rules, Landlock as a hard requirement, uid 65532, one network policy for the model host from python. |
| `ollama.yaml.tmpl`, `model-up.sh` | The model endpoint in the atespace: a Deployment that pulls the model and a Service. |

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| The first create never reaches Ready and the template's `errorMessage` says `GoldenActorCrashed ... MANIFEST_UNKNOWN` | A template names a digest the registry does not have. | `build.sh` asks the registry for every digest; do not take digests from `docker inspect`. |
| `chat` returns 502 `RemoteDisconnected` with the model | No Substrate egress rule for the model host: atenet-egress answered 403. | `MODEL_HOST` and `MODEL_PORT` on `gateway-up.sh` must match `model-up.sh`. |
| `egress` to the model host says `reached: false` | The host is not in the policy, or the binary path is not `/usr/local/bin/python3.12`. | `data.yaml.tmpl`, then a new create. |
| `egress` says `Temporary failure in name resolution` for every host | The workload's resolver is not `127.0.0.53`. | The launcher writes it; check the sandbox container's `/etc/resolv.conf`. |
| The workload exits with `Permission denied` | No `--policy`: the gateway's default Landlock rules do not cover `/opt/helpdesk`. | Pass the rendered `data.yaml.tmpl`. |
| `launcher[sandbox] no supervisor listener on 169.254.17.2:17777` | The supervisor did not start or did not bind the actor address. | The supervisor container's log lines in the worker pod log. |
| `model-up.sh` waits for the rollout | The Ollama pod is still pulling the model. | `kubectl -n "${ATESPACE}" get pods`; the probe passes once the model is listed. |
| `delete actor-template` says `Aborted: another operation is in progress` | Its golden warm-up is running. | Retry after it tags. |
