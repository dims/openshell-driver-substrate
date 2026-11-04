#!/usr/bin/env bash
# The driver as the harness runs it: built from this checkout, dialing
# ate-api-server through a port-forward with a 24 h ate-client token, serving
# ComputeDriver with reflection on $WORK/driver/driver.sock, configured with
# the helpdesk template fragment and egress policy. Rerun after build.sh.
#
#   harness/scripts/driver-up.sh            # start or restart
#   harness/scripts/driver-up.sh --stop
#   harness/scripts/driver-up.sh --refresh-token
#
# Environment: WORK (default $HOME/openshell-substrate), ATESPACE (default
# ate-openshell-microvm), API_LOCAL_PORT (default 8443), GRPCURL_VERSION.
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${WORK:-$HOME/openshell-substrate}
DIR=$WORK/driver
export ATESPACE=${ATESPACE:-ate-openshell-microvm}
API_LOCAL_PORT=${API_LOCAL_PORT:-8443}
BIN=$REPO/target/release/openshell-driver-substrate
SOCK=$DIR/driver.sock
SVC=openshell.compute.v1.ComputeDriver
mkdir -p "$DIR" "$WORK/bin"
export PATH=$WORK/bin:$PATH

step() { printf '\n== %s %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
fail() { echo "FAILED: $*" >&2; exit 1; }
alive() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }
stop() {  # pidfile
  if alive "$1"; then kill "$(cat "$1")"; while kill -0 "$(cat "$1")" 2>/dev/null; do sleep 0.2; done; fi
  rm -f "$1"
}
wait_for() {  # seconds what command...
  local deadline=$(( $(date +%s) + $1 )); local what=$2; shift 2
  until "$@" >/dev/null 2>&1; do [ "$(date +%s)" -lt "$deadline" ] || fail "$what did not come up"; sleep 1; done
}
mint_token() {  # atomic replace: the driver reads the file on every client build
  (umask 077; kubectl create token ate-client -n ate-system --audience api.ate-system.svc --duration=24h > "$DIR/token.new")
  mv "$DIR/token.new" "$DIR/token"
  echo $(( $(date +%s) + 24 * 3600 - 300 )) > "$DIR/token.expires-at"
}

case ${1:-} in
  --stop) stop "$DIR/driver.pid"; stop "$DIR/port-forward.pid"; rm -f "$SOCK"; exit 0 ;;
  --refresh-token) mint_token; exit 0 ;;
  "") ;;
  *) fail "unknown argument $1" ;;
esac

step "tools"
for t in cargo kubectl go; do command -v "$t" >/dev/null || fail "$t is not on PATH"; done
command -v grpcurl >/dev/null || GOBIN=$WORK/bin go install "github.com/fullstorydev/grpcurl/cmd/grpcurl@${GRPCURL_VERSION:-v1.9.4}"

step "build"
cargo build -q --release --manifest-path "$REPO/Cargo.toml" -p openshell-driver-substrate

step "configuration"
[ -f "$REPO/out/helpdesk.env" ] || fail "no out/helpdesk.env: run examples/helpdesk/build.sh"
set -o allexport
# shellcheck source=/dev/null
. "$REPO/out/runtime.env"
# shellcheck source=/dev/null
. "$REPO/out/helpdesk.env"
set +o allexport
"$REPO/harness/scripts/render.sh" "$REPO/examples/helpdesk/template.yaml.tmpl" > "$DIR/template.yaml"
"$REPO/harness/scripts/render.sh" "$REPO/examples/helpdesk/egress-policy.yaml.tmpl" > "$DIR/egress-policy.yaml"
printf 'template_file = "template.yaml"\negress_policy_file = "egress-policy.yaml"\n' > "$DIR/driver.toml"

step "credentials"
kubectl get clustertrustbundle servicedns.podcert.ate.dev:identity:primary-bundle -o jsonpath='{.spec.trustBundle}' > "$DIR/ctb.crt"
mint_token

step "port-forward to ate-api-server"
if ! alive "$DIR/port-forward.pid"; then
  kubectl port-forward -n ate-system svc/api "$API_LOCAL_PORT:443" > "$DIR/port-forward.log" 2>&1 &
  echo $! > "$DIR/port-forward.pid"
fi
wait_for 30 "the port-forward" bash -c "exec 3<>/dev/tcp/127.0.0.1/$API_LOCAL_PORT"

step "driver"
stop "$DIR/driver.pid"
rm -f "$SOCK"
nohup "$BIN" --bind-socket "$SOCK" --api-endpoint "127.0.0.1:$API_LOCAL_PORT" \
  --api-tls-ca "$DIR/ctb.crt" --api-tls-server-name api.ate-system.svc \
  --api-bearer-token-path "$DIR/token" --atespace "$ATESPACE" \
  --config "$DIR/driver.toml" > "$DIR/driver.log" 2>&1 &
echo $! > "$DIR/driver.pid"
wait_for 20 "the driver" grpcurl -plaintext -unix "$SOCK" list
grpcurl -plaintext -unix -max-time 15 -d '{}' "$SOCK" "$SVC/ListSandboxes" >/dev/null ||
  { tail -20 "$DIR/driver.log"; fail "the driver cannot list actors: token, trust bundle or port-forward"; }
echo "driver on $SOCK; token until $(date -u -d @"$(cat "$DIR/token.expires-at")" +%FT%TZ); log $DIR/driver.log"
