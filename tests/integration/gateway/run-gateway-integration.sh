#!/usr/bin/env bash
# §7b end-to-end harness: deploy the OpenShell gateway, render a templates
# pair with a minted sandbox JWT, spawn a test actor, exercise each of the
# five gateway-driven features, and dump the evidence.
#
# Pre-requisites the operator must satisfy before calling this script:
#   - The bigbox kind cluster is up with the standard substrate install.
#   - ATEOM_IMAGE points at a published ateom-gvisor digest (the standard
#     POC bootstrap; see ~/notes/.../2026-05-23-openshell-on-substrate-state.md).
#   - The Ed25519 JWT signing material is present at the paths in
#     /etc/openshell-jwt/{signing.pem,public.pem,kid}. The gateway-secret.yaml
#     applied earlier in this harness mounts them inside the gateway pod,
#     but the JWT-minting helper needs them locally too — pass via
#     OPENSHELL_JWT_DIR (default: /tmp).
#
# Outputs:
#   /tmp/oshl-v3-<TS>/         — per-run evidence directory
#     gateway-image            — gateway image digest
#     supervisor-image         — supervisor image digest
#     sandbox-token            — minted JWT (raw)
#     gateway-pod.log          — full gateway-pod stdout/stderr
#     supervisor-tail.log      — tail of the actor's supervisor log
#     features.md              — per-feature PASS/FAIL summary
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INTEG_ROOT="$(cd "$HERE/.." && pwd)"
NAMESPACE="${NAMESPACE:-ate-openshell-m0}"
GATEWAY_ID="${GATEWAY_ID:-openshell-poc}"
# Actor ID = unique per run, used by ateapi.Control.CreateActor.
ACTOR_ID="${ACTOR_ID:-oshl-v3-$(date +%s)}"
# Sandbox ID = identity the supervisor presents to the gateway. Pre-
# provisioned templates can't carry per-actor IDs, so we use a fixed
# POC value here and mint a JWT with sub=<this>.
SANDBOX_ID="${SANDBOX_ID:-poc-sandbox}"
JWT_DIR="${OPENSHELL_JWT_DIR:-/tmp}"
EVIDENCE_DIR="${EVIDENCE_DIR:-/tmp/oshl-v3-$(date +%s)}"

# Path overrides (rarely needed)
SUPERVISOR_BUILD_SCRIPT="${INTEG_ROOT}/build-image.sh"
GATEWAY_BUILD_SCRIPT="${HERE}/build-gateway-image.sh"
GATEWAY_DEPLOY_SCRIPT="${HERE}/run-gateway.sh"
MINT_SCRIPT="${HERE}/mint-sandbox-token.py"

mkdir -p "$EVIDENCE_DIR"
echo "[run-gw] evidence directory: $EVIDENCE_DIR" >&2

for p in "$SUPERVISOR_BUILD_SCRIPT" "$GATEWAY_BUILD_SCRIPT" "$GATEWAY_DEPLOY_SCRIPT" "$MINT_SCRIPT"; do
  [ -x "$p" ] || { echo "[run-gw] required script missing or not executable: $p" >&2; exit 1; }
done

for f in signing.pem public.pem kid; do
  [ -f "$JWT_DIR/openshell-jwt-$f" ] || {
    echo "[run-gw] missing JWT material: $JWT_DIR/openshell-jwt-$f" >&2
    echo "[run-gw] generate via: openssl genpkey -algorithm ED25519 -out $JWT_DIR/openshell-jwt-signing.pem" >&2
    exit 1
  }
done

# ─── 1. Build images ─────────────────────────────────────────────────────
echo "[run-gw] phase 1: building supervisor + gateway images" >&2
SUPERVISOR_IMAGE="$("$SUPERVISOR_BUILD_SCRIPT")"
GATEWAY_IMAGE="$("$GATEWAY_BUILD_SCRIPT")"
echo "$SUPERVISOR_IMAGE" > "$EVIDENCE_DIR/supervisor-image"
echo "$GATEWAY_IMAGE"    > "$EVIDENCE_DIR/gateway-image"
echo "[run-gw]   supervisor: $SUPERVISOR_IMAGE" >&2
echo "[run-gw]   gateway:    $GATEWAY_IMAGE"    >&2

# ─── 2. Deploy the gateway ───────────────────────────────────────────────
echo "[run-gw] phase 2: deploying the gateway" >&2
GATEWAY_IMAGE="$GATEWAY_IMAGE" "$GATEWAY_DEPLOY_SCRIPT"
GATEWAY_ENDPOINT="http://openshell-gateway.${NAMESPACE}.svc.cluster.local:50051"

# ─── 3. Mint a sandbox JWT ───────────────────────────────────────────────
echo "[run-gw] phase 3: minting sandbox JWT for $SANDBOX_ID" >&2
TOKEN="$(python3 "$MINT_SCRIPT" \
  --sandbox-id "$SANDBOX_ID" \
  --signing-key "$JWT_DIR/openshell-jwt-signing.pem" \
  --kid-file    "$JWT_DIR/openshell-jwt-kid" \
  --gateway-id  "$GATEWAY_ID")"
echo -n "$TOKEN" > "$EVIDENCE_DIR/sandbox-token"
echo "[run-gw]   token length: ${#TOKEN} chars" >&2

# ─── 4. Render + apply the templates ─────────────────────────────────────
echo "[run-gw] phase 4: rendering + applying templates" >&2

# The supervisor template (used by live tests) — substitute and reapply, then
# delete + recreate to force a fresh golden snapshot.
kubectl -n "$NAMESPACE" delete actortemplate supervisor --ignore-not-found

sed -e "s|__ATEOM_IMAGE__|$ATEOM_IMAGE|g" \
    -e "s|__SUPERVISOR_IMAGE__|$SUPERVISOR_IMAGE|g" \
    -e "s|__GATEWAY_ENDPOINT__|$GATEWAY_ENDPOINT|g" \
    -e "s|__SANDBOX_TOKEN__|$TOKEN|g" \
    "$HERE/cluster-setup-with-gateway.yaml" \
  | kubectl apply -f -

# Feature-test template (used by run.sh path).
kubectl -n "$NAMESPACE" delete actortemplate oshl-feature-test --ignore-not-found

sed -e "s|__IMAGE__|$SUPERVISOR_IMAGE|g" \
    -e "s|__GATEWAY_ENDPOINT__|$GATEWAY_ENDPOINT|g" \
    -e "s|__SANDBOX_TOKEN__|$TOKEN|g" \
    "$HERE/actor-template-with-gateway.yaml" \
  | kubectl apply -f -

echo "[run-gw] waiting for both templates to reach Ready (300 s deadline)" >&2
for tmpl in supervisor oshl-feature-test; do
  for _ in $(seq 1 150); do
    phase=$(kubectl -n "$NAMESPACE" get actortemplate "$tmpl" -o jsonpath='{.status.phase}' 2>/dev/null || true)
    [ "$phase" = "Ready" ] && { echo "[run-gw]   $tmpl Ready"; break; }
    [ "$phase" = "Failed" ] && { echo "[run-gw]   $tmpl FAILED" >&2; exit 1; }
    sleep 2
  done
  [ "$phase" = "Ready" ] || { echo "[run-gw]   $tmpl did not reach Ready (last: $phase)" >&2; exit 1; }
done

# ─── 5. Spawn a test actor + give the supervisor time to phone home ──────
echo "[run-gw] phase 5: spawning actor $ACTOR_ID (sandbox_id=$SANDBOX_ID)" >&2

# Re-mint the gateway port-forward + cluster-trust bundle the same way run.sh does.
kubectl -n ate-system create token ate-controller --audience=api.ate-system.svc > /tmp/ate-bearer.token
kubectl get clustertrustbundle servicedns.podcert.ate.dev:identity:primary-bundle \
  -o jsonpath='{.spec.trustBundle}' > /tmp/ate-servicedns-ca.pem
[ -f /tmp/pf.pid ] && kill "$(cat /tmp/pf.pid)" 2>/dev/null || true
sleep 1
API_POD=$(kubectl -n ate-system get pods -o name | grep ate-api-server | head -1 | sed s,^pod/,,)
nohup kubectl -n ate-system port-forward "$API_POD" 18443:443 >/tmp/pf.log 2>&1 &
echo $! > /tmp/pf.pid
sleep 5

grpcurl -insecure \
  -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $(cat /tmp/ate-bearer.token)" \
  -d "{\"actor_id\":\"$ACTOR_ID\",\"actor_template_namespace\":\"$NAMESPACE\",\"actor_template_name\":\"oshl-feature-test\"}" \
  127.0.0.1:18443 ateapi.Control/CreateActor
grpcurl -insecure \
  -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $(cat /tmp/ate-bearer.token)" \
  -d "{\"actor_id\":\"$ACTOR_ID\",\"boot\":false}" \
  127.0.0.1:18443 ateapi.Control/ResumeActor

echo "[run-gw] sleeping 45s for settings-poll + log-push cycles to complete" >&2
sleep 45

# ─── 6. Dump evidence ────────────────────────────────────────────────────
echo "[run-gw] phase 6: capturing evidence" >&2

GW_POD=$(kubectl -n "$NAMESPACE" get pods -l app=openshell-gateway -o name | head -1)
# The pod has dockerd + gateway containers; default kubectl logs picks
# the first (dockerd). We want the gateway's stdout.
kubectl -n "$NAMESPACE" logs "$GW_POD" -c gateway --tail=20000 > "$EVIDENCE_DIR/gateway-pod.log" 2>&1 || true

for pod in $(kubectl -n "$NAMESPACE" get pods -o name | grep openshell-m0-pool); do
  kubectl -n "$NAMESPACE" logs "$pod" --tail=4000 \
    | grep -E "\[oshl-test|openshell_sandbox|inference|RelayStream|PushSandboxLogs|GetSandboxConfig|sandbox bootstrap" \
    >> "$EVIDENCE_DIR/supervisor-tail.log" 2>/dev/null || true
done

echo "[run-gw] phase 6: cleanup actor" >&2
grpcurl -insecure -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $(cat /tmp/ate-bearer.token)" \
  -d "{\"actor_id\":\"$ACTOR_ID\"}" \
  127.0.0.1:18443 ateapi.Control/SuspendActor || true
grpcurl -insecure -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $(cat /tmp/ate-bearer.token)" \
  -d "{\"actor_id\":\"$ACTOR_ID\"}" \
  127.0.0.1:18443 ateapi.Control/DeleteActor || true

echo "" >&2
echo "[run-gw] done. evidence in $EVIDENCE_DIR" >&2
echo "[run-gw] next: inspect supervisor-tail.log + gateway-pod.log; fill in features.md" >&2
