# OpenShell helpdesk on Agent Substrate

A 10-beat demo that runs an LLM-backed helpdesk agent inside an OpenShell sandbox, on top of an [agent-substrate](https://github.com/agent-substrate/substrate) actor (gVisor + checkpoint/restore). **Every sandbox lifecycle call flows through `openshell-driver-substrate` — the gateway is the operator-facing surface, the driver is the implementation.**

```
operator  ──gRPC──>  openshell-gateway  ──in-process──>  openshell-driver-substrate  ──gRPC──>  ate-api-server
                     (compute_drivers =                  (this crate)                            (substrate)
                       ["substrate"])
```

**Status: verified end-to-end on bigbox 2026-05-24** against substrate `main` + [PR #75](https://github.com/agent-substrate/substrate/pull/75) and `dims/OpenShell@integration/openshell-driver-substrate` tip `753d3e4c` (M3.14–M3.16). Every one of the gateway's lifecycle RPCs (`CreateSandbox`, `GetSandbox`, `ListSandboxes`, `DeleteSandbox`) executed against the driver in the verified run; the driver's `validate_sandbox_create` + `create_sandbox` + `list_sandboxes` + `delete_sandbox` were all exercised. The supervisor stays in standalone mode (policy + OPA + Ollama Cloud routing all baked into the image) so the data plane (`/chat`, `/probe`) hits atenet directly — the demo proves the driver's control plane, not the gateway's data plane (the §7b POC covers that separately).

## Recording

A ~2 minute screen recording of the full 10-beat run — two tmux panes, top is the live `kubectl-ate get actors / get workers` watch, bottom walks the beats. Every command is echoed at a green `$` prompt before its output, so the recording is intelligible without audio.

- [`helpdesk-demo.mp4`](helpdesk-demo.mp4) — h.264, ~1.6 MB. Plays in any browser / QuickTime / Slack.

Regenerate from `~/Downloads/helpdesk-demo.cast` (or any fresh asciicast) with:
```sh
agg --idle-time-limit 2 --font-size 14 helpdesk-demo.cast helpdesk-demo.gif
ffmpeg -y -i helpdesk-demo.gif -movflags +faststart -pix_fmt yuv420p \
       -vf "scale=trunc(iw/2)*2:trunc(ih/2)*2" \
       -c:v libx264 -preset veryslow -crf 23 helpdesk-demo.mp4
```

## The 10 beats

Organized as three acts.

| # | Beat | What it proves | RPC path |
|---|---|---|---|
| **I — Sandbox provisioning** | | | |
| 1 | Provision `alice` via `OpenShell.CreateSandbox` | The gateway accepts a public-API CreateSandbox, translates it, calls into the driver, which creates an `Actor` in substrate. The pre-applied `helpdesk-agent` ActorTemplate is referenced via `SandboxTemplate.annotations["substrate_actor_template"]` (M3.16). | Gateway → driver.validate_sandbox_create + create_sandbox → ateapi.CreateActor + ResumeActor |
| 2 | Provision `bob` | Multi-tenant: a second actor in the same pool. | (same as Beat 1) |
| 3 | `OpenShell.ListSandboxes` | List works through the driver and projects substrate's actor state into the gateway's sandbox model. | Gateway → driver.list_sandboxes → ateapi.ListActors |
| **II — One actor's life** | | | |
| 4 | Cold ask to alice (data plane) | Ollama Cloud round-trip works: gpt-oss:20b-cloud returns a triage checklist. ~4s. | curl → atenet → sandbox → OPA-proxied egress |
| 5 | Suspend alice (substrate admin op) | Actor goes `STATUS_SUSPENDED`, worker freed. The OpenShell public API has no Suspend RPC, so this step uses `kubectl ate suspend` directly. | (substrate ateapi.SuspendActor) |
| 6 | 20-second idle | Both workers `FREE`. Idle costs zero. | (no calls) |
| 7 | Follow-up to alice | Implicit resume on traffic. Chat history survives the suspend (`history_turns: 2`). | curl → atenet → substrate resumes from snapshot |
| 8 | Exfil from bob | `/probe` against `http://evil.example.com/` returns `{"blocked": true, "http_status": 403}` via OpenShell's OPA-proxied CONNECT deny. | curl → atenet → sandbox → OPA |
| 9 | **Pod-kill migration** | `kubectl delete pod` against alice's worker. PR #75's `releaseActorOnDeadWorker` resets alice to `STATUS_SUSPENDED`; next curl triggers implicit resume on a free worker. Chat history survives. **Bob is unaffected** — the load-bearing multi-tenant proof. | (PR #75 syncer hook) |
| **III — Hygiene** | | | |
| 10 | `OpenShell.DeleteSandbox` alice | Driver deletes alice's actor; the pre-provisioned `helpdesk-agent` ActorTemplate survives (driver only reaps templates it synthesized itself). Bob keeps talking. | Gateway → driver.delete_sandbox → ateapi.DeleteActor |

## Prerequisites

| Tool | Version | Install |
|---|---|---|
| Linux host (Ubuntu/Debian/Fedora; macOS works via Docker Desktop with ≥12 GiB RAM) | — | — |
| `docker` | 28+ | distro package |
| `kind` | v0.31.0+ | `go install sigs.k8s.io/kind@v0.31.0` |
| `kubectl` | matches kind | distro package |
| `go` | 1.22+ | distro package |
| `cargo` (rust, for image builds) | 1.88+ | rustup |
| `ko` | latest | `go install github.com/google/ko@latest` |
| `kubectl-osh` (this repo) | built locally | `(cd cmd/kubectl-osh && make install)` |
| `jq` | 1.6+ | distro package |
| `curl` | any | distro package |
| Ollama Cloud API key (free tier) | — | https://ollama.com/settings |

### Companion changes upstream

This demo lives on the in-flight M3 work for both substrate and OpenShell. You need these PRs/branches on top of upstream `main`:

| Repo + branch / PR | Required for | Status |
|---|---|---|
| [`dims/OpenShell@integration/openshell-driver-substrate`](https://github.com/dims/OpenShell/tree/integration/openshell-driver-substrate) (tip `753d3e4c`, M3.14–M3.16) | The gateway-side wiring (`ComputeDriverKind::Substrate` dispatch arm + `substrate_actor_template` annotation path) | local branch on personal fork |
| [`agent-substrate/substrate#75`](https://github.com/agent-substrate/substrate/pull/75) | Beat 9 (pod-kill migration) — without it, alice would strand in `STATUS_RUNNING` pointing at a dead pod | open PR |
| [`agent-substrate/substrate#67`](https://github.com/agent-substrate/substrate/pull/67) | `install-ate-kind.sh --deploy-ate-system` publishes the `ateom-gvisor` image | open PR; skip if you already have `localhost:5001/ateom-gvisor` cached |
| [`agent-substrate/substrate#66`](https://github.com/agent-substrate/substrate/pull/66) | `ateom-gvisor` eth0 idempotency on restore — strongly recommended for repeated Beat 9 cycles | open PR |
| [`NVIDIA/OpenShell#1548`](https://github.com/NVIDIA/OpenShell/pull/1548) | `OPENSHELL_BEST_EFFORT_FAILURES` gate — required, but the integration/openshell-driver-substrate branch above already carries the equivalent best-effort patches | open PR |

The driver-side work (M3.1–M3.13) is already on `integration/openshell-driver-substrate`. M3.14 + M3.16 land in this same demo cycle:

- **M3.14**: `wire ComputeDriverKind::Substrate dispatch into gateway compute runtime` — replaces the scaffold's placeholder `Err(...)` arm with a real `ComputeRuntime::new_substrate(...)` call, statically links the driver crate into the gateway binary.
- **M3.16**: `read substrate_actor_template from public-API annotations path` — extends `template_name_from_spec` to also look under `platform_config["annotations"]["substrate_actor_template"]`, which is where the gateway's `build_platform_config` puts `SandboxTemplate.annotations`. Without this, the public OpenShell CreateSandbox API has no way to reference a pre-provisioned ActorTemplate.

## Quick start

Three working trees:

```bash
# 1. substrate, with PRs #66 + #67 + #75 merged.
git clone https://github.com/agent-substrate/substrate ~/go/src/github.com/agent-substrate/substrate
cd ~/go/src/github.com/agent-substrate/substrate
git remote add dims https://github.com/dims/substrate
git fetch dims fix/actor-resume-recovery feat/install-publish-ateom-image fix/ateom-gvisor-eth0-rollback
git checkout -b try/helpdesk-prereqs dims/fix/actor-resume-recovery
git merge --no-edit dims/feat/install-publish-ateom-image dims/fix/ateom-gvisor-eth0-rollback

# 2. OpenShell with the M3 driver wiring.
git clone https://github.com/dims/OpenShell ~/go/src/github.com/nvidia/OpenShell-driver-substrate
cd ~/go/src/github.com/nvidia/OpenShell-driver-substrate
git checkout integration/openshell-driver-substrate

# 3. this repo (you're reading this README inside it).
git clone https://github.com/dims/openshell-driver-substrate ~/go/src/github.com/dims/openshell-driver-substrate
```

Bring-up:

```bash
# 4. Stand up the substrate kind cluster.
cd ~/go/src/github.com/agent-substrate/substrate
./hack/create-kind-cluster.sh
./hack/install-ate-kind.sh --deploy-ate-system
go install ./cmd/kubectl-ate

# 5. Build the helpdesk supervisor image.
cd ~/go/src/github.com/dims/openshell-driver-substrate
HARNESS=$PWD/tests/integration
cp examples/helpdesk/{helpdesk-agent.py,helpdesk.Dockerfile} "$HARNESS/"
cp examples/helpdesk/helpdesk-data.yaml "$HARNESS/data.yaml"
cp examples/helpdesk/routes.yaml         "$HARNESS/routes.local.yaml"
sed -i 's|<your-ollama-cloud-key>|'"$(cat ~/.config/ollama/key)"'|' \
    "$HARNESS/routes.local.yaml"
# Then run the harness's build script (see tests/integration/README.md
# for the cp-block extension required to include the helpdesk files).

# 6. Apply WorkerPool + pre-provisioned ActorTemplate.
ATEOM_IMAGE=$(docker inspect localhost:5001/ateom-gvisor:latest \
                  --format '{{index .RepoDigests 0}}')
SUPERVISOR_IMAGE=$(cat "$HARNESS/.supervisor-image-digest")
sed -e "s|__ATEOM_IMAGE__|$ATEOM_IMAGE|" \
    -e "s|__SUPERVISOR_IMAGE__|$SUPERVISOR_IMAGE|" \
    examples/helpdesk/helpdesk-template.yaml | kubectl apply -f -
kubectl wait -n ate-demo-helpdesk --for=condition=Ready pod \
    -l ate.dev/worker-pool=helpdesk-pool --timeout=120s

# 7. Stand up the OpenShell gateway (with the substrate driver compiled in).
kubectl create namespace ate-openshell-m0
cd "$HARNESS/gateway"  # tests/integration/gateway/
bash generate-jwt-keys.sh | kubectl apply -f -

cd ~/go/src/github.com/dims/openshell-driver-substrate
# Copy substrate's ate-api-server CA into ate-openshell-m0 so the
# gateway pod can verify the api server's TLS cert (the kube-root-ca
# won't validate it — substrate uses its own podcertificate CA signer).
kubectl get secret -n podcertificate-controller-system service-dns-ca-pool \
  -o jsonpath='{.data.pool}' | base64 -d | \
  jq -r '.CAs[].RootCertificateDER' | while read der; do
    echo "-----BEGIN CERTIFICATE-----"
    echo "$der" | fold -w 64
    echo "-----END CERTIFICATE-----"
  done > /tmp/ate-api-ca.pem
kubectl create secret generic -n ate-openshell-m0 ate-api-server-ca \
    --from-file=ca.crt=/tmp/ate-api-ca.pem --dry-run=client -o yaml | \
    kubectl apply -f -

./examples/helpdesk/gateway/run.sh   # builds the gateway image,
                                     # deploys RBAC + ConfigMap +
                                     # Deployment + Service.

# 8. Build + install kubectl-osh on PATH. The demo invokes it as
#    `kubectl osh ...`; protos ship with the source tree, no manual
#    .proto staging needed.
(cd cmd/kubectl-osh && make install)

# 9. Run the demo.
SUPERVISOR_IMAGE="$SUPERVISOR_IMAGE" ./examples/helpdesk/helpdesk-run.sh
```

## What's in this folder

| File | Purpose |
|---|---|
| `helpdesk-agent.py` | Python helpdesk agent (Flask-shaped `http.server`). RAM-only chat history. Calls Ollama Cloud via the OpenShell CONNECT proxy. |
| `helpdesk-data.yaml` | OpenShell sandbox policy data (filesystem + landlock + process + network rules). `network_policies` is empty for default-deny. |
| `routes.yaml` | OpenShell standalone-mode route config. **Template only** — copy to `routes.local.yaml` (gitignored) and paste your Ollama key before staging into the build context. |
| `helpdesk-template.yaml` | substrate `WorkerPool` + `ActorTemplate`. Operator-applied; the gateway/driver references the template by name via `SandboxTemplate.annotations["substrate_actor_template"]`. |
| `helpdesk.Dockerfile` | Two-layer derivative on top of the harness's `oshl-feature-test` image. |
| `helpdesk-run.sh` | 10-beat demo driver. Hits `openshell.v1.OpenShell` (gateway) for lifecycle + atenet for data plane. |
| `beat6-test.sh` | Focused beat-6 (migration) test for iterating without re-running the full driver. |
| `gateway/` | Subdirectory for the substrate-driven OpenShell gateway image + manifests. See `gateway/README.md` for details. |

## What's in `gateway/`

| File | Purpose |
|---|---|
| `Dockerfile` | Builds `openshell-gateway` from `OpenShell-driver-substrate` (the M3 branch with the substrate driver). Same multi-stage shape as `tests/integration/gateway/Dockerfile.gateway`, different source tree. |
| `build-image.sh` | Wraps the Docker build + push to the kind-registry. Prints the resulting `<repo>@sha256:<digest>`. |
| `gateway-rbac.yaml` | `ServiceAccount openshell-gateway-substrate` in `ate-openshell-m0` + `ClusterRoleBinding` to the existing `ate-controller` ClusterRole (cluster-wide RBAC for substrate ateapi operations). |
| `gateway-config.yaml` | ConfigMap with `gateway.toml`: `compute_drivers = ["substrate"]` + `[openshell.drivers.substrate]` block. Reuses the §7b POC's JWT signing key. Includes a stub `[openshell.drivers.kubernetes]` block to satisfy the gateway's in-cluster JWT bootstrap check (SE-8) — the kubernetes driver is never invoked. |
| `gateway-deployment.yaml` | Deployment + Service. Projected volume mounts a kubelet-rotated SA token (audience `api.ate-system.svc`) + the substrate ate-api-server CA at `/etc/openshell-substrate/{token,ca.crt}` for the driver to use. |
| `run.sh` | End-to-end orchestration: apply RBAC + ConfigMap, build the image, render + apply the Deployment, wait for rollout. |

## Verified output (excerpt)

```
=== Beat 1: Provision alice via OpenShell.CreateSandbox (driver path) ===
{
  "sandbox": {
    "metadata": {"id": "ef02921b-c6be-47bc-92b6-869bd952ef2b", "name": "alice"},
    "spec": {
      "logLevel": "info",
      "template": {
        "image": "localhost:5001/oshl-helpdesk@sha256:400efe27...",
        "annotations": {"substrate_actor_template": "helpdesk-agent"}
      },
      "policy": {"version": 1, "process": {"runAsUser": "sandbox", "runAsGroup": "sandbox"}}
    },
    "phase": "SANDBOX_PHASE_PROVISIONING"
  }
}
real    0m2.820s
  -> alice → actor_id ef02921b-c6be-47bc-92b6-869bd952ef2b

=== Beat 3: ListSandboxes ===
[
  {"id": "ef02921b-...", "phase": "SANDBOX_PHASE_READY"},
  {"id": "f4101364-...", "phase": "SANDBOX_PHASE_READY"}
]

=== Beat 4: Cold ask to alice ===
{"reply": "**Database Timeout Triage Checklist** ...", "history_turns": 1}

=== Beat 7: Follow-up to alice (implicit resume) ===
{"reply": "You asked about a user (foo) who reported that **their database was timing out**.", "history_turns": 2}

=== Beat 8: Exfil attempt from bob ===
{"blocked": true, "url": "http://evil.example.com/", "http_status": 403, ...}

=== Beat 9: Kill alice's pod — alice migrates, bob is unaffected ===
{"reply": "Yes—User **foo** reported that their database is timing out.", "history_turns": 2}
alice: helpdesk-pool-deployment-...-m27l8 → helpdesk-pool-deployment-...-44nlm
bob still on: helpdesk-pool-deployment-...-tchqb

=== Beat 10: Delete alice via OpenShell.DeleteSandbox ===
{"deleted": true}
post-delete list: [{"id": "f4101364-...", "phase": "SANDBOX_PHASE_READY"}]
Pre-provisioned ActorTemplate survives: helpdesk-agent
```

Key signals:

- Beat 1 returns within ~3s — gateway → driver → CreateActor + ResumeActor round-trip.
- Beat 7 `history_turns: 2` proves the suspend/resume cycle preserved alice's chat memory.
- Beat 9 `history_turns: 2` proves the pod-kill migration preserved it again. Alice's worker name changes; bob's doesn't.
- Beat 10 `"deleted": true` confirms the driver acknowledged the delete; post-list shows only bob; the pre-provisioned ActorTemplate is untouched.

## Troubleshooting

**Beat 1 returns `failed to connect to Substrate ate-api-server at api.ate-system.svc:443: transport error`** — the gateway pod can't reach or can't verify the ate-api-server's TLS cert. Most likely the `ate-api-server-ca` Secret in `ate-openshell-m0` is missing or has the wrong CA. Re-run the JSON→PEM conversion (see Quick Start step 7). Confirm the bundle validates with `openssl s_client -connect <api-svc-ip>:443 -servername api.ate-system.svc -CAfile /tmp/ate-api-ca.pem`.

**Gateway pod CrashLoops with `K8s ServiceAccount bootstrap requires [openshell.drivers.kubernetes]`** — SE-8. Re-apply `gateway/gateway-config.yaml` which contains the stub `[openshell.drivers.kubernetes]` block.

**Beat 4 returns "not found" from atenet** — the demo client is using the wrong actor ID for the `Host:` header. The gateway assigns sandbox `metadata.id` as a UUID, and that UUID is the substrate actor_id. The demo captures the returned id from CreateSandbox into the `ACTOR_ID[alice]` shell associative array; check that it was populated.

**Beat 9 returns 502 or stalls** — substrate without PR #75. The actor stays `STATUS_RUNNING` pointing at the deleted pod and atenet times out. Verify your ate-api-server build is on a branch that contains `cmd/ateapi/internal/controlapi/syncer.go`'s `releaseActorOnDeadWorker` helper.

## Cleanup

```bash
# Delete both demo actors via the gateway.
OPENSHELL_GATEWAY=localhost:50051 \
  kubectl osh delete sandbox alice bob --ignore-not-found

# Tear down the gateway.
kubectl delete -n ate-openshell-m0 deploy/openshell-gateway-substrate svc/openshell-gateway-substrate
kubectl delete configmap -n ate-openshell-m0 openshell-gateway-substrate-config
kubectl delete clusterrolebinding openshell-gateway-substrate
kubectl delete sa -n ate-openshell-m0 openshell-gateway-substrate

# Tear down helpdesk.
kubectl delete -n ate-demo-helpdesk actortemplate helpdesk-agent workerpool helpdesk-pool
kubectl delete namespace ate-demo-helpdesk

# Only if you don't want to iterate on the cluster:
~/go/src/github.com/agent-substrate/substrate/hack/delete-kind-cluster.sh
```

## Further reading

- [`../../src/lib.rs`](../../src/lib.rs) — the substrate driver's `ComputeDriver` implementation.
- [`../../tests/live.rs`](../../tests/live.rs) — driver-side integration tests (the unit-test analog of this demo).
- [`../../tests/integration/gateway/`](../../tests/integration/gateway/) — the §7b POC: real openshell-gateway against substrate, verifying the supervisor↔gateway channel (F1/F2/F3 — settings poll, inference routing, log push). Complementary to this demo, which verifies the gateway↔driver↔substrate channel (create/list/delete).
- [`../../docs/poc-intro.md`](../../docs/poc-intro.md) — architecture overview for the OpenShell-on-Substrate driver.
- [substrate PR #75](https://github.com/agent-substrate/substrate/pull/75) — `ateapi/syncer: release actor when host pod is deleted` (Beat 9's enabling change).
- [substrate PR #73](https://github.com/agent-substrate/substrate/pull/73) — `ActorTemplate.spec.containers[].securityContext`.
- [substrate PR #66](https://github.com/agent-substrate/substrate/pull/66) — `ateom-gvisor` eth0 idempotency on restore.
- [NVIDIA/OpenShell PR #1548](https://github.com/NVIDIA/OpenShell/pull/1548) — `OPENSHELL_BEST_EFFORT_FAILURES` gate.
