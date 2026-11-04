#!/usr/bin/env bash
# A stock OpenShell gateway and this driver as one pod in the harness cluster,
# the Services the actors dial, the access policy the driver needs, and the
# openshell CLI on this host. Needs a cluster from kind-up.sh and the runtime
# image from runtime/build.sh. Rerun after either changes.
#
#   harness/scripts/gateway-up.sh
#
# Environment: WORK (default $HOME/openshell-substrate), REGISTRY (default
# localhost:5001), ATESPACE (default ate-openshell-microvm), OPENSHELL_VERSION
# (default 0.1.7), MODEL_HOST and MODEL_PORT (default the Ollama Service
# examples/helpdesk/model-up.sh deploys in the atespace, 11434): the one host
# the egress rules let a workload reach besides the gateway and the driver.
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${WORK:-$HOME/openshell-substrate}
DIR=$WORK/gateway
REGISTRY=${REGISTRY:-localhost:5001}
export ATESPACE=${ATESPACE:-ate-openshell-microvm}
OPENSHELL_VERSION=${OPENSHELL_VERSION:-0.1.7}
GATEWAY_IMAGE_REF=ghcr.io/nvidia/openshell/gateway:${OPENSHELL_VERSION}
export GATEWAY_PORT=17670 GATEWAY_HEALTH_PORT=17671 LAUNCH_PORT=17790 LAUNCH_AUDIENCE=openshell-driver-substrate
export GATEWAY_HOST=openshell-gateway.${ATESPACE}.svc.cluster.local LAUNCH_HOST=openshell-driver.${ATESPACE}.svc.cluster.local
export MODEL_HOST=${MODEL_HOST:-ollama.${ATESPACE}.svc.cluster.local} MODEL_PORT=${MODEL_PORT:-11434}
mkdir -p "$DIR/pki" "$WORK/bin"
export PATH=$WORK/bin:$PATH
render() { "$REPO/harness/scripts/render.sh" "$1"; }

step() { printf '\n== %s %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
fail() { echo "FAILED: $*" >&2; exit 1; }

step "tools"
for t in docker kubectl kubectl-ate cargo go openssl curl python3; do command -v "$t" >/dev/null || fail "$t is not on PATH"; done
[ -f "$REPO/out/runtime.env" ] || fail "no out/runtime.env: run runtime/build.sh"
set -o allexport
# shellcheck source=/dev/null
. "$REPO/out/runtime.env"
set +o allexport
CRANE=$REPO/out/bin/crane
[ -x "$CRANE" ] || GOBIN=$REPO/out/bin go install github.com/google/go-containerregistry/cmd/crane@v0.22.1

step "openshell CLI"
if ! command -v openshell >/dev/null; then
  arch=$(uname -m | sed -e 's/arm64/aarch64/')
  base=https://github.com/NVIDIA/OpenShell/releases/download/v${OPENSHELL_VERSION}
  tgz=openshell-${arch}-unknown-linux-musl.tar.gz
  curl -fsSL -o "$DIR/$tgz" "$base/$tgz"
  curl -fsSL "$base/openshell-checksums-sha256.txt" | grep " $tgz\$" | sed "s| .*| $DIR/$tgz|" | sha256sum -c --quiet -
  tar -xzf "$DIR/$tgz" -C "$WORK/bin" --no-same-owner
  find "$WORK/bin" -mindepth 2 -type f -name openshell -exec mv -f {} "$WORK/bin/openshell" \;
fi
openshell --version

step "driver image"
cargo build -q --release --manifest-path "$REPO/Cargo.toml" -p openshell-driver-substrate
mkdir -p "$DIR/image"
cp "$REPO/target/release/openshell-driver-substrate" "$DIR/image/"
cp "$REPO/harness/gateway/driver.Dockerfile" "$DIR/image/Dockerfile"
docker build -q --provenance=false --sbom=false -t "$REGISTRY/openshell-driver-substrate:dev" "$DIR/image" >/dev/null
docker push -q "$REGISTRY/openshell-driver-substrate:dev" >/dev/null
DRIVER_IMAGE=$REGISTRY/openshell-driver-substrate:dev@$("$CRANE" digest "$REGISTRY/openshell-driver-substrate:dev")
GATEWAY_IMAGE=$GATEWAY_IMAGE_REF@$("$CRANE" digest "$GATEWAY_IMAGE_REF")
export DRIVER_IMAGE GATEWAY_IMAGE
echo "$DRIVER_IMAGE"
echo "$GATEWAY_IMAGE"

step "gateway TLS and JWT keys"
if [ ! -f "$DIR/pki/tls.key" ]; then
  (
    cd "$DIR/pki"
    umask 077
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 365 -subj "/CN=openshell harness CA" \
      -addext "basicConstraints=critical,CA:TRUE" -keyout ca.key -out ca.crt 2>/dev/null
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=$GATEWAY_HOST" -keyout tls.key -out tls.csr 2>/dev/null
    printf 'subjectAltName=DNS:%s,DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' "$GATEWAY_HOST" > san.cnf
    openssl x509 -req -in tls.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 365 -extfile san.cnf -out tls.crt 2>/dev/null
    openssl genpkey -algorithm ed25519 -out signing.pem
    openssl pkey -in signing.pem -pubout -out public.pem
    printf 'kind-1' > kid
  )
fi
kubectl label namespace "$ATESPACE" ate.dev/api-access=true --overwrite >/dev/null
kubectl -n "$ATESPACE" create secret tls openshell-gateway-tls --cert="$DIR/pki/tls.crt" --key="$DIR/pki/tls.key" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl -n "$ATESPACE" create secret generic openshell-gateway-jwt --from-file=signing.pem="$DIR/pki/signing.pem" --from-file=public.pem="$DIR/pki/public.pem" --from-file=kid="$DIR/pki/kid" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl -n "$ATESPACE" create configmap openshell-gateway-ca --from-file=ca.crt="$DIR/pki/ca.crt" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl get clustertrustbundle servicedns.podcert.ate.dev:identity:primary-bundle -o jsonpath='{.spec.trustBundle}' > "$DIR/ctb.crt"
kubectl -n "$ATESPACE" create configmap ate-ca --from-file=ctb.crt="$DIR/ctb.crt" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

step "configuration"
render "$REPO/harness/gateway/gateway.toml.tmpl" > "$DIR/gateway.toml"
render "$REPO/harness/gateway/driver.toml.tmpl" > "$DIR/driver.toml"
render "$REPO/harness/gateway/template.yaml.tmpl" > "$DIR/template.yaml"
render "$REPO/harness/gateway/egress-policy.yaml.tmpl" > "$DIR/egress-policy.yaml"
kubectl -n "$ATESPACE" create configmap openshell-gateway-config --from-file=gateway.toml="$DIR/gateway.toml" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl -n "$ATESPACE" create configmap openshell-driver-config --from-file=driver.toml="$DIR/driver.toml" --from-file=template.yaml="$DIR/template.yaml" --from-file=egress-policy.yaml="$DIR/egress-policy.yaml" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

step "access policy"
MEMBER=user:system:serviceaccount:${ATESPACE}:openshell-driver
current=$(kubectl-ate get access-policy -a "$ATESPACE" -o json 2>/dev/null)  # exits 0 with a sentence when there is none
if [ "${current:0:1}" = "{" ]; then
  printf '%s' "$current" > "$DIR/access-policy.current.json"
  # An update replaces the policy and needs its uid and version; keep what is there.
  python3 - "$DIR/access-policy.current.json" "$MEMBER" > "$DIR/access-policy.json" <<'MERGE'
import json, sys
policy, member = json.load(open(sys.argv[1])), sys.argv[2]
bindings = policy.setdefault("bindings", [])
editor = next((b for b in bindings if b.get("role") == "editor"), None)
if editor is None:
    editor = {"role": "editor", "members": []}
    bindings.append(editor)
if member not in editor.setdefault("members", []):
    editor["members"].append(member)
json.dump(policy, sys.stdout)
MERGE
  kubectl-ate update access-policy -a "$ATESPACE" -f "$DIR/access-policy.json" >/dev/null
else
  render "$REPO/harness/gateway/access-policy.yaml.tmpl" > "$DIR/access-policy.yaml"
  kubectl-ate create access-policy -a "$ATESPACE" -f "$DIR/access-policy.yaml" >/dev/null
fi

step "pod"
render "$REPO/harness/gateway/deployment.yaml.tmpl" | kubectl apply -f - >/dev/null
kubectl -n "$ATESPACE" rollout restart deploy/openshell-gateway >/dev/null
kubectl -n "$ATESPACE" rollout status deploy/openshell-gateway --timeout=300s >/dev/null ||
  { kubectl -n "$ATESPACE" logs deploy/openshell-gateway --all-containers --tail=40; fail "the gateway pod did not come up"; }
echo "gateway https://$GATEWAY_HOST:$GATEWAY_PORT, launch endpoint http://$LAUNCH_HOST:$LAUNCH_PORT"
echo "CLI: kubectl -n $ATESPACE port-forward svc/openshell-gateway $GATEWAY_PORT:$GATEWAY_PORT, then"
echo "     openshell --gateway-endpoint https://127.0.0.1:$GATEWAY_PORT --gateway-insecure sandbox list"
