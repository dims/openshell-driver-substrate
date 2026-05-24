#!/usr/bin/env bash
# 10-beat helpdesk demo driver. Every sandbox lifecycle call flows
# through openshell-gateway's public API
# (openshell.v1.OpenShell.{CreateSandbox,GetSandbox,ListSandboxes,DeleteSandbox})
# which in turn calls into openshell-driver-substrate, which finally
# talks to ate-api-server. The driver crate is now on the runtime path
# of every provisioning operation.
#
# Three acts:
#   I  (beats 1-3): provision alice + bob via CreateSandbox; list via
#                   ListSandboxes. The gateway carries
#                   SandboxTemplate.annotations["substrate_actor_template"]
#                   through to the driver (M3.16 read-from-annotations),
#                   so the driver reuses the pre-applied helpdesk-agent
#                   ActorTemplate without re-synthesizing it.
#   II (beats 4-9): one actor's life. Cold ask → Stop → Idle → Resume
#                   → Exfil deny → Pod-kill migration. Beat 9 is the
#                   multi-tenant proof: bob is unaffected.
#   III (beat 10):  cleanup via DeleteSandbox. Driver calls DeleteActor;
#                   the pre-provisioned ActorTemplate survives.
#
# Required env (or use defaults):
#   SUPERVISOR_IMAGE — digest-pinned image, must match helpdesk-template.yaml.
#   PROTO_DIR        — directory containing openshell.proto + friends.
set -euo pipefail
export PATH="$HOME/go/bin:$PATH"

NS_HD=ate-demo-helpdesk
NS_GW=ate-openshell-m0
SUPERVISOR_IMAGE="${SUPERVISOR_IMAGE:?set SUPERVISOR_IMAGE=<image>@sha256:<digest>}"
PROTO_DIR="${PROTO_DIR:-$HOME/proto}"
ROUTER_URL=http://localhost:8000
GW_URL=localhost:50051
ACTOR_TEMPLATE=helpdesk-agent

if [ ! -f "$PROTO_DIR/openshell.proto" ]; then
  echo "missing $PROTO_DIR/openshell.proto — copy from OpenShell-driver-substrate/proto/" >&2
  exit 1
fi

beat() { printf "\n\033[1;33m=== Beat %s: %s ===\033[0m\n" "$1" "$2"; }
osh()  { grpcurl -plaintext -import-path "$PROTO_DIR" -proto openshell.proto \
                -d @ "$GW_URL" "openshell.v1.OpenShell/$1" ; }
chat() { curl -sS -X POST -H "Host: $1.actors.resources.substrate.ate.dev" \
              -H "Content-Type: application/json" \
              -d "$3" "$ROUTER_URL/$2"; echo; }

# The gateway assigns each sandbox an internal UUID (metadata.id) which
# is what flows through to the substrate driver as the actor_id. Track
# the mapping from caller-supplied display name -> substrate actor id
# so subsequent operations (atenet curl, kubectl ate) target the real
# actor.
declare -A ACTOR_ID

create_sandbox() {
  local name=$1
  local resp
  resp=$(printf '{
    "name": "%s",
    "spec": {
      "log_level": "info",
      "policy": {"version": 1},
      "template": {
        "image": "%s",
        "annotations": {"substrate_actor_template": "%s"}
      }
    }
  }' "$name" "$SUPERVISOR_IMAGE" "$ACTOR_TEMPLATE" | osh CreateSandbox)
  echo "$resp"
  ACTOR_ID[$name]=$(echo "$resp" | jq -r '.sandbox.metadata.id')
  echo "  -> $name → actor_id ${ACTOR_ID[$name]}"
}

delete_sandbox() { printf '{"name":"%s"}' "$1" | osh DeleteSandbox; }
list_sandboxes() { printf '{}' | osh ListSandboxes | jq '.sandboxes // [] | map({id:.metadata.id, phase})'; }
actor_worker()   { kubectl ate get actor "$1" -o json | jq -r '.actors[0].ateomPodName // empty'; }
actor_status()   { kubectl ate get actor "$1" -o json | jq -r '.actors[0].status'; }

# Port-forwards: atenet for data plane, gateway for OpenShell gRPC.
kubectl port-forward -n ate-system svc/atenet-router 8000:80 >/dev/null 2>&1 &
PF_ROUTER=$!
kubectl port-forward -n "$NS_GW" svc/openshell-gateway-substrate 50051:50051 >/dev/null 2>&1 &
PF_GATEWAY=$!
trap '
  kill $PF_ROUTER $PF_GATEWAY 2>/dev/null || true
  # Best-effort: tear down both demo actors so the next run starts clean.
  for name in alice bob; do
    printf "{\"name\":\"%s\"}" "$name" | grpcurl -plaintext \
      -import-path "$PROTO_DIR" -proto openshell.proto \
      -d @ "$GW_URL" openshell.v1.OpenShell/DeleteSandbox >/dev/null 2>&1 || true
  done
' EXIT
sleep 3

# ─── Act I — Provisioning via gateway → driver → substrate ────────────────

beat 1 "Provision alice via OpenShell.CreateSandbox (driver path)"
time create_sandbox alice
sleep 5

beat 2 "Provision bob (second tenant in the same pool)"
time create_sandbox bob
sleep 5

beat 3 "ListSandboxes (gateway-mediated, driver-backed)"
list_sandboxes

# ─── Act II — One actor's life ────────────────────────────────────────────

ALICE=${ACTOR_ID[alice]}
BOB=${ACTOR_ID[bob]}

beat 4 "Cold ask to alice (data plane via atenet) — actor=$ALICE"
time chat "$ALICE" chat '{"message":"User foo reports their database is timing out — give me a triage checklist."}'

# Quiesce: the supervisor's HTTPS_PROXY → Ollama Cloud connection from
# Beat 4 may still be tearing down. A bare-second pause lets gVisor's
# cgroup hierarchy settle before we ask runsc to checkpoint and tear
# down the pause container — without this we sporadically hit
# "removing cgroup path /sys/fs/cgroup/pause: device or resource busy".
sleep 5

beat 5 "Suspend alice (substrate admin op; no public Suspend RPC on the gateway)"
time kubectl ate suspend actor "$ALICE"
for _ in $(seq 1 30); do
  [ "$(actor_status "$ALICE")" = "STATUS_SUSPENDED" ] && break
  sleep 1
done
echo "alice status=$(actor_status "$ALICE"), worker=$(actor_worker "$ALICE")"

beat 6 "20-second idle — capacity recovered"
kubectl ate get workers
sleep 20
kubectl ate get workers

beat 7 "Follow-up to alice (implicit resume, memory preserved)"
time chat "$ALICE" chat '{"message":"What was the user issue I just asked you about?"}'
ALICE_WORKER=$(actor_worker "$ALICE")
echo "alice resumed on worker: $ALICE_WORKER"

beat 8 "Exfil attempt from bob (expect blocked) — actor=$BOB"
chat "$BOB" probe '{}'

beat 9 "Kill alice's pod — alice migrates, bob is unaffected"
kubectl delete pod -n "$NS_HD" "$ALICE_WORKER" --wait=false
sleep 5
time chat "$ALICE" chat '{"message":"Confirm you still remember the user issue."}'
echo "alice: $ALICE_WORKER → $(actor_worker "$ALICE")"
echo "bob still on: $(actor_worker "$BOB")"
echo "verify bob still responds:"
chat "$BOB" chat '{"message":"Are you still here?"}'

# ─── Act III — Hygiene via gateway → driver → substrate ───────────────────

beat 10 "Delete alice via OpenShell.DeleteSandbox (driver path; bob untouched)"
time delete_sandbox alice
sleep 3
echo "post-delete list (bob should remain):"
list_sandboxes
echo "Pre-provisioned ActorTemplate survives (driver did NOT synthesize it):"
kubectl get actortemplate -n "$NS_HD" "$ACTOR_TEMPLATE" -o jsonpath='{.metadata.name}{"\n"}'
