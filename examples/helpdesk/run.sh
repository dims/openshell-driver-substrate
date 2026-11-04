#!/usr/bin/env bash
# The demo: two agents, one actor's life, one host's death, one deletion.
# Needs build.sh's out/helpdesk.env and a cluster from harness/scripts/kind-up.sh.
# After each beat, `under` prints what Substrate and OpenShell logged for it.
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
export ATESPACE=${ATESPACE:-ate-openshell-microvm}
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "${HERE}/../.." && pwd)
set -o allexport
# shellcheck source=/dev/null  # MODEL_HOST, MODEL_PORT and CHILD_ENV come from the env file
. "${REPO}/out/helpdesk.env"
set +o allexport
template() {  # the driver's fragment with the name and the pool the demo gives it
  printf 'metadata:\n  atespace: %s\n  name: %s\nworkerSelector:\n  matchLabels:\n    workload: openshell-microvm\n' "${ATESPACE}" "${TEMPLATE}"
  "${REPO}/harness/scripts/render.sh" "${HERE}/template.yaml.tmpl"
}
TEMPLATE_HASH=$(TEMPLATE=x template | sha256sum | cut -c1-8)
export TEMPLATE=helpdesk-${TEMPLATE_HASH}
TMP=$(mktemp -d)
T0=$(date +%s)
SINCE=
POOL=openshell-microvm
# Local ports of the router proxies, one per actor, and the proxy processes.
declare -A PORT=([alice]=18081 [bob]=18082)
declare -A PROXY_PID=()
MODEL_LOCAL_PORT=18434

beat()  { SINCE=$(date -u +%Y-%m-%dT%H:%M:%SZ); printf '\n\033[1;33m== %s\033[0m  (+%ss)\n' "$*" "$(( $(date +%s) - T0 ))"; }
actors() { kubectl-ate get actors -a "${ATESPACE}"; }
state() { kubectl-ate get actors -a "${ATESPACE}" "$1" -o json | jq -r '.status.state'; }
pod()   { kubectl-ate get actors -a "${ATESPACE}" "$1" -o json | jq -r '.status.workerAssignment.workerPod // empty'; }
until_state() {  # actor state [seconds]
  local i; for ((i = 0; i < ${3:-120}; i++)); do
    [[ $(state "$1") == "$2" ]] && return 0; sleep 1
  done; echo "$1 did not reach $2" >&2; return 1
}
proxy() {  # actor: a local port to the relay through atenet-router, which takes mTLS; kubectl-ate holds the client certificate
  kubectl-ate proxy actor -a "${ATESPACE}" "$1" "${PORT[$1]}:8081" > "${TMP}/proxy-$1.log" 2>&1 &
  PROXY_PID[$1]=$!
  sleep 1
  kill -0 "${PROXY_PID[$1]}" 2>/dev/null || { cat "${TMP}/proxy-$1.log" >&2; echo "the proxy for $1 did not start" >&2; return 1; }
}
unproxy() {  # actor: the router resumes a suspended actor on any connection, so nothing may listen for one while it is suspended
  kill "${PROXY_PID[$1]}" 2>/dev/null || true
  wait "${PROXY_PID[$1]}" 2>/dev/null || true
  unset "PROXY_PID[$1]"
}
ask() {  # actor path [curl args]
  curl -fsS --max-time 90 "${@:3}" "http://127.0.0.1:${PORT[$1]}$2" && echo
}
until_up() {  # actor: a 502 from the router is not up
  local i; for ((i = 0; i < 60; i++)); do ask "$1" /status >/dev/null 2>&1 && return 0; sleep 1; done
  echo "$1 never answered /status through the router; read the worker pod log" >&2; return 1
}
expect_turns() {  # actor n: /status must report that many turns
  local out; out=$(ask "$1" /status); echo "${out}"
  jq -e --argjson n "$2" '.turns == $n' <<<"${out}" >/dev/null || { echo "$1 reports turns $(jq -r .turns <<<"${out}"), expected $2" >&2; return 1; }
}
chat()  {  # actor message: the answer must carry a reply, not the app's error JSON
  local out; out=$(ask "$1" /chat -X POST -H 'Content-Type: application/json' -d "{\"message\":\"$2\"}")
  echo "${out}"; jq -e '.reply' <<<"${out}" >/dev/null || { echo "$1 gave no reply" >&2; return 1; }
}
allow_egress() {  # actor: Substrate's own egress gate, the model host only
  "${REPO}/harness/scripts/render.sh" "${HERE}/egress-policy.yaml.tmpl" > "${TMP}/egress-policy.yaml"
  kubectl-ate create egress-policy -a "${ATESPACE}" "$1" -f "${TMP}/egress-policy.yaml" >/dev/null
}
under() {  # pattern [namespace selector source]: matching log lines since the beat began
  sleep 1
  kubectl logs -n "${2:-${ATESPACE}}" -l "${3:-ate.dev/worker-pool}" --since-time="${SINCE}" --tail=-1 2>/dev/null |
    jq -R -r --arg src "${4:-ateworker}" 'fromjson? |
      if .message then
        "  \(.labels."ate.actor.name" // "-" | if test("^[0-9a-f]{8}-") then "golden" else . end)/\(.labels."ate.actor.container.name" // "actor")\t\(.message | sub("^\\S+Z ";""))"
      elif .msg then
        "  \($src)\t\(.msg)\(if .total then " in \(.total/1000000|floor) ms" else "" end)\(if .snapshot_files then "  \(.snapshot_files | join(" "))" else "" end)\(if ."ate.actor.name" then "  \(."ate.actor.name")" else "" end)"
      else empty end' 2>/dev/null | grep -E "$1" | cut -c1-220 | awk '!seen[$0]++' || true
}
free_workers() {
  kubectl-ate get workers -o json 2>/dev/null | jq --arg pool "${POOL}" \
    '[.workers[] | select(.workerPool == $pool and .status.state == "WORKER_STATE_ACTIVE" and (.status.allocated.actors // 0) == 0)] | length'
}

free=0
for ((i = 0; i < 30; i++)); do  # a delete is asynchronous: the previous run's actors may still be going
  free=$(free_workers); (( free >= 2 )) && break; sleep 2
done
(( free >= 2 )) || { echo "need two free workers in pool ${POOL}, have ${free}: delete other actors, or raise the pool's replicas" >&2; exit 1; }

cleanup() {
  local rc=$?
  if (( rc == 0 )); then
    for a in alice bob; do kubectl-ate delete actor -a "${ATESPACE}" "$a" >/dev/null 2>&1 || true; done
  else
    printf "\nkept alice and bob for inspection: kubectl-ate delete actor -a %s <name>\n" "${ATESPACE}" >&2
    for f in "${TMP}"/proxy-*.log; do [[ -f $f ]] && { echo "== $(basename "$f")" >&2; tail -5 "$f" >&2; }; done
  fi
  jobs -p | xargs -r kill 2>/dev/null || true
  rm -rf "${TMP}"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
kubectl -n "${ATESPACE}" port-forward svc/ollama "${MODEL_LOCAL_PORT}:${MODEL_PORT}" >/dev/null 2>&1 &
MODEL=${CHILD_ENV##*HELPDESK_MODEL=}; MODEL=${MODEL%%,*}  # load the model before the timed beats compete with snapshot I/O
curl -fsS --retry 20 --retry-connrefused --retry-delay 1 --max-time 120 -o /dev/null \
  "http://127.0.0.1:${MODEL_LOCAL_PORT}/v1/chat/completions" -H 'Content-Type: application/json' \
  -d "{\"model\":\"${MODEL}\",\"messages\":[{\"role\":\"user\",\"content\":\"Say OK.\"}],\"max_tokens\":5}" ||
  { echo "the model service ${MODEL_HOST}:${MODEL_PORT} did not answer a chat for ${MODEL} within 120 s" >&2; exit 1; }

beat "1  Template and golden snapshot"
if kubectl-ate get actor-template -a "${ATESPACE}" "${TEMPLATE}" >/dev/null 2>&1; then
  echo "  ${TEMPLATE} exists; reusing its golden snapshot"
else
  template | kubectl-ate create actor-template -f - >/dev/null; created=1
  for ((i = 0; ; i++)); do
    kubectl-ate get actor-template -a "${ATESPACE}" "${TEMPLATE}" -o json |
      jq -e '.status.goldenSnapshotStatus.goldenTag.name' >/dev/null 2>&1 && break
    (( i < 150 )) || { echo "no golden snapshot after 300 s: $(kubectl-ate get actor-template -a "${ATESPACE}" "${TEMPLATE}" -o json | jq -r '.status.goldenSnapshotStatus.errorMessage // "no error message"')" >&2; exit 1; }
    sleep 2
  done
fi
kubectl-ate get actor-template -a "${ATESPACE}" "${TEMPLATE}"
evidence=$(under 'Landlock ruleset|listener ready|boundary attached|PROC:LAUNCH|Actor checkpointed'); echo "${evidence}"
[[ -z ${created:-} ]] || { grep -q 'boundary attached' <<<"${evidence}" && grep -q 'PROC:LAUNCH' <<<"${evidence}"; } ||
  { echo "the golden warm-up never logged 'Isolation boundary attached' and 'PROC:LAUNCH': the sandbox did not clear its gates or the supervisor never reached it; read the worker pod log" >&2; exit 1; }

beat "2  Two agents restored from that one snapshot"
for a in alice bob; do
  kubectl-ate create actor "$a" -a "${ATESPACE}" --template "${TEMPLATE}" >/dev/null
  kubectl-ate resume actor -a "${ATESPACE}" "$a" >/dev/null
done
for a in alice bob; do until_state "$a" ACTOR_STATE_RUNNING; proxy "$a"; until_up "$a"; allow_egress "$a"; done
actors
under 'Actor restored'

beat "3  Egress is an allow-list: the model host, from python, and nothing else"
denied=$(ask alice "/egress?url=https://example.com/"); echo "${denied}"
jq -e '.reached == false' <<<"${denied}" >/dev/null || { echo "example.com was reachable from the sandbox" >&2; exit 1; }
allowed=$(ask alice "/egress?url=http://${MODEL_HOST}:${MODEL_PORT}/api/tags"); echo "${allowed}"
jq -e '.reached == true' <<<"${allowed}" >/dev/null || { echo "the model host was not reachable from the sandbox" >&2; exit 1; }
under 'network_broker|OCSF'

beat "4  alice answers through the supervisor's proxy"
chat alice "User foo reports their database is timing out. Give me a three-step triage checklist."
expect_turns alice 1 >/dev/null
under 'OCSF HTTP'

beat "5  Suspend alice: a snapshot is written and her worker is free"
unproxy alice
kubectl-ate suspend actor -a "${ATESPACE}" --mode sync alice >/dev/null; until_state alice ACTOR_STATE_SUSPENDED
kubectl-ate get workers
under 'checkpoint'

beat "6  Resume alice: her memory comes back with the snapshot"
kubectl-ate resume actor -a "${ATESPACE}" alice >/dev/null; until_state alice ACTOR_STATE_RUNNING; proxy alice; until_up alice
expect_turns alice 1
chat alice "In one sentence, what problem did the user report?"
expect_turns alice 2 >/dev/null
under 'Actor restored'

beat "7  bob was never involved"
expect_turns bob 0

beat "8  alice's host dies: alice crashes, bob does not"
worker_pod=$(pod alice)
[[ -n ${worker_pod} ]] || { echo "alice has no worker pod" >&2; exit 1; }
unproxy alice
kubectl delete pod -n "${ATESPACE}" "${worker_pod}" --grace-period=0 --force
until_state alice ACTOR_STATE_CRASHED
actors
kubectl-ate get workers
under 'Releasing actor|pod is gone' ate-system app=ate-api-server ateapi

beat "9  Revert alice to her last snapshot; she resumes on the new worker"
kubectl-ate revert actor -a "${ATESPACE}" alice >/dev/null; until_state alice ACTOR_STATE_SUSPENDED
resumed=
for ((i = 0; i < 90; i++)); do  # the replacement worker registers within seconds
  if err=$(kubectl-ate resume actor -a "${ATESPACE}" alice 2>&1 >/dev/null); then resumed=1; break; fi; sleep 2
done
[[ -n ${resumed} ]] || { echo "resume after revert kept failing: ${err}" >&2; exit 1; }
until_state alice ACTOR_STATE_RUNNING 300; proxy alice; until_up alice
kubectl-ate get workers
actors
expect_turns alice 1
under 'Actor restored'

beat "10 Delete alice; bob and the template stay"
unproxy alice
kubectl-ate delete actor -a "${ATESPACE}" alice >/dev/null
actors
chat bob "In one sentence, what does a helpdesk triage agent do?"

beat "Done"
