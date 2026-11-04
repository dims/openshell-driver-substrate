#!/usr/bin/env bash
# One sandbox through the stock gateway, start to finish: create to Ready, exec
# and logs, a chat through atenet-router, suspend and resume, stop and start, a
# Substrate-side resume behind a stop, a gateway pod restart behind a stop, a
# Substrate-side suspend and resume with the workload's memory kept, delete.
# Needs gateway-up.sh and examples/helpdesk/build.sh. It touches only its own
# sandbox, alice. The last line printed is GATEWAY_TEST_EXIT=<rc>.
#
#   harness/scripts/gateway-test.sh
#
# Environment: WORK (default $HOME/openshell-substrate), ATESPACE (default
# ate-openshell-microvm), GATEWAY_LOCAL_PORT (default 17670).
set -u -o pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${WORK:-$HOME/openshell-substrate}
export ATESPACE=${ATESPACE:-ate-openshell-microvm}
GW_PORT=${GATEWAY_LOCAL_PORT:-17670}
export PATH=$WORK/bin:$PATH
TMP=$(mktemp -d)
rc=1 PF='' PX='' id=''

step() { printf '\n#### %s %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
fail() { echo "FAILED: $*" >&2; exit 1; }  # never inside $(...): the exit would end only the subshell
os() {  # the CLI against the port-forward; the harness CA is self-signed, so every call warns
  local out st=0
  out=$(openshell --gateway-endpoint "https://127.0.0.1:$GW_PORT" --gateway-insecure "$@" 2>&1) || st=$?
  [ -z "$out" ] || grep -v 'TLS certificate verification' <<<"$out" || true
  return $st
}
getj() {  # the warning lands on stdout too
  openshell --gateway-endpoint "https://127.0.0.1:$GW_PORT" --gateway-insecure sandbox get alice -o json 2>/dev/null |
    grep -v 'TLS certificate verification'
}
phase() { getj | jq -c '{phase, conditions: [.conditions[]? | "\(.type)=\(.status) \(.reason)"]}'; }
wait_phase() {  # phase seconds
  local ph='' i
  for ((i = 0; i < $2; i += 2)); do
    ph=$(getj | jq -r '.phase // empty')
    [ "$ph" = "$1" ] && { echo "$(date -u +%H:%M:%SZ) phase=$ph"; return 0; }
    sleep 2
  done
  echo "$(date -u +%H:%M:%SZ) phase=$ph, wanted $1" >&2; return 1
}
actor_state() { kubectl-ate get actors -a "$ATESPACE" "$id" -o json 2>/dev/null | jq -r '.status.state // empty'; }
wait_actor() {  # state seconds
  local i
  for ((i = 0; i < $2; i++)); do [ "$(actor_state)" = "$1" ] && return 0; sleep 1; done
  echo "actor $id is $(actor_state), wanted $1" >&2; return 1
}
port_forward() {  # open, or reopen after a pod restart
  [ -z "$PF" ] || { kill "$PF" 2>/dev/null; wait "$PF" 2>/dev/null; }
  kubectl -n "$ATESPACE" port-forward svc/openshell-gateway "$GW_PORT:17670" > "$TMP/gateway-pf.log" 2>&1 & PF=$!
  local i
  for ((i = 0; i < 30; i++)); do (exec 3<>"/dev/tcp/127.0.0.1/$GW_PORT") 2>/dev/null && return 0; sleep 1; done
  fail "no port-forward to the gateway"
}
proxy() { kubectl-ate proxy actor -a "$ATESPACE" "$id" 18081:8081 > "$TMP/proxy.log" 2>&1 & PX=$!; sleep 2; }
unproxy() { kill "$PX" 2>/dev/null; wait "$PX" 2>/dev/null; PX=; }
turns() {  # "turns=N uptime=S" from the workload, or a failure
  local out
  out=$(curl -fsS --max-time 60 http://127.0.0.1:18081/status) || return 1
  jq -r '"turns=\(.turns) uptime=\(.uptime_seconds)"' <<<"$out"
}
chat() {  # the workload must answer with a reply, not its error JSON
  local out
  out=$(curl -fsS --max-time 90 -X POST -H 'Content-Type: application/json' -d "{\"message\":\"$1\"}" http://127.0.0.1:18081/chat) || return 1
  jq -e '.reply' <<<"$out" >/dev/null || { cut -c1-200 <<<"$out"; return 1; }
  cut -c1-120 <<<"$out"
}
actor_messages() {
  echo "#### actor container messages"
  kubectl logs -n "$ATESPACE" -l ate.dev/worker-pool --tail=-1 2>/dev/null |
    jq -r --arg a "$id" 'select(.labels."ate.actor.name" == $a and (.message | test("launcher|Error|error|exited|attached|accepted|agent started|PROC:|SSH"))) | "\(.time[11:19]) \(.labels."ate.actor.container.name")\t\(.message)"' 2>/dev/null |
    tail -60 | cut -c1-220
}
diagnose() {
  echo "#### diagnostics"; phase; kubectl-ate get actors -a "$ATESPACE" 2>/dev/null
  echo "#### driver log"; kubectl -n "$ATESPACE" logs deploy/openshell-gateway -c driver --tail=30 2>/dev/null
  echo "#### gateway log"; kubectl -n "$ATESPACE" logs deploy/openshell-gateway -c gateway --tail=40 2>/dev/null | sed -e 's/\x1b\[[0-9;]*m//g' | grep -v 'request{' | cut -c1-240
  actor_messages
}
finish() {
  [ "$rc" -eq 0 ] || diagnose
  [ -z "$PX" ] || kill "$PX" 2>/dev/null
  [ -z "$PF" ] || kill "$PF" 2>/dev/null
  rm -rf "$TMP"
  echo "GATEWAY_TEST_EXIT=$rc"
}
trap finish EXIT

for t in kubectl kubectl-ate openshell jq curl; do command -v "$t" >/dev/null || fail "$t is not on PATH"; done
[ -f "$REPO/out/helpdesk.env" ] || fail "no out/helpdesk.env: run examples/helpdesk/build.sh"
set -o allexport
# shellcheck source=/dev/null  # SANDBOX_IMAGE, MODEL_HOST, MODEL_PORT, CHILD_ENV
. "$REPO/out/helpdesk.env"
set +o allexport
MODEL=${CHILD_ENV##*HELPDESK_MODEL=}; MODEL=${MODEL%%,*}
"$REPO/harness/scripts/render.sh" "$REPO/examples/helpdesk/data.yaml.tmpl" > "$TMP/policy.yaml"

step "port-forward"; port_forward
step "cleanup"
os sandbox delete alice >/dev/null 2>&1; sleep 2

step "create"
os sandbox create --name alice --from "$SANDBOX_IMAGE" --policy "$TMP/policy.yaml" --detach \
  --env "OPENAI_BASE_URL=http://$MODEL_HOST:$MODEL_PORT/v1" --env "HELPDESK_MODEL=$MODEL" -- python3 /opt/helpdesk/agent.py || fail create
id=$(getj | jq -r '.id // empty'); [ -n "$id" ] || fail "no sandbox id"
wait_phase Ready 180 || fail "create did not reach Ready"
echo "GATEWAY_SANDBOX_READY $(date -u +%H:%M:%SZ)"

step "exec and logs"
os sandbox exec alice -- cat /etc/resolv.conf || fail exec
os logs alice -n 3 | cut -c1-160

step "the workload through the router"
proxy; turns || fail status
chat "User foo reports their database is timing out. Answer in one line." || fail chat
turns || fail status; unproxy

step "gateway suspend, then resume"
os sandbox suspend alice || fail suspend; wait_phase Suspended 60 || fail "suspend phase"
os sandbox resume alice || fail resume; wait_phase Ready 120 || fail "resume phase"
proxy; turns || fail status; unproxy

step "gateway stop, then start"
os sandbox stop alice || fail stop; wait_phase Stopped 60 || fail "stop phase"
os sandbox start alice || fail start; wait_phase Ready 120 || fail "start phase"
proxy; turns || fail status; chat "Say hi in three words." || fail chat; unproxy

step "a Substrate-side resume behind a stop: the gateway must stay Stopped"
os sandbox stop alice || fail stop; wait_phase Stopped 60 || fail "stop phase"
kubectl-ate resume actor -a "$ATESPACE" "$id" >/dev/null 2>&1 || fail "kubectl-ate resume"
wait_actor ACTOR_STATE_RUNNING 60 || fail "the actor did not resume"
sleep 8; phase
[ "$(getj | jq -r '.phase // empty')" = "Stopped" ] || fail "the gateway left Stopped on a resume it did not ask for"
os sandbox start alice || fail "start after the resume behind the gateway's back"
wait_phase Ready 120 || fail "start phase"
proxy; turns || fail status; chat "Say hi again." || fail chat; unproxy

step "a gateway pod restart behind a stop"
os sandbox stop alice || fail stop; wait_phase Stopped 60 || fail "stop phase"
kubectl -n "$ATESPACE" rollout restart deploy/openshell-gateway >/dev/null || fail "rollout restart"
kubectl -n "$ATESPACE" rollout status deploy/openshell-gateway --timeout=300s >/dev/null || fail "rollout status"
port_forward
wait_phase Stopped 60 || fail "the restarted gateway does not show Stopped"
os sandbox start alice || fail "start after the restart"; wait_phase Ready 180 || fail "start phase"
proxy; chat "One more line, please." || fail chat
before=$(turns) || fail status; unproxy; echo "$before"

step "a Substrate-side suspend and resume: the gateway waits, the memory stays"
kubectl-ate suspend actor -a "$ATESPACE" --mode sync "$id" >/dev/null 2>&1 || fail "kubectl-ate suspend"
sleep 3; phase
kubectl-ate resume actor -a "$ATESPACE" "$id" >/dev/null 2>&1 || fail "kubectl-ate resume"
wait_phase Ready 90 || fail "the gateway did not return to Ready after the Substrate-side resume"
proxy; after=$(turns) || fail status; unproxy; echo "$after"
[ "${before%% *}" = "turns=1" ] && [ "${after%% *}" = "turns=1" ] || fail "memory lost across the Substrate suspend: $before, then $after"

step "delete"
os sandbox delete alice || fail delete; sleep 3
os sandbox list; kubectl-ate get actors -a "$ATESPACE" 2>/dev/null
actor_messages
rc=0
