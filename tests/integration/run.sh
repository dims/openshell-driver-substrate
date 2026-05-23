#!/usr/bin/env bash
# End-to-end feature observation: build image, apply template, spawn
# actor, watch its log markers, then clean up. Findings are extracted
# by grepping `[oshl-test]` lines out of the worker pod's stdout.
set -euo pipefail

# grpcurl is a hard requirement for the actor lifecycle calls.
if ! command -v grpcurl >/dev/null 2>&1; then
  if [ -x "$HOME/go/bin/grpcurl" ]; then
    PATH="$PATH:$HOME/go/bin"
  else
    echo "[run] grpcurl not found; install with:" >&2
    echo "      go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest" >&2
    exit 1
  fi
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAMESPACE="${NAMESPACE:-ate-openshell-m0}"
TEMPLATE_NAME="${TEMPLATE_NAME:-oshl-feature-test}"

ACTOR_ID="${ACTOR_ID:-oshl-feature-$(date +%s)}"

echo "[run] building + pushing image"
IMAGE_DIGEST=$("$HERE/build-image.sh")
echo "[run]   digest: $IMAGE_DIGEST"

# Bootstrap the ate-openshell-m0 namespace + WorkerPool + the basic
# `supervisor` template the FIRST time after a fresh ate-system install.
# Subsequent runs skip this (WorkerPool's ateomImage is an op-set value
# we don't want to overwrite).
#
# Operators must export `ATEOM_IMAGE=localhost:5001/ateom-gvisor@sha256:...`
# before the first run; the value is the digest produced by:
#     KO_DOCKER_REPO=localhost:5001 ko publish ./cmd/servers/ateom-gvisor
# from the substrate repo. After the bootstrap the value is captured in
# the live `WorkerPool` spec and re-runs do not need it.
if ! kubectl get namespace "$NAMESPACE" >/dev/null 2>&1; then
  if [ -z "${ATEOM_IMAGE:-}" ]; then
    echo "[run] namespace $NAMESPACE missing and ATEOM_IMAGE not set" >&2
    echo "[run] export ATEOM_IMAGE='localhost:5001/ateom-gvisor@sha256:...' from a prior ko publish" >&2
    exit 1
  fi
  echo "[run] bootstrap: namespace $NAMESPACE missing; applying cluster-setup.yaml"
  echo "[run]   ateom image: $ATEOM_IMAGE"
  sed -e "s|__ATEOM_IMAGE__|$ATEOM_IMAGE|g" \
      -e "s|__SUPERVISOR_IMAGE__|$IMAGE_DIGEST|g" \
      "$HERE/cluster-setup.yaml" \
    | kubectl apply -f -
else
  echo "[run] namespace $NAMESPACE present; skipping cluster setup"
fi

echo "[run] applying feature-test ActorTemplate"
sed "s|__IMAGE__|$IMAGE_DIGEST|g" "$HERE/actor-template.yaml" \
  | kubectl apply -f -

echo "[run] waiting for template Ready"
for _ in $(seq 1 90); do
  phase=$(kubectl -n "$NAMESPACE" get actortemplate "$TEMPLATE_NAME" \
            -o jsonpath='{.status.phase}' 2>/dev/null || true)
  [ "$phase" = "Ready" ] && break
  sleep 2
done
[ "$phase" = "Ready" ] || { echo "[run] template did not reach Ready (last: $phase)"; exit 1; }

echo "[run] minting JWT + extracting CA + refreshing port-forward"
kubectl -n ate-system create token ate-controller --audience=api.ate-system.svc > /tmp/ate-bearer.token
kubectl get clustertrustbundle servicedns.podcert.ate.dev:identity:primary-bundle \
  -o jsonpath='{.spec.trustBundle}' > /tmp/ate-servicedns-ca.pem
[ -f /tmp/pf.pid ] && kill "$(cat /tmp/pf.pid)" 2>/dev/null || true
sleep 1
API_POD=$(kubectl -n ate-system get pods -o name | grep ate-api-server | head -1 | sed s,^pod/,,)
nohup kubectl -n ate-system port-forward "$API_POD" 18443:443 >/tmp/pf.log 2>&1 &
echo $! > /tmp/pf.pid
sleep 5

echo "[run] spawning actor $ACTOR_ID via grpcurl"
grpcurl -insecure \
  -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $(cat /tmp/ate-bearer.token)" \
  -d "{\"actor_id\":\"$ACTOR_ID\",\"actor_template_namespace\":\"$NAMESPACE\",\"actor_template_name\":\"$TEMPLATE_NAME\"}" \
  127.0.0.1:18443 ateapi.Control/CreateActor

grpcurl -insecure \
  -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $(cat /tmp/ate-bearer.token)" \
  -d "{\"actor_id\":\"$ACTOR_ID\",\"boot\":false}" \
  127.0.0.1:18443 ateapi.Control/ResumeActor

echo "[run] waiting for actor to reach Running + run probes"
sleep 25

echo "[run] === findings ==="
# Each template build produces a fresh golden actor; the workload runs
# inside that golden, so its log lines may live on a different worker
# than the one currently hosting the named actor we just resumed.
# Search every openshell-m0-pool pod in the namespace.
for pod in $(kubectl -n "$NAMESPACE" get pods -o name | grep openshell-m0-pool); do
  hits=$(kubectl -n "$NAMESPACE" logs "$pod" --tail=10000 2>&1 \
         | grep -c "\[oshl-test")
  [ "$hits" = 0 ] && continue
  echo "[run]   pod $pod has $hits findings"
  kubectl -n "$NAMESPACE" logs "$pod" --tail=10000 2>&1 \
    | grep "\[oshl-test" \
    | sed 's/.*"msg":"//;s/","labels.*$//' \
    | tail -80
done

echo "[run] === cleanup ==="
grpcurl -insecure \
  -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $(cat /tmp/ate-bearer.token)" \
  -d "{\"actor_id\":\"$ACTOR_ID\"}" \
  127.0.0.1:18443 ateapi.Control/SuspendActor || true
grpcurl -insecure \
  -authority api.ate-system.svc \
  -cacert /tmp/ate-servicedns-ca.pem \
  -rpc-header "authorization: Bearer $(cat /tmp/ate-bearer.token)" \
  -d "{\"actor_id\":\"$ACTOR_ID\"}" \
  127.0.0.1:18443 ateapi.Control/DeleteActor || true

kubectl -n "$NAMESPACE" delete actortemplate "$TEMPLATE_NAME" --ignore-not-found
