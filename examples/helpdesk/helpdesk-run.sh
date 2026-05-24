#!/usr/bin/env bash
# 6-beat helpdesk demo driver. Run from bigbox where the kind cluster lives.
set -euo pipefail

export PATH="$HOME/go/bin:$PATH"

NS=ate-demo-helpdesk
ACTOR=helpdesk-1
ROUTER_URL=http://localhost:8000
HOST_HDR="$ACTOR.actors.resources.substrate.ate.dev"

beat() { printf "\n\033[1;33m=== Beat %s: %s ===\033[0m\n" "$1" "$2"; }
post() { curl -sS -X POST -H "Host: $HOST_HDR" -H "Content-Type: application/json" \
              -d "$2" "$ROUTER_URL/$1"; echo; }
get()  { curl -sS    -H "Host: $HOST_HDR" "$ROUTER_URL/$1"; echo; }

actor_status() {
  kubectl ate get actor "$ACTOR" -o json | jq -r '.actors[0].status'
}
actor_worker() {
  kubectl ate get actor "$ACTOR" -o json | jq -r '.actors[0].ateomPodName // empty'
}

wait_status() {
  local want="$1"
  for _ in $(seq 1 30); do
    [ "$(actor_status)" = "$want" ] && return 0
    sleep 1
  done
  echo "timeout waiting for $want (current: $(actor_status))" >&2
  exit 1
}

# Port-forward the router so curl from the host hits it
kubectl port-forward -n ate-system svc/atenet-router 8000:80 >/dev/null &
FWD_PID=$!
trap 'kill $FWD_PID 2>/dev/null || true
      kubectl ate delete actor "$ACTOR" 2>/dev/null || true' EXIT
sleep 3

# Create the actor (idempotent — ignore "already exists")
kubectl ate create actor "$ACTOR" --template "$NS/helpdesk-agent" 2>/dev/null || true

beat 1 "Cold ask"
time post chat '{"message":"User foo reports their database is timing out — give me a triage checklist."}'

beat 2 "Suspend"
time kubectl ate suspend actor "$ACTOR"
wait_status STATUS_SUSPENDED
echo "status=$(actor_status); worker=$(actor_worker || echo '-none-')"

beat 3 "20-second idle (capacity recovered)"
echo "--- workers (one should be free) ---"
kubectl ate get workers
sleep 20
echo "--- still suspended; workers still free ---"
echo "status=$(actor_status)"
kubectl ate get workers

beat 4 "Follow-up (implicit resume)"
time post chat '{"message":"What was the user issue I just asked you about?"}'
get status

WORKER=$(actor_worker)
echo "current worker: $WORKER"

beat 5 "Exfil attempt (expect blocked + OCSF Denied event)"
echo "--- agent view: /probe attempt ---"
post probe '{}'
echo "--- supervisor view: CONNECT lines from OCSF stderr ---"
kubectl logs -c supervisor "$WORKER" --tail=50 2>/dev/null | grep -E "CONNECT" || \
  kubectl logs "$WORKER" --tail=50 2>/dev/null | grep -E "CONNECT" || \
  echo "(no CONNECT lines yet — re-issue if needed)"

beat 6 "Migrate (kill the worker, ask again, land on a new worker)"
kubectl delete pod -n "$NS" "$WORKER" --wait=false
sleep 5
time post chat '{"message":"Confirm you still remember the user issue."}'
NEW_WORKER=$(actor_worker)
echo "old worker: $WORKER → new worker: $NEW_WORKER"
get status
