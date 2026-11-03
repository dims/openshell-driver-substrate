#!/usr/bin/env bash
# Run the ten beats N times in a row and stop at the first failure.
#
#   examples/helpdesk/soak.sh 10
#
# Each run's output goes to SOAK_LOGS (default $HOME/openshell-substrate/logs/soak-<utc time>).
set -o nounset -o pipefail
N=${1:-10}
HERE=$(cd "$(dirname "$0")" && pwd)
LOGS=${SOAK_LOGS:-$HOME/openshell-substrate/logs/soak-$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$LOGS"
for ((i = 1; i <= N; i++)); do
  start=$(date +%s)
  if "$HERE/run.sh" > "$LOGS/run-$i.log" 2>&1; then
    echo "$(date -u +%H:%M:%SZ) run $i: pass in $(( $(date +%s) - start )) s"
  else
    echo "$(date -u +%H:%M:%SZ) run $i: FAIL after $(( $(date +%s) - start )) s; see $LOGS/run-$i.log"
    tail -15 "$LOGS/run-$i.log"
    exit 1
  fi
done
echo "SOAK_PASS $N of $N; logs in $LOGS"
