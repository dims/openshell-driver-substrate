#!/usr/bin/env bash
# Mint the credentials, then build and push the two helpdesk images.
#
#   REGISTRY=localhost:5001 examples/helpdesk/build.sh
#
# The tokens last one hour, so run run.sh within the hour. MODEL_* name an
# OpenAI-compatible endpoint the kind node can reach; it goes into the
# workload's environment and is the only host the sandbox policy allows.
set -o errexit -o nounset -o pipefail
REGISTRY=${REGISTRY:?e.g. localhost:5001}
MODEL_HOST=${MODEL_HOST:-172.18.0.1}   # the host, seen from a kind node
MODEL_PORT=${MODEL_PORT:-11434}
MODEL_NAME=${MODEL_NAME:-qwen2.5:1.5b}

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "${HERE}/../.." && pwd)
OUT=${REPO}/out
CTX=${OUT}/helpdesk
mkdir -p "${OUT}" "${CTX}/files/supervisor"

# Credentials (root README steps 4 and 5).
[[ -f "${OUT}/signing.key.pem" ]] || {
  openssl genpkey -algorithm ed25519 -out "${OUT}/signing.key.pem"
  openssl pkey -in "${OUT}/signing.key.pem" -pubout -out "${OUT}/signing.pub.pem"
}
BOOTSTRAP_CHILD_ENV="OPENAI_BASE_URL=http://${MODEL_HOST}:${MODEL_PORT}/v1,HELPDESK_MODEL=${MODEL_NAME}" \
  cargo run -q --manifest-path "${REPO}/harness/bootstrap-gen/Cargo.toml" -- "${OUT}"
"${REPO}/harness/scripts/package-credentials.sh" "${OUT}" "${REGISTRY}/openshell-sandbox:dev" "${REGISTRY}" >/dev/null

# The sandbox image: stock openshell-sandbox, python, the agent, its bootstrap.
cp "${HERE}"/{Dockerfile,agent.py,relay.py} "${OUT}/bootstrap.tar" "${CTX}/"
docker build -q --build-arg "SANDBOX_IMAGE=${REGISTRY}/openshell-sandbox:dev" \
  -t "${REGISTRY}/helpdesk-sandbox:dev" "${CTX}" >/dev/null
docker push -q "${REGISTRY}/helpdesk-sandbox:dev" >/dev/null

# The supervisor's files: descriptor, auth bundle, policy, data.
cp "${OUT}"/{runtime-descriptor.json,auth.json} "${HERE}/policy.rego" "${CTX}/files/supervisor/"
MODEL_HOST=${MODEL_HOST} MODEL_PORT=${MODEL_PORT} \
  "${REPO}/harness/scripts/render.sh" "${HERE}/data.yaml.tmpl" > "${CTX}/files/supervisor/data.yaml"
printf 'FROM scratch\nCOPY supervisor/ /supervisor/\n' > "${CTX}/files/Dockerfile"
docker build -q -t "${REGISTRY}/openshell-bootstrap-files:dev" "${CTX}/files" >/dev/null
docker push -q "${REGISTRY}/openshell-bootstrap-files:dev" >/dev/null

digest() { docker inspect --format '{{index .RepoDigests 0}}' "$1"; }
cat > "${OUT}/helpdesk.env" <<ENV
SANDBOX_IMAGE=$(digest "${REGISTRY}/helpdesk-sandbox:dev")
SUPERVISOR_IMAGE=$(digest "${REGISTRY}/openshell-supervisor:dev")
BOOTSTRAP_FILES_IMAGE=$(digest "${REGISTRY}/openshell-bootstrap-files:dev")
MODEL_HOST=${MODEL_HOST}
MODEL_PORT=${MODEL_PORT}
ENV
cat "${OUT}/helpdesk.env"
