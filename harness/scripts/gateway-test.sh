#!/usr/bin/env bash
# Two sandboxes through the stock gateway, start to finish. alice: create to
# Ready, exec and logs, the agent's egress allow-list, a chat, suspend and
# resume, stop and start, a Substrate-side resume behind a stop, a gateway pod
# restart behind a stop, a Substrate-side suspend and resume with the
# workload's memory kept, her worker's death (Error, then stop and start),
# delete. bob: created after her from the same golden snapshot and untouched
# until the end. The agent is reached with `openshell sandbox exec`. Needs
# gateway-up.sh, examples/helpdesk/build.sh and two free workers. With
# WITH_MODEL=1 the chats go to the model endpoint (examples/helpdesk/model-up.sh)
# and the egress check expects it reachable too; without it the agent answers
# on its own. The last line printed is GATEWAY_TEST_EXIT=<rc>.
#
#   harness/scripts/gateway-test.sh
#
# Environment: WORK (default $HOME/openshell-substrate), ATESPACE (default
# ate-openshell-microvm), GATEWAY_LOCAL_PORT (default 17670), WITH_MODEL,
# MODEL_HOST, MODEL_PORT and MODEL_NAME (the defaults of model-up.sh).
set -u -o pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${WORK:-$HOME/openshell-substrate}
export ATESPACE=${ATESPACE:-ate-openshell-microvm}
GW_PORT=${GATEWAY_LOCAL_PORT:-17670}
WITH_MODEL=${WITH_MODEL:-}
export MODEL_HOST=${MODEL_HOST:-ollama.${ATESPACE}.svc.cluster.local} MODEL_PORT=${MODEL_PORT:-11434}
MODEL_NAME=${MODEL_NAME:-qwen2.5:1.5b}
export PATH=$WORK/bin:$PATH
TMP=$(mktemp -d)
rc=1 PF=''
declare -A ID=()

step() { printf '\n#### %s %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
fail() { echo "FAILED: $*" >&2; exit 1; }  # never inside $(...): the exit would end only the subshell
os() {  # the CLI against the port-forward; the harness CA is self-signed, so every call warns
  local out st=0
  out=$(openshell --gateway-endpoint "https://127.0.0.1:$GW_PORT" --gateway-insecure "$@" 2>&1) || st=$?
  [ -z "$out" ] || grep -v 'TLS certificate verification' <<<"$out" || true
  return $st
}
getj() {  # name: the sandbox as JSON; the warning lands on stdout too
  openshell --gateway-endpoint "https://127.0.0.1:$GW_PORT" --gateway-insecure sandbox get "$1" -o json 2>/dev/null |
    grep -v 'TLS certificate verification'
}
phase() { getj "$1" | jq -c '{phase, conditions: [.conditions[]? | "\(.type)=\(.status) \(.reason)"]}'; }
wait_phase() {  # name phase seconds
  local ph='' i
  for ((i = 0; i < $3; i += 2)); do
    ph=$(getj "$1" | jq -r '.phase // empty')
    [ "$ph" = "$2" ] && { echo "$(date -u +%H:%M:%SZ) $1 phase=$ph"; return 0; }
    sleep 2
  done
  echo "$(date -u +%H:%M:%SZ) $1 phase=$ph, wanted $2" >&2; return 1
}
actor() { kubectl-ate get actors -a "$ATESPACE" "${ID[$1]}" -o json 2>/dev/null; }
actor_state() { actor "$1" | jq -r '.status.state // empty'; }
worker_pod() { actor "$1" | jq -r '.status.workerAssignment.workerPod // empty'; }
wait_actor() {  # name state seconds
  local i
  for ((i = 0; i < $3; i++)); do [ "$(actor_state "$1")" = "$2" ] && return 0; sleep 1; done
  echo "the actor of $1 is $(actor_state "$1"), wanted $2" >&2; return 1
}
port_forward() {  # open, or reopen after a pod restart
  [ -z "$PF" ] || { kill "$PF" 2>/dev/null; wait "$PF" 2>/dev/null; }
  kubectl -n "$ATESPACE" port-forward svc/openshell-gateway "$GW_PORT:17670" > "$TMP/gateway-pf.log" 2>&1 & PF=$!
  local i
  for ((i = 0; i < 30; i++)); do (exec 3<>"/dev/tcp/127.0.0.1/$GW_PORT") 2>/dev/null && return 0; sleep 1; done
  fail "no port-forward to the gateway"
}
create() {  # name: the helpdesk agent, with the model's environment when asked
  local env=()
  [ -z "$WITH_MODEL" ] || env=(--env "OPENAI_BASE_URL=http://$MODEL_HOST:$MODEL_PORT/v1" --env "HELPDESK_MODEL=$MODEL_NAME")
  os sandbox create --name "$1" --from "$SANDBOX_IMAGE" --policy "$TMP/policy.yaml" --detach "${env[@]}" \
    -- python3 /opt/helpdesk/agent.py || return 1
  ID[$1]=$(getj "$1" | jq -r '.id // empty')
  [ -n "${ID[$1]}" ] || { echo "no sandbox id for $1" >&2; return 1; }
}
agent() {  # name status|chat|egress [args]: the agent over loopback, from inside the sandbox
  os sandbox exec "$1" -- python3 /opt/helpdesk/agent.py "${@:2}"
}
expect_turns() {  # name n: the agent's history has n turns
  local out got
  out=$(agent "$1" status) || { echo "$out"; return 1; }
  got=$(jq -r '.turns' <<<"$out") || { echo "$out"; return 1; }
  [ "$got" = "$2" ] || { echo "$1 reports turns=$got, expected $2" >&2; return 1; }
  echo "$1 turns=$got"
}
chat() {  # name message: a reply, not the agent's error JSON
  local out
  out=$(agent "$1" chat "$2") || { cut -c1-200 <<<"$out"; return 1; }
  jq -e '.reply' <<<"$out" >/dev/null || { cut -c1-200 <<<"$out"; return 1; }
  cut -c1-120 <<<"$out"
}
egress() {  # name url true|false: whether the agent reaches the url
  local out
  out=$(agent "$1" egress "$2") || { echo "$out"; return 1; }
  cut -c1-200 <<<"$out"
  jq -e --argjson want "$3" '.reached == $want' <<<"$out" >/dev/null
}
actor_messages() {
  echo "#### actor container messages"
  kubectl logs -n "$ATESPACE" -l ate.dev/worker-pool --tail=-1 2>/dev/null |
    jq -r --arg a "${ID[alice]:-}" 'select(.labels."ate.actor.name" == $a and (.message | test("launcher|Error|error|exited|attached|accepted|agent started|PROC:|SSH"))) | "\(.time[11:19]) \(.labels."ate.actor.container.name")\t\(.message)"' 2>/dev/null |
    tail -60 | cut -c1-220
}
diagnose() {
  echo "#### diagnostics"
  for s in "${!ID[@]}"; do phase "$s"; done
  kubectl-ate get actors -a "$ATESPACE" 2>/dev/null
  echo "#### driver log"; kubectl -n "$ATESPACE" logs deploy/openshell-gateway -c driver --tail=30 2>/dev/null
  echo "#### gateway log"; kubectl -n "$ATESPACE" logs deploy/openshell-gateway -c gateway --tail=40 2>/dev/null | sed -e 's/\x1b\[[0-9;]*m//g' | grep -v 'request{' | cut -c1-240
  actor_messages
}
finish() {
  [ "$rc" -eq 0 ] || diagnose
  [ -z "$PF" ] || kill "$PF" 2>/dev/null
  rm -rf "$TMP"
  echo "GATEWAY_TEST_EXIT=$rc"
}
trap finish EXIT

for t in kubectl kubectl-ate openshell jq; do command -v "$t" >/dev/null || fail "$t is not on PATH"; done
[ -f "$REPO/out/helpdesk.env" ] || fail "no out/helpdesk.env: run examples/helpdesk/build.sh"
# shellcheck source=/dev/null  # SANDBOX_IMAGE
. "$REPO/out/helpdesk.env"
"$REPO/harness/scripts/render.sh" "$REPO/examples/helpdesk/data.yaml.tmpl" > "$TMP/policy.yaml"

step "port-forward"; port_forward
step "cleanup"
for s in alice bob; do os sandbox delete "$s" >/dev/null 2>&1; done; sleep 2

step "create alice, then bob from the same golden snapshot"
create alice || fail "create alice"
wait_phase alice Ready 180 || fail "alice did not reach Ready"
echo "GATEWAY_SANDBOX_READY $(date -u +%H:%M:%SZ)"
create bob || fail "create bob"
wait_phase bob Ready 60 || fail "bob did not reach Ready"
[ "$(worker_pod alice)" != "$(worker_pod bob)" ] || fail "alice and bob share a worker; the fragment should take a whole one"

step "exec and logs"
os sandbox exec alice -- cat /etc/resolv.conf || fail exec
os logs alice -n 3 | cut -c1-160

step "the agent is fresh, and its egress is an allow-list"
expect_turns alice 0 || fail status
egress alice https://example.com/ false || fail "example.com was reachable from the sandbox"
[ -z "$WITH_MODEL" ] || egress alice "http://$MODEL_HOST:$MODEL_PORT/api/tags" true || fail "the model host was not reachable from the sandbox"
chat alice "User foo reports their database is timing out. Answer in one line." || fail chat
expect_turns alice 1 || fail status

step "gateway suspend, then resume: the new generation restarts the workload"
os sandbox suspend alice || fail suspend; wait_phase alice Suspended 60 || fail "suspend phase"
os sandbox resume alice || fail resume; wait_phase alice Ready 120 || fail "resume phase"
expect_turns alice 0 || fail status

step "gateway stop, then start"
os sandbox stop alice || fail stop; wait_phase alice Stopped 60 || fail "stop phase"
os sandbox start alice || fail start; wait_phase alice Ready 120 || fail "start phase"
chat alice "Say hi in three words." || fail chat; expect_turns alice 1 || fail status

step "a Substrate-side resume behind a stop: the gateway must stay Stopped"
os sandbox stop alice || fail stop; wait_phase alice Stopped 60 || fail "stop phase"
kubectl-ate resume actor -a "$ATESPACE" "${ID[alice]}" >/dev/null 2>&1 || fail "kubectl-ate resume"
wait_actor alice ACTOR_STATE_RUNNING 60 || fail "the actor did not resume"
sleep 8; phase alice
[ "$(getj alice | jq -r '.phase // empty')" = "Stopped" ] || fail "the gateway left Stopped on a resume it did not ask for"
os sandbox start alice || fail "start after the resume behind the gateway's back"
wait_phase alice Ready 120 || fail "start phase"
chat alice "Say hi again." || fail chat

step "a gateway pod restart behind a stop"
os sandbox stop alice || fail stop; wait_phase alice Stopped 60 || fail "stop phase"
kubectl -n "$ATESPACE" rollout restart deploy/openshell-gateway >/dev/null || fail "rollout restart"
kubectl -n "$ATESPACE" rollout status deploy/openshell-gateway --timeout=300s >/dev/null || fail "rollout status"
port_forward
wait_phase alice Stopped 60 || fail "the restarted gateway does not show Stopped"
os sandbox start alice || fail "start after the restart"; wait_phase alice Ready 180 || fail "start phase"
chat alice "One more line, please." || fail chat; expect_turns alice 1 || fail status

step "a Substrate-side suspend and resume: the gateway waits, the memory stays"
kubectl-ate suspend actor -a "$ATESPACE" --mode sync "${ID[alice]}" >/dev/null 2>&1 || fail "kubectl-ate suspend"
sleep 3; phase alice
kubectl-ate resume actor -a "$ATESPACE" "${ID[alice]}" >/dev/null 2>&1 || fail "kubectl-ate resume"
wait_phase alice Ready 90 || fail "the gateway did not return to Ready after the Substrate-side resume"
expect_turns alice 1 || fail "memory lost across the Substrate suspend"

step "bob was never involved"
[ "$(getj bob | jq -r '.phase // empty')" = "Ready" ] || fail "bob is not Ready"
expect_turns bob 0 || fail "bob's status"

step "alice's worker dies: Error for her, nothing for bob, then stop and start"
pod=$(worker_pod alice); [ -n "$pod" ] || fail "alice has no worker pod"
kubectl delete pod -n "$ATESPACE" "$pod" --grace-period=0 --force >/dev/null 2>&1 || fail "delete the worker pod"
wait_actor alice ACTOR_STATE_CRASHED 90 || fail "the actor did not crash"
wait_phase alice Error 60 || fail "the gateway did not show Error"
expect_turns bob 0 || fail "bob after alice's crash"
os sandbox stop alice || fail "stop from Error"; wait_phase alice Stopped 60 || fail "stop phase after the crash"
started='' err=''
for ((i = 0; i < 45; i++)); do  # the replacement worker registers within seconds
  if err=$(os sandbox start alice 2>&1); then started=1; break; fi; sleep 2
done
[ -n "$started" ] || fail "start after the crash kept failing: $err"
wait_phase alice Ready 180 || fail "start phase after the crash"
chat alice "Back from the dead?" || fail chat; expect_turns alice 1 || fail status
kubectl-ate get workers 2>/dev/null

step "delete"
os sandbox delete alice || fail "delete alice"; os sandbox delete bob || fail "delete bob"; sleep 3
os sandbox list; kubectl-ate get actors -a "$ATESPACE" 2>/dev/null
actor_messages
rc=0
