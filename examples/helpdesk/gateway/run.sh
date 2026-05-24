#!/usr/bin/env bash
# Deploy the substrate-driven OpenShell gateway and verify the
# substrate compute driver is actually on the runtime path.
#
# Steps:
#   1. Apply RBAC + ConfigMap.
#   2. Build the image (or trust an existing digest).
#   3. Render __GATEWAY_IMAGE__ in the Deployment and apply.
#   4. Wait for the pod to become Ready.
#   5. Smoke test: grpcurl ComputeDriver/GetCapabilities -> driver_name = "substrate".
#
# Requires the §7b POC's openshell-gateway-jwt Secret to exist in
# ate-openshell-m0 (run tests/integration/gateway/generate-jwt-keys.sh
# once if it doesn't).
set -euo pipefail
export PATH="${PATH}:${HOME}/go/bin"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NS="${NS:-ate-openshell-m0}"
GATEWAY_IMAGE="${GATEWAY_IMAGE:-}"

echo "[run-helpdesk-gw] === 0. preflight ==="
kubectl get ns "$NS" >/dev/null 2>&1 || kubectl create namespace "$NS"
if ! kubectl get secret -n "$NS" openshell-gateway-jwt >/dev/null 2>&1; then
  echo "[run-helpdesk-gw] missing Secret openshell-gateway-jwt in $NS." >&2
  echo "[run-helpdesk-gw] run tests/integration/gateway/generate-jwt-keys.sh once first." >&2
  exit 1
fi
kubectl get ns ate-demo-helpdesk >/dev/null 2>&1 || {
  echo "[run-helpdesk-gw] missing namespace ate-demo-helpdesk. Apply the helpdesk WorkerPool first." >&2
  exit 1
}
kubectl get workerpool -n ate-demo-helpdesk helpdesk-pool >/dev/null 2>&1 || {
  echo "[run-helpdesk-gw] missing WorkerPool helpdesk-pool in ate-demo-helpdesk." >&2
  echo "[run-helpdesk-gw] Apply examples/helpdesk/helpdesk-template.yaml's WorkerPool section first." >&2
  exit 1
}

echo "[run-helpdesk-gw] === 1. RBAC + ConfigMap ==="
kubectl apply -f "$HERE/gateway-rbac.yaml"
kubectl apply -f "$HERE/gateway-config.yaml"

echo "[run-helpdesk-gw] === 2. resolve gateway image ==="
if [ -z "$GATEWAY_IMAGE" ]; then
  echo "[run-helpdesk-gw] building from source (set GATEWAY_IMAGE=<digest> to skip)..."
  GATEWAY_IMAGE="$("$HERE/build-image.sh")"
fi
echo "[run-helpdesk-gw] image: $GATEWAY_IMAGE"

echo "[run-helpdesk-gw] === 3. Deployment + Service ==="
sed -e "s|__GATEWAY_IMAGE__|$GATEWAY_IMAGE|" "$HERE/gateway-deployment.yaml" \
  | kubectl apply -f -

echo "[run-helpdesk-gw] === 4. wait for rollout ==="
kubectl -n "$NS" rollout status deploy/openshell-gateway-substrate --timeout=180s

echo "[run-helpdesk-gw] === 5. smoke test: ComputeDriver/GetCapabilities ==="
# Port-forward the gRPC service to localhost so grpcurl can reach it.
kubectl -n "$NS" port-forward svc/openshell-gateway-substrate 50051:50051 >/dev/null 2>&1 &
PF_PID=$!
trap 'kill "$PF_PID" 2>/dev/null || true' EXIT
sleep 3

if ! command -v grpcurl >/dev/null 2>&1; then
  echo "[run-helpdesk-gw] grpcurl missing; install via: go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest" >&2
  exit 1
fi

CAPS=$(grpcurl -plaintext localhost:50051 \
  openshell.compute.v1.ComputeDriver/GetCapabilities 2>&1)
echo "[run-helpdesk-gw] GetCapabilities response:"
echo "$CAPS" | sed 's/^/    /'

if echo "$CAPS" | grep -q '"driverName": *"substrate"'; then
  echo "[run-helpdesk-gw] OK: driver_name = substrate. Substrate driver is on the runtime path."
else
  echo "[run-helpdesk-gw] FAIL: expected driver_name = substrate; gateway is not routing through the substrate driver." >&2
  exit 1
fi

echo "[run-helpdesk-gw] === done ==="
