#!/usr/bin/env bash
# The demo: two agents, one actor's life, one host's death, one deletion.
# Needs build.sh's out/helpdesk.env and a cluster from the root README.
# After each beat, `under` prints what Substrate and OpenShell logged for it.
set -o errexit -o nounset -o pipefail
ATESPACE=${ATESPACE:-ate-openshell-microvm}
BUCKET_NAME=${BUCKET_NAME:-ate-snapshots}
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "${HERE}/../.." && pwd)
set -o allexport; . "${REPO}/out/helpdesk.env"; set +o allexport
TEMPLATE=helpdesk-$(echo "${SANDBOX_IMAGE}${SUPERVISOR_IMAGE}${BOOTSTRAP_FILES_IMAGE}" | sha256sum | cut -c1-8)
export ATESPACE BUCKET_NAME TEMPLATE
WORK=$(mktemp -d)
T0=$(date +%s)
SINCE=

beat()  { SINCE=$(date -u +%Y-%m-%dT%H:%M:%SZ); printf '\n\033[1;33m== %s\033[0m  (+%ss)\n' "$*" "$(( $(date +%s) - T0 ))"; }
actors() { kubectl-ate get actors -a "${ATESPACE}"; }
state() { kubectl-ate get actors -a "${ATESPACE}" "$1" -o json | jq -r '.status.state'; }
pod()   { kubectl-ate get actors -a "${ATESPACE}" "$1" -o json | jq -r '.status.workerAssignment.workerPod'; }
until_state() {  # actor state [seconds]
  local i; for ((i = 0; i < ${3:-120}; i++)); do
    [[ $(state "$1") == "$2" ]] && return 0; sleep 1
  done; echo "$1 did not reach $2" >&2; return 1
}
ask() {  # actor path [curl args]: through atenet-router's CONNECT tunnel to the relay
  curl -sS --max-time 90 -p -x http://127.0.0.1:8001 \
    --proxy-header "ate-target-actor: ${ATESPACE}/$1" "${@:3}" "http://$1:8081$2"; echo
}
until_up() { local i; for ((i = 0; i < 60; i++)); do ask "$1" /status >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }
chat()  { ask "$1" /chat -X POST -H 'Content-Type: application/json' -d "{\"message\":\"$2\"}"; }
allow_egress() {  # actor: Substrate's own egress gate, the model host only
  grpcurl -cacert "${WORK}/ca.crt" -authority api.ate-system.svc \
    -H "authorization: Bearer $(cat "${WORK}/token")" \
    -import-path "${REPO}/proto" -proto ateapi.proto \
    -d "{\"actor\":{\"atespace\":\"${ATESPACE}\",\"name\":\"$1\"},
         \"egress_policy\":{\"metadata\":{\"atespace\":\"${ATESPACE}\",\"name\":\"default\"},
                            \"rules\":[{\"cidrs\":{\"cidrs\":[\"${MODEL_HOST}/32\"]}}]}}" \
    127.0.0.1:8443 ateapi.Control/CreateActorEgressPolicy >/dev/null
}
under() {  # pattern [namespace selector source]: matching log lines since the beat began
  sleep 1
  kubectl logs -n "${2:-${ATESPACE}}" -l "${3:-ate.dev/worker-pool}" --since-time="${SINCE}" --tail=-1 2>/dev/null |
    jq -r --arg src "${4:-ateom}" '
      if .message then
        "  \(.labels."ate.actor.name" // "-" | if test("^[0-9a-f]{8}-") then "golden" else . end)/\(.labels."ate.actor.container.name" // "actor")\t\(.message | sub("^\\S+Z ";""))"
      elif .msg then
        "  \($src)\t\(.msg)\(if .total then " in \(.total/1000000|floor) ms" else "" end)\(if .snapshot_files then "  \(.snapshot_files | join(" "))" else "" end)\(if ."ate.actor.name" then "  \(."ate.actor.name")" else "" end)"
      else empty end' 2>/dev/null | grep -E "$1" | cut -c1-220 | awk '!seen[$0]++' || true
}

kubectl port-forward -n ate-system svc/atenet-router 8001:8081 >/dev/null 2>&1 &
kubectl port-forward -n ate-system svc/api 8443:443 >/dev/null 2>&1 &
trap 'for a in alice bob; do kubectl-ate delete actor -a "${ATESPACE}" --any-state "$a" >/dev/null 2>&1 || true; done
      kill $(jobs -p) 2>/dev/null; rm -rf "${WORK}"' EXIT
kubectl create token ate-client -n ate-system --audience api.ate-system.svc --duration=1h > "${WORK}/token"
kubectl get clustertrustbundle servicedns.podcert.ate.dev:identity:primary-bundle \
  -o jsonpath='{.spec.trustBundle}' > "${WORK}/ca.crt"
sleep 2

beat "1  Template and golden snapshot"
if kubectl-ate get actor-template -a "${ATESPACE}" "${TEMPLATE}" >/dev/null 2>&1; then
  echo "  ${TEMPLATE} exists; reusing its golden snapshot"
else
  "${REPO}/harness/scripts/render.sh" "${HERE}/template.yaml.tmpl" | kubectl-ate create actor-template -f - >/dev/null
  until kubectl-ate get actor-template -a "${ATESPACE}" "${TEMPLATE}" -o json |
        jq -e '.status.goldenSnapshotStatus.goldenTag.name' >/dev/null 2>&1; do sleep 2; done
fi
kubectl-ate get actor-template -a "${ATESPACE}" "${TEMPLATE}"
under 'qualif|Landlock ruleset|listener ready|boundary attached|PROC:LAUNCH|Actor checkpointed'

beat "2  Two agents restored from that one snapshot"
for a in alice bob; do
  kubectl-ate create actor "$a" -a "${ATESPACE}" --template "${TEMPLATE}" >/dev/null
  kubectl-ate resume actor -a "${ATESPACE}" "$a" >/dev/null
done
for a in alice bob; do until_state "$a" ACTOR_STATE_RUNNING; until_up "$a"; allow_egress "$a"; done
actors
under 'Actor restored'

beat "3  Egress is an allow-list: the model host, from python, and nothing else"
ask alice "/egress?url=https://example.com/"
ask alice "/egress?url=http://${MODEL_HOST}:${MODEL_PORT}/api/tags"
under 'network_broker|OCSF'

beat "4  alice answers through the supervisor's proxy"
chat alice "User foo reports their database is timing out. Give me a three-step triage checklist."
under 'OCSF HTTP'

beat "5  Suspend alice: a snapshot is written and her worker is free"
kubectl-ate suspend actor -a "${ATESPACE}" alice >/dev/null; until_state alice ACTOR_STATE_SUSPENDED
kubectl-ate get workers
under 'checkpoint'

beat "6  Resume alice: her memory comes back with the snapshot"
kubectl-ate resume actor -a "${ATESPACE}" alice >/dev/null; until_state alice ACTOR_STATE_RUNNING; until_up alice
ask alice /status
chat alice "In one sentence, what problem did the user report?"
under 'Actor restored'

beat "7  bob was never involved"
ask bob /status

beat "8  alice's host dies: alice crashes, bob does not"
kubectl delete pod -n "${ATESPACE}" "$(pod alice)" --grace-period=0 --force 2>/dev/null
until_state alice ACTOR_STATE_CRASHED
actors
kubectl-ate get workers
under 'Releasing actor|pod is gone' ate-system app=ate-api-server ateapi

beat "9  Revert alice to her last snapshot; she resumes on the new worker"
kubectl-ate revert actor -a "${ATESPACE}" alice >/dev/null; until_state alice ACTOR_STATE_SUSPENDED
for ((i = 0; i < 90; i++)); do  # the replacement worker registers within seconds
  kubectl-ate resume actor -a "${ATESPACE}" alice >/dev/null 2>&1 && break; sleep 2
done
until_state alice ACTOR_STATE_RUNNING 300; until_up alice
kubectl-ate get workers
actors
ask alice /status
under 'Actor restored'

beat "10 Delete alice; bob and the template stay"
kubectl-ate delete actor -a "${ATESPACE}" --any-state alice >/dev/null
actors
chat bob "In one sentence, what does a helpdesk triage agent do?"

beat "Done"
