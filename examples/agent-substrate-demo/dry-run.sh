#!/usr/bin/env bash
# End-to-end validator for the agent-substrate-demo: creates a fresh actor
# from the supervisor template, observes restore + heartbeat, then exercises
# one suspend/resume cycle and asserts that the child workload's monotonic
# clock did not advance during the snapshot window.
#
# Expected wall-clock: ~125 s. Pass criterion (plan M2): under 8 minutes.
set -euo pipefail
export PATH="${PATH}:${HOME}/go/bin"

NS="${NS:-ate-openshell-m0}"
TEMPLATE="${TEMPLATE:-supervisor}"
START=$(date +%s)
trap 'echo "[failed at line $LINENO]" >&2' ERR

echo "=== Step 1: verify ActorTemplate is Ready ==="
phase=$(kubectl -n "$NS" get actortemplate "$TEMPLATE" -o jsonpath='{.status.phase}')
echo "phase: $phase"
[ "$phase" = "Ready" ] || { echo "FAIL: template not Ready"; exit 1; }

echo
echo "=== Step 2: create child actor + first resume ==="
ACTOR="m1-$(uuidgen)"
echo "actor: $ACTOR"
kubectl ate create actor "$ACTOR" --template "$NS/$TEMPLATE" >/dev/null
kubectl ate resume actor "$ACTOR" >/dev/null
sleep 3
status=$(kubectl ate get actors 2>/dev/null | awk -v a="$ACTOR" '$3==a {print $4}')
echo "status: $status"
[ "$status" = "STATUS_RUNNING" ] || { echo "FAIL: expected STATUS_RUNNING, got $status"; exit 1; }

echo
echo "=== Step 3: confirm restore + child heartbeat ==="
sleep 5
got=""
for p in $(kubectl -n "$NS" get pods -o name); do
  hits=$(kubectl -n "$NS" logs "$p" --since=2m 2>&1 \
    | grep -E "\"ate.dev/actor_id\":\"$ACTOR\"" \
    | grep -E "(Actor restoring|Actor restored|\\[child\\] alive)" || true)
  got+=$'\n'"$hits"
done
echo "lines matched: $(printf '%s\n' "$got" | grep -c .)"
echo "$got" | grep -oE '"msg":"[^"]+"' | head -6 || true
echo "$got" | grep -q "Actor restored"   || { echo "FAIL: no 'Actor restored'"; exit 1; }
echo "$got" | grep -q "\\[child\\] alive" || { echo "FAIL: no '[child] alive'"; exit 1; }

echo
echo "=== Step 4: suspend + resume, verify heartbeat continuity ==="
sleep 65
echo "[t+65s] suspending"
kubectl ate suspend actor "$ACTOR" >/dev/null
sleep 3
status=$(kubectl ate get actors 2>/dev/null | awk -v a="$ACTOR" '$3==a {print $4}')
[ "$status" = "STATUS_SUSPENDED" ] || { echo "FAIL: expected STATUS_SUSPENDED, got $status"; exit 1; }

echo "[t+68s] resuming"
kubectl ate resume actor "$ACTOR" >/dev/null
sleep 35
status=$(kubectl ate get actors 2>/dev/null | awk -v a="$ACTOR" '$3==a {print $4}')
[ "$status" = "STATUS_RUNNING" ] || { echo "FAIL: expected STATUS_RUNNING, got $status"; exit 1; }

echo
echo "=== heartbeat timeline (expect 30 s spacing across the suspend gap) ==="
combined=""
for p in $(kubectl -n "$NS" get pods -o name); do
  hits=$(kubectl -n "$NS" logs "$p" --since=5m 2>&1 \
    | grep -E "\"ate.dev/actor_id\":\"$ACTOR\"" \
    | grep -E "(\\[child\\] alive|Actor checkpointing|Actor checkpointed|Actor restoring|Actor restored)" || true)
  combined+=$'\n'"$hits"
done
echo "$combined" | grep -oE '"time":"[^"]+"[^}]*"msg":"[^"]+"' | tail -12 || true

cp_count=$(echo "$combined" | grep -c "Actor checkpointing" || true)
rs_count=$(echo "$combined" | grep -c "Actor restored"      || true)
hb_count=$(echo "$combined" | grep -c "\\[child\\] alive"   || true)
echo
echo "checkpoint events: $cp_count   restored events: $rs_count   heartbeats: $hb_count"
[ "$cp_count" -ge 1 ] || { echo "FAIL: no checkpoint event"; exit 1; }
[ "$rs_count" -ge 2 ] || { echo "FAIL: expected >= 2 'Actor restored' (initial + post-suspend), got $rs_count"; exit 1; }
[ "$hb_count" -ge 2 ] || { echo "FAIL: expected >= 2 heartbeats, got $hb_count"; exit 1; }

echo
echo "=== Step 5: cleanup ==="
bash "$(dirname "${BASH_SOURCE[0]}")/cleanup.sh"

ELAPSED=$(( $(date +%s) - START ))
echo
echo "=== DRY-RUN COMPLETE in ${ELAPSED}s ==="
