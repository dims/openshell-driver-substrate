#!/usr/bin/env bash
# Cleanup for the agent-substrate-demo. Suspends and deletes every actor
# under the configured ActorTemplate but preserves the golden actor and
# snapshot so the next run can resume without re-creating the template
# (the golden-snapshot warm-up is ~70 s).
#
# Usage:
#   bash cleanup.sh            # delete only non-golden actors
#   bash cleanup.sh --hard     # also delete ActorTemplate + WorkerPool
set -euo pipefail
export PATH="${PATH}:${HOME}/go/bin"

NS="${NS:-ate-openshell-m0}"
TEMPLATE="${TEMPLATE:-supervisor}"
GOLDEN_ID=$(kubectl -n "$NS" get actortemplate "$TEMPLATE" \
  -o jsonpath='{.status.goldenActorID}' 2>/dev/null || true)

echo "namespace:   $NS"
echo "template:    $TEMPLATE"
echo "golden:      ${GOLDEN_ID:-<none>}"
echo

mapfile -t ACTORS < <(
  kubectl ate get actors 2>/dev/null \
    | awk -v ns="$NS" -v t="$TEMPLATE" '$1==ns && $2==t {print $3}'
)

for a in "${ACTORS[@]}"; do
  [ "$a" = "$GOLDEN_ID" ] && { echo "keep   $a (golden)"; continue; }
  status=$(kubectl ate get actors 2>/dev/null | awk -v a="$a" '$3==a {print $4}')
  if [ "$status" != "STATUS_SUSPENDED" ]; then
    echo "suspend $a (was: $status)"
    if ! kubectl ate suspend actor "$a" >/dev/null 2>&1; then
      # Stuck actors (e.g. `runsc checkpoint` fails because the sandbox
      # never fully booted) are harmless apart from clutter. Skip rather
      # than block; see the demo README.
      echo "skip    $a (suspend refused; leave in place)"
      continue
    fi
    sleep 2
  fi
  echo "delete  $a"
  kubectl ate delete actor "$a" >/dev/null 2>&1 || echo "skip    $a (delete refused; leave in place)"
done

if [ "${1:-}" = "--hard" ]; then
  echo
  echo "hard mode: deleting ActorTemplate + WorkerPool too"
  kubectl -n "$NS" delete actortemplate "$TEMPLATE" --ignore-not-found
  kubectl -n "$NS" delete workerpool openshell-m0-pool --ignore-not-found
fi

echo
echo "remaining actors in $NS:"
kubectl ate get actors 2>/dev/null | awk -v ns="$NS" 'NR==1 || $1==ns'
