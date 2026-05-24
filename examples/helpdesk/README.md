# OpenShell helpdesk on Agent Substrate

A six-beat demo that runs an LLM-backed helpdesk agent inside an OpenShell sandbox, on top of an [agent-substrate](https://github.com/agent-substrate/substrate) actor (gVisor + checkpoint/restore). Five minutes start to finish on a single `kind` cluster.

**Status: verified end-to-end on bigbox 2026-05-24 against substrate `main` + [PR #75](https://github.com/agent-substrate/substrate/pull/75).**

## The six beats

| # | Beat | What you see |
|---|---|---|
| 1 | Cold ask | A triage checklist comes back from the model (gpt-oss:20b-cloud, free tier). ~6s. |
| 2 | Suspend | `kubectl ate suspend actor helpdesk-1` → STATUS_SUSPENDED, both pool pods FREE. |
| 3 | Idle | Twenty seconds with no compute consumed. Workers stay free. |
| 4 | Follow-up | A second user message implicitly resumes the actor. The agent remembers the original problem. ~4s. |
| 5 | Exfil deny | A second endpoint (`/probe`) tries to fetch `http://evil.example.com/`. OpenShell's OPA policy denies via the HTTP CONNECT proxy; the agent returns `{"blocked": true, "http_status": 403, ...}`. |
| 6 | Migration | `kubectl delete pod` against the worker hosting the actor. The next user message lands on a free worker, with full chat history intact. ~4s. |

Beat 6 is the substrate-side reason PR #75 exists: before that change, pod deletion stranded the actor in `STATUS_RUNNING` pointing at a dead pod and atenet timed out.

## Prerequisites

| Tool | Version | Install |
|---|---|---|
| Linux host (Ubuntu/Debian/Fedora; macOS works via Docker Desktop with ≥12 GiB RAM) | — | — |
| `docker` | 28+ | distro package |
| `kind` | v0.31.0+ | `go install sigs.k8s.io/kind@v0.31.0` |
| `kubectl` | matches kind | distro package |
| `go` | 1.22+ | distro package |
| `cargo` (rust) | 1.88+ | rustup |
| `ko` | latest | `go install github.com/google/ko@latest` |
| `jq` | 1.6+ | distro package |
| `curl` | any | distro package |
| Ollama Cloud API key (free tier) | — | https://ollama.com/settings |

You also need three source checkouts. The first two are personal forks of the upstream repos; the helpdesk image is built on top of `tests/integration/` in this repo.

```bash
# 1. substrate, on the slim fix branch
git clone https://github.com/agent-substrate/substrate ~/go/src/github.com/agent-substrate/substrate
cd ~/go/src/github.com/agent-substrate/substrate
git remote add dims https://github.com/dims/substrate
git fetch dims fix/actor-resume-recovery
git checkout fix/actor-resume-recovery   # PR #75

# 2. patched OpenShell with OPENSHELL_BEST_EFFORT_FAILURES gate
git clone https://github.com/dims/OpenShell ~/go/src/github.com/nvidia/OpenShell
cd ~/go/src/github.com/nvidia/OpenShell
git checkout chore/gvisor-degraded-netns-v2   # b6d3a35

# 3. this repo (you're reading this README inside it)
git clone https://github.com/dims/openshell-driver-substrate ~/go/src/github.com/dims/openshell-driver-substrate
```

## Quick start

From a clean host, the bring-up is four phases of roughly five minutes each.

### 1. Stand up the kind cluster + substrate

```bash
cd ~/go/src/github.com/agent-substrate/substrate
./hack/create-kind-cluster.sh
./hack/install-ate-kind.sh --deploy-ate-system
go install ./cmd/kubectl-ate     # plugin lands in $GOBIN; export PATH if needed
```

`install-ate-kind.sh` runs `ko publish` for `ate-api-server` / `atelet` / `atenet`, applies the kustomize manifests, and waits for the rollout. Rerunning it after a substrate code change rebuilds and rolls forward in place.

### 2. Build the helpdesk supervisor image

Stage the four helpdesk-specific files into the integration harness, then run its image-build script:

```bash
cd ~/go/src/github.com/dims/openshell-driver-substrate
HARNESS=$PWD/tests/integration
HELP=$PWD/examples/helpdesk

cp "$HELP/helpdesk-agent.py"   "$HARNESS/"
cp "$HELP/helpdesk-data.yaml"  "$HARNESS/data.yaml"      # replaces harness default
cp "$HELP/helpdesk.Dockerfile" "$HARNESS/Dockerfile.helpdesk"

# Stage your local routes file with the real Ollama key (see Configuration below).
cp "$HELP/routes.yaml" "$HARNESS/routes.local.yaml"
sed -i 's|<your-ollama-cloud-key>|'"$(cat ~/.config/ollama/key)"'|' "$HARNESS/routes.local.yaml"

cd "$HARNESS" && ./build-image.sh                        # prints SUPERVISOR_IMAGE digest
```

The harness `build-image.sh` is opinionated and copies a fixed list of files into the build context. Extend its `cp` block to pick up `helpdesk-agent.py`, `routes.local.yaml`, and `Dockerfile.helpdesk`, and switch the `--file` argument to `Dockerfile.helpdesk`. The existing `oshl-feature-test` build remains the base image; the helpdesk Dockerfile is a thin derivative on top of it.

### 3. Apply the helpdesk ActorTemplate

```bash
ATEOM_IMAGE=$(docker inspect localhost:5001/ateom-gvisor:latest --format '{{index .RepoDigests 0}}')
SUPERVISOR_IMAGE=$(cat "$HARNESS/.supervisor-image-digest")

sed -e "s|__ATEOM_IMAGE__|$ATEOM_IMAGE|" \
    -e "s|__SUPERVISOR_IMAGE__|$SUPERVISOR_IMAGE|" \
    "$HELP/helpdesk-template.yaml" | kubectl apply -f -

kubectl wait -n ate-demo-helpdesk --for=condition=Ready pod -l ate.dev/worker-pool=helpdesk-pool --timeout=120s
```

### 4. Run the demo

```bash
"$HELP/helpdesk-run.sh"
```

You should see six labelled beats land in sequence and exit zero. The migration beat (6) prints the old worker name and the new one — they must differ.

## Configuration

| File | Purpose |
|---|---|
| `helpdesk-agent.py` | Python helpdesk agent (Flask-shaped `http.server`). Holds chat history in a Python list that survives gVisor checkpoint/restore. Forwards to `inference.local` over HTTPS via the OpenShell CONNECT proxy. |
| `helpdesk-data.yaml` | OpenShell sandbox policy data: `filesystem_policy`, `landlock: best_effort`, `run_as_user: root`, `network_policies: {}` (default-deny). |
| `routes.yaml` | OpenShell standalone-mode route config. Template only — copy to `routes.local.yaml` (gitignored) and paste your Ollama key before staging into the build context. |
| `helpdesk-template.yaml` | substrate `WorkerPool` + `ActorTemplate`. Placeholders `__ATEOM_IMAGE__` and `__SUPERVISOR_IMAGE__` get rendered at apply time. |
| `helpdesk.Dockerfile` | Two-layer derivative on top of the harness's `oshl-feature-test` image: copy `helpdesk-agent.py` to `/opt/helpdesk/agent.py`, copy `routes.local.yaml` to `/etc/openshell/routes.yaml`. |
| `helpdesk-run.sh` | Six-beat demo driver. Port-forwards `svc/atenet-router` to `localhost:8000`, sends curl calls with the `Host:` header substrate's atenet router uses to demux actors, prints timings. |
| `beat6-test.sh` | Focused beat-6 test for iterating on the migration path without re-running the full driver. |

### Environment overrides

The agent reads three env vars at request time (not start time), so you can change them with `kubectl set env` and the next user message picks them up:

| Var | Default | Purpose |
|---|---|---|
| `HELPDESK_MODEL` | `gpt-oss:20b-cloud` | Free-tier Ollama Cloud model. Swap to `gpt-oss:120b-cloud` if 20b returns 429. |
| `HELPDESK_PROBE_URL` | `http://evil.example.com/` | Target for the beat-5 exfil attempt. Change for different audiences. |
| `OPENSHELL_INFERENCE_BASE` | `https://inference.local/v1` | Override the OpenShell short-circuit target if you point at a local Ollama instead of cloud. |

## Expected output

Edited transcript from the verified bigbox run on 2026-05-24:

```
=== Beat 1: Cold ask ===
{"reply": "**Database Timeout Triage Checklist** ... 12 numbered steps ...", "history_turns": 1}
real    0m6.113s

=== Beat 2: Suspend ===
helpdesk-1   STATUS_SUSPENDED   <none>                 5
status=STATUS_SUSPENDED; worker=

=== Beat 3: 20-second idle (capacity recovered) ===
NAMESPACE           POOL            POD                                         STATUS   ASSIGNED ACTOR
ate-demo-helpdesk   helpdesk-pool   helpdesk-pool-deployment-...-f8zx2          FREE     <none>
ate-demo-helpdesk   helpdesk-pool   helpdesk-pool-deployment-...-tchqb          FREE     <none>

=== Beat 4: Follow-up (implicit resume) ===
{"reply": "The user reported that their **database connection is timing out**.", "history_turns": 2}
real    0m3.572s

=== Beat 5: Exfil attempt (expect blocked + OCSF Denied event) ===
{"blocked": true, "url": "http://evil.example.com/", "http_status": 403, "reason": "HTTP Error 403: Forbidden",
 "explanation": "OpenShell HTTP CONNECT proxy denied per OPA policy"}

=== Beat 6: Migrate (kill the worker, ask again, land on a new worker) ===
pod "helpdesk-pool-deployment-...-f8zx2" deleted from ate-demo-helpdesk namespace
{"reply": "Got it—User **foo** reported that their database is timing out. ...", "history_turns": 2}
real    0m4.395s
old worker: helpdesk-pool-deployment-...-f8zx2 → new worker: helpdesk-pool-deployment-...-m27l8
```

Key signals to look for:

- Beat 4 `history_turns: 2` proves chat memory survived the suspend.
- Beat 5 returns `blocked: true` with HTTP 403, not a 200.
- Beat 6 `history_turns: 2` proves chat memory also survived the pod-delete migration. The old and new worker names must differ.

## Troubleshooting

The corrections accumulated across live runs. Most live in `../2026-05-24-helpdesk-demo-plan.md` Appendix Z; the load-bearing ones:

**`kubectl ate: unknown command 'ate'`** — non-interactive SSH didn't pick up `$GOBIN`. Either `export PATH="$HOME/go/bin:$PATH"` in `.bashrc` (or whatever the non-login shell reads), or ship the binary to `/usr/local/bin`. The driver script already does the export at the top.

**`error: services "atenet-router-envoy" not found`** — substrate vintage. Older builds expose `svc/atenet-router`; newer ones add `-envoy`. The driver uses `atenet-router`. Patch with `kubectl get svc -n ate-system | grep atenet` and update the port-forward line accordingly.

**Beat 1 returns 502 with `Name or service not known`** — the supervisor's HTTPS_PROXY injection didn't reach Python. The agent has a defensive fallback (sets `HTTPS_PROXY=http://127.0.0.1:3128` if unset); if you see this anyway, double-check the supervisor came up cleanly (`kubectl logs -c supervisor <pod>` should show `[helpdesk] listening on :80, model=gpt-oss:20b-cloud`).

**Beat 4 returns "I have no memory of a prior conversation"** — the actor lost its snapshot. Check `kubectl ate get actor helpdesk-1 -o json | jq '.actors[0].lastSnapshot'` — it must be non-empty. The most common cause is a failed checkpoint (gVisor `inconsistent private memory files on restore` after a sandbox grandchild SIGPIPE), which substrate's `cmdRestore` reports as `internal server error`. Pull `kubectl logs -n ate-system ds/atelet` for the underlying runsc stderr.

**Beat 6 returns 502 or stalls** — without PR #75 the actor stays `STATUS_RUNNING` pointing at the deleted pod and atenet times out forwarding to a dead IP. Verify your ate-api-server image was built from a tree that contains `cmd/ateapi/internal/controlapi/syncer.go`'s `releaseActorOnDeadWorker` helper:

```bash
kubectl exec -n ate-system deploy/ate-api-server-deployment -- /ate-api-server --help 2>&1 | head -1
# then grep the source on disk, or look at the image digest in the deployment spec.
```

If the helper is absent, you're running pre-PR-#75 substrate. Rebuild from a branch that has it.

**Pool pods stuck `Pending` / `ImagePullBackOff`** — the `__ATEOM_IMAGE__` placeholder wasn't rendered. Re-run the `sed | kubectl apply` step in Quick Start step 3, and confirm `docker inspect localhost:5001/ateom-gvisor:latest --format '{{index .RepoDigests 0}}'` prints a real digest (force a `docker pull` first if it returns empty).

## Cleanup

```bash
kubectl delete -n ate-demo-helpdesk actortemplate helpdesk-agent workerpool helpdesk-pool
kubectl delete namespace ate-demo-helpdesk

# Tear the cluster down only if you don't want to iterate on it.
~/go/src/github.com/agent-substrate/substrate/hack/delete-kind-cluster.sh
```

Rotate the Ollama Cloud API key at https://ollama.com/settings if it sat in a local `routes.local.yaml` long enough to matter.

## Further reading

- [`../../tests/integration/`](../../tests/integration/) — the supervisor-image harness this demo builds on (`build-image.sh`, base `Dockerfile`, policy `data.yaml`, OpenShell `policy.rego`).
- [`../../docs/poc-intro.md`](../../docs/poc-intro.md) — architecture overview for the OpenShell-on-Substrate driver.
- [substrate PR #75](https://github.com/agent-substrate/substrate/pull/75) — `ateapi/syncer: release actor when host pod is deleted` (Beat 6's enabling change).
- [substrate PR #73](https://github.com/agent-substrate/substrate/pull/73) — `ActorTemplate.spec.containers[].securityContext` (per-container caps + UID/GID).
- [substrate PR #66](https://github.com/agent-substrate/substrate/pull/66) — `ateom-gvisor` eth0 idempotency on restore.
- [NVIDIA/OpenShell PR #1548](https://github.com/NVIDIA/OpenShell/pull/1548) — `OPENSHELL_BEST_EFFORT_FAILURES` gate, the upstream-shaped change inside OpenShell.
