#!/usr/bin/env bash
# 6-beat gpu-counter demo. Provisions through openshell-gateway ->
# openshell-driver-substrate -> ate-api-server (same path as helpdesk),
# then proves a 1 MiB on-device CUDA buffer survives substrate's
# suspend/resume cycle.
#
# Env: SUPERVISOR_IMAGE=<gpu-counter image @sha256:...>
# Tools: kubectl, kubectl-ate, kubectl-osh, jq, curl
# Host pre-flight on each kind node: see README.md.
set -euo pipefail
export PATH="$HOME/go/bin:$PATH"

NS=ate-demo-gpu-counter
NS_GW=ate-openshell-m0
SUPERVISOR_IMAGE="${SUPERVISOR_IMAGE:?set SUPERVISOR_IMAGE=<image>@sha256:<digest>}"
ROUTER_URL=http://localhost:8000
export OPENSHELL_GATEWAY=localhost:50051
TPL=gpu-counter

beat() { printf "\n\033[1;33m=== Beat %s: %s ===\033[0m\n" "$1" "$2"; }
req()  { curl -sS -X "$2" -H "Host: $1.actors.resources.substrate.ate.dev" "$ROUTER_URL$3"; echo; }

actor_status() { kubectl ate get actor "$1" -o json | jq -r '.actors[0].status'; }

kubectl port-forward -n ate-system svc/atenet-router 8000:80 >/dev/null 2>&1 &
PF1=$!
kubectl port-forward -n "$NS_GW" svc/openshell-gateway-substrate 50051:50051 >/dev/null 2>&1 &
PF2=$!
trap 'kubectl osh delete sandbox gpu1 --ignore-not-found 2>/dev/null||true; kill $PF1 $PF2 2>/dev/null||true' EXIT
sleep 3

kubectl osh delete sandbox gpu1 --ignore-not-found 2>/dev/null || true

beat 1 "Provision via OpenShell.CreateSandbox"
GPU1=$(kubectl osh create sandbox gpu1 --image="$SUPERVISOR_IMAGE" --template="$TPL" -o json | jq -r '.metadata.id')
echo "gpu1 id=$GPU1"
sleep 5

beat 2 "/info + /sum (boot sentinel = 0x42)"
req "$GPU1" GET /info
req "$GPU1" GET /sum

beat 3 "POST /set?val=99 -> /sum reports sample=99"
req "$GPU1" POST "/set?val=99"
req "$GPU1" GET /sum

sleep 15
beat 4 "Suspend (runsc checkpoint + cuda-checkpoint --toggle)"
kubectl ate suspend actor "$GPU1"
for _ in $(seq 1 30); do [ "$(actor_status "$GPU1")" = "STATUS_SUSPENDED" ] && break; sleep 1; done
echo "status=$(actor_status "$GPU1")"

beat 5 "Resume by requesting /sum — sample MUST still be 99"
req "$GPU1" GET /sum

beat 6 "Delete"
kubectl osh delete sandbox gpu1
kubectl get actortemplate -n "$NS" "$TPL" -o jsonpath='{.metadata.name}{"\n"}'
