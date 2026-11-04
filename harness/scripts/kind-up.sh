#!/usr/bin/env bash
# Substrate on a kind cluster, ready for the helpdesk demo: the install with
# the micro-VM class, the OpenShell worker pool, the atespace and the model
# endpoint. Steps already done are skipped.
#
#   harness/scripts/kind-up.sh
#
# Environment: WORK (default $HOME/openshell-substrate; the Substrate clone,
# bin/ and logs/ live under it), SUBSTRATE_REF (default upstream/main),
# ATESPACE (default ate-openshell-microvm), MODEL_NAME (default qwen2.5:1.5b),
# MODEL_PORT (default 11434), POST_KIND_HOOK (a script run once the cluster
# exists, for a host firewall that blocks the kind bridge).
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${WORK:-$HOME/openshell-substrate}
SUBSTRATE_REF=${SUBSTRATE_REF:-upstream/main}
export ATESPACE=${ATESPACE:-ate-openshell-microvm}
export MODEL_NAME=${MODEL_NAME:-qwen2.5:1.5b}
export MODEL_PORT=${MODEL_PORT:-11434}
POST_KIND_HOOK=${POST_KIND_HOOK:+$(realpath "$POST_KIND_HOOK")}
INGRESS_CLIENT_ID=spiffe://cluster.local/ns/ate-system/sa/atenet-client
POOL=openshell-microvm
SUBSTRATE=$WORK/substrate
LOGS=$WORK/logs
mkdir -p "$WORK/bin" "$LOGS"
export PATH=$WORK/bin:$PATH
export GOFLAGS=-buildvcs=false

step() { printf '\n== %s %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
fail() { echo "FAILED: $*" >&2; exit 1; }
wait_for() {  # seconds what command...
  local deadline=$(( $(date +%s) + $1 )); local what=$2; shift 2
  until "$@"; do [ "$(date +%s)" -lt "$deadline" ] || fail "$what did not happen within the wait"; sleep 5; done
}

step "host"
[ -e /dev/kvm ] || fail "no /dev/kvm: the micro-VM class needs KVM"
for t in docker kind kubectl go jq curl envsubst; do command -v "$t" >/dev/null || fail "$t is not on PATH"; done
echo "arch $(uname -m), page size $(getconf PAGESIZE), $(nproc) cpus, $(free -g | awk '/^Mem/ {print $2}') GiB"

step "substrate at $SUBSTRATE_REF"
if [ ! -d "$SUBSTRATE/.git" ]; then
  git clone -q https://github.com/agent-substrate/substrate.git "$SUBSTRATE"
  git -C "$SUBSTRATE" remote rename origin upstream
fi
git -C "$SUBSTRATE" fetch -q upstream
git -C "$SUBSTRATE" checkout -q --detach "$SUBSTRATE_REF"
REV=$(git -C "$SUBSTRATE" rev-parse --short HEAD)
echo "substrate $(git -C "$SUBSTRATE" log -1 --format='%h %s')"
(cd "$SUBSTRATE" && go build -o "$WORK/bin/kubectl-ate" ./cmd/kubectl-ate)
cd "$SUBSTRATE"

step "kind cluster"
if kind get clusters 2>/dev/null | grep -qx kind; then
  echo "cluster exists"
else
  hack/create-kind-cluster.sh > "$LOGS/kind-create.log" 2>&1 || { tail -20 "$LOGS/kind-create.log"; fail "create-kind-cluster"; }
fi
[ -z "$POST_KIND_HOOK" ] || bash "$POST_KIND_HOOK"
docker exec kind-control-plane crictl pull registry.k8s.io/pause:3.10 >/dev/null 2>&1 ||
  fail "the kind node has no egress; let the kind bridge forward (POST_KIND_HOOK)"
docker exec kind-control-plane test -e /dev/kvm || fail "the kind node has no /dev/kvm"
echo "node egress and /dev/kvm ok"

step "ate-system"
if kubectl -n ate-system get deploy ate-api-server >/dev/null 2>&1; then
  installed=$(cat "$WORK/installed-rev" 2>/dev/null || echo unknown)
  echo "present, installed from $installed"
  [ "$installed" = "$REV" ] || echo "this checkout is $REV: kind delete cluster to reinstall"
else
  go run ./cmd/ate-setup --kind --credential-provider='{"name":"k8s.io"}' deploy ate-system \
    --sandbox-classes=microvm --ingress-allowed-spiffe-ids="$INGRESS_CLIENT_ID" > "$LOGS/install.log" 2>&1 ||
    { tail -30 "$LOGS/install.log"; fail "ate-setup deploy ate-system"; }
  echo "$REV" > "$WORK/installed-rev"
fi
kubectl -n ate-system wait --for=condition=Available deploy --all --timeout=600s >/dev/null
settled() {
  local pods; pods=$(kubectl -n ate-system get pods --no-headers) || return 1
  [ -z "$(awk '$3 != "Running" && $3 != "Completed"' <<<"$pods")" ]
}
wait_for 300 "ate-system pods" settled
kubectl get sandboxconfig microvm >/dev/null || fail "no SandboxConfig microvm"
echo "ate-system up; microvm SandboxConfig version $(kubectl get sandboxconfig microvm -o jsonpath='{.spec.defaultVersion}')"

step "worker pool and atespace"
if kubectl -n "$ATESPACE" get workerpool "$POOL" >/dev/null 2>&1; then
  echo "pool exists"
else
  go run ./cmd/ate-setup --kind publish worker-images > "$LOGS/worker-images.out" 2> "$LOGS/worker-images.err" ||
    { cat "$LOGS/worker-images.err"; fail "publish worker-images"; }
  ATEWORKER_MICROVM_IMAGE=$(grep -oE -m1 '[^ ]*ateworker-microvm[^ ]*@sha256:[0-9a-f]{64}' "$LOGS/worker-images.out") ||
    { cat "$LOGS/worker-images.out"; fail "no ateworker-microvm ref in the publish output"; }
  export ATEWORKER_MICROVM_IMAGE
  "$REPO/harness/scripts/render.sh" "$REPO/harness/manifests/openshell-microvm-pool.yaml.tmpl" | kubectl apply -f - >/dev/null
fi
kubectl-ate get atespace "$ATESPACE" >/dev/null 2>&1 || kubectl-ate create atespace "$ATESPACE" >/dev/null
active() { kubectl-ate get workers 2>/dev/null | awk -v pool="$POOL" '$2 == pool && $3 == "WORKER_STATE_ACTIVE"' | wc -l; }
workers_active() { [ "$(active)" -ge 2 ]; }
wait_for 600 "two active workers" workers_active
echo "workers: $(active) active"

step "model endpoint"
"$REPO/harness/scripts/render.sh" "$REPO/examples/helpdesk/ollama.yaml.tmpl" | kubectl apply -f - >/dev/null
kubectl -n "$ATESPACE" rollout status deploy/ollama --timeout=900s >/dev/null
echo "ollama.$ATESPACE.svc.cluster.local:$MODEL_PORT serves $MODEL_NAME"

step "done"
