#!/usr/bin/env bash
# Deploy the OpenShell gateway into the bigbox kind cluster for the v3 POC.
# Builds the image (or accepts a pre-built $GATEWAY_IMAGE), applies the three
# manifest files, and waits for the Deployment to roll out.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAMESPACE="${NAMESPACE:-ate-openshell-m0}"

if [ -z "${GATEWAY_IMAGE:-}" ]; then
  echo "[run-gateway] GATEWAY_IMAGE not set; building via build-gateway-image.sh" >&2
  GATEWAY_IMAGE="$("$HERE/build-gateway-image.sh")"
fi
echo "[run-gateway] image: $GATEWAY_IMAGE" >&2

kubectl get namespace "$NAMESPACE" >/dev/null 2>&1 || {
  echo "[run-gateway] namespace $NAMESPACE missing; apply cluster-setup.yaml first" >&2
  exit 1
}

echo "[run-gateway] applying secret + config + deployment" >&2
# The Secret is rendered from gateway-secret.yaml.template at apply time
# (generate-jwt-keys.sh mints or reuses the Ed25519 keys under $JWT_DIR
# and writes the resulting manifest to stdout). The signing key never
# touches the repo.
"$HERE/generate-jwt-keys.sh" | kubectl apply -f -
kubectl apply -f "$HERE/gateway-config.yaml"
sed "s|__GATEWAY_IMAGE__|$GATEWAY_IMAGE|g" "$HERE/gateway-deployment.yaml" \
  | kubectl apply -f -

echo "[run-gateway] waiting for rollout (180s deadline)" >&2
kubectl -n "$NAMESPACE" rollout status deployment/openshell-gateway --timeout=180s

echo "[run-gateway] gateway state:" >&2
kubectl -n "$NAMESPACE" get pods,svc -l app=openshell-gateway
