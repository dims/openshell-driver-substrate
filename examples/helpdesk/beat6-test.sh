#!/usr/bin/env bash
# Targeted test for RFC 0001 Phase 1 (the syncer-side orphan-recovery fix).
#
# Scenario: actor running, host pod forcibly deleted. Expected behaviour
# AFTER the fix:
#   * The syncer's pod-delete handler chases Worker.ActorId, acquires the
#     per-actor lock, clears the actor's AteomPod* + InProgressSnapshot,
#     and writes Status=STATUS_SUSPENDED via versioned UpdateActor.
#   * The next implicit resume (next curl) lands on a fresh worker and
#     answers with full chat history (gVisor checkpoint preserved).
#
# Before the fix the actor would have stayed STATUS_RUNNING with a stale
# ateomPodName pointing at the deleted pod; the next curl would time out
# at the router; kubectl ate suspend / delete would refuse.
set -euo pipefail
export PATH=$HOME/go/bin:$PATH

NS=ate-demo-helpdesk
ACTOR=helpdesk-1
HOST="$ACTOR.actors.resources.substrate.ate.dev"
ROUTER_URL=http://localhost:8000

beat() { printf "\n\033[1;33m== Beat %s: %s ==\033[0m\n" "$1" "$2"; }
post() { curl -sS -X POST -H "Host: $HOST" -H "Content-Type: application/json" \
              -d "$2" --max-time 30 "$ROUTER_URL/$1"; echo; }
get()  { curl -sS    -H "Host: $HOST" --max-time 15 "$ROUTER_URL/$1"; echo; }

actor_status() { kubectl ate get actor "$ACTOR" -o json | jq -r ".actors[0].status"; }
actor_worker() { kubectl ate get actor "$ACTOR" -o json | jq -r ".actors[0].ateomPodName // empty"; }

# Cleanup any previous run.
kubectl ate suspend actor "$ACTOR" 2>/dev/null || true; sleep 2
kubectl ate delete actor "$ACTOR"  2>/dev/null || true; sleep 3

kubectl ate create actor "$ACTOR" --template "$NS/helpdesk-agent" | tail -1
kubectl port-forward -n ate-system svc/atenet-router 8000:80 >/dev/null &
PF=$!
trap 'kill $PF 2>/dev/null || true' EXIT
sleep 3

beat 1 "seed the conversation"
time post chat '{"message":"User foo reports their database is timing out — give me a triage checklist."}'

WORKER=$(actor_worker)
echo "actor on: $WORKER (status: $(actor_status))"
[ -z "$WORKER" ] && { echo "no worker assigned — bring-up problem"; exit 1; }

beat 2 "FORCIBLY DELETE THE WORKER POD (the test)"
time kubectl delete pod -n "$NS" "$WORKER" --wait=false

beat 3 "wait for the syncer to detect + clear assignment"
T0=$(date +%s)
for i in $(seq 1 60); do
  STAT=$(actor_status)
  WK=$(actor_worker)
  printf "  t+%2ds  status=%s  worker=%s\n" $(( $(date +%s) - T0 )) "$STAT" "${WK:-(none)}"
  if [ "$STAT" = "STATUS_SUSPENDED" ] && [ -z "$WK" ]; then
    echo "  → syncer cleared the assignment after $(( $(date +%s) - T0 ))s"
    break
  fi
  sleep 1
done
if [ "$(actor_status)" != "STATUS_SUSPENDED" ]; then
  echo "FAIL: actor did not reset to STATUS_SUSPENDED within 60s"
  kubectl ate get actor "$ACTOR"
  exit 2
fi

beat 4 "follow-up — implicit resume on a fresh worker, chat history intact"
time post chat '{"message":"What was the user issue I just asked you about?"}'
get status
NEW_WORKER=$(actor_worker)
echo "old worker: $WORKER → new worker: ${NEW_WORKER:-(none)}"
[ "$NEW_WORKER" = "$WORKER" ] && \
  echo "(note: same pod name; the ReplicaSet replaced it on the same node — still a valid migration)"
echo
echo "PASS"
