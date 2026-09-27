#!/usr/bin/env bash
# Mint the credentials, then build and push the two helpdesk images.
#
#   REGISTRY=localhost:5001 examples/helpdesk/build.sh
#
# The tokens last one hour, so run run.sh within the hour. MODEL_* name an
# OpenAI-compatible endpoint the kind node can reach; it goes into the
# workload's environment and is the only host the sandbox policy allows.
set -o errexit -o nounset -o pipefail
# Same inputs, same digests: BuildKit stamps the build time into the image and
# records file mtimes, and a new digest means a new template. See --provenance below.
export SOURCE_DATE_EPOCH=0
REGISTRY=${REGISTRY:?e.g. localhost:5001}
MODEL_HOST=${MODEL_HOST:-172.18.0.1}   # the host, seen from a kind node
MODEL_PORT=${MODEL_PORT:-11434}
MODEL_NAME=${MODEL_NAME:-qwen2.5:1.5b}

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "${HERE}/../.." && pwd)
OUT=${REPO}/out
CTX=${OUT}/helpdesk
mkdir -p "${OUT}" "${CTX}/files/supervisor"

# Credentials (root README steps 4 and 5). Reused while under 30 minutes old
# and minted for the same model settings; a mint means a new template.
CHILD_ENV="OPENAI_BASE_URL=http://${MODEL_HOST}:${MODEL_PORT}/v1,HELPDESK_MODEL=${MODEL_NAME}"
if [[ -n $(find "${OUT}/bootstrap.json" -mmin -30 2>/dev/null) ]] &&
   grep -qxF "CHILD_ENV=${CHILD_ENV}" "${OUT}/helpdesk.env" 2>/dev/null; then
  echo "reusing the credentials in ${OUT}"
else
  [[ -f "${OUT}/signing.key.pem" ]] || {
    openssl genpkey -algorithm ed25519 -out "${OUT}/signing.key.pem"
    openssl pkey -in "${OUT}/signing.key.pem" -pubout -out "${OUT}/signing.pub.pem"
  }
  BOOTSTRAP_CHILD_ENV="${CHILD_ENV}" \
    cargo run -q --manifest-path "${REPO}/Cargo.toml" -p bootstrap-gen -- "${OUT}"
fi

# The sandbox unlinks its bootstrap after reading it, so it must sit on the
# writable rootfs, not an image volume. Substrate discards image file
# ownership, so the tree is world-writable rather than owned by 65532.
rm -rf "${OUT}/bake"; mkdir -p "${OUT}/bake/.openshell/channel/sandbox"
cp "${OUT}"/{bootstrap.json,server.crt,server.key} "${OUT}/bake/.openshell/channel/sandbox/"
chmod -R a+rwX "${OUT}/bake/.openshell"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -cf "${CTX}/bootstrap.tar" -C "${OUT}/bake" .

# The sandbox image: stock openshell-sandbox, python, the agent, its bootstrap.
cp "${HERE}"/{Dockerfile,agent.py,relay.py} "${CTX}/"
find "${CTX}" -maxdepth 1 -type f -exec touch -d @0 {} +
docker build -q --provenance=false --sbom=false --build-arg "SANDBOX_IMAGE=${REGISTRY}/openshell-sandbox:dev" \
  -t "${REGISTRY}/helpdesk-sandbox:dev" "${CTX}" >/dev/null
docker push -q "${REGISTRY}/helpdesk-sandbox:dev" >/dev/null

# The supervisor's files: descriptor, auth bundle, OpenShell's stock policy at
# the pinned rev, data.
REV=$(grep -oE 'rev = "[0-9a-f]{40}"' "${REPO}/Cargo.toml" | head -1 | cut -d'"' -f2)
curl -fsSL -o "${CTX}/files/supervisor/policy.rego" \
  "https://raw.githubusercontent.com/NVIDIA/OpenShell/${REV}/crates/openshell-supervisor-network/data/sandbox-policy.rego"
cp "${OUT}"/{runtime-descriptor.json,auth.json} "${CTX}/files/supervisor/"
MODEL_HOST=${MODEL_HOST} MODEL_PORT=${MODEL_PORT} \
  "${REPO}/harness/scripts/render.sh" "${HERE}/data.yaml.tmpl" > "${CTX}/files/supervisor/data.yaml"
printf 'FROM scratch\nCOPY supervisor/ /supervisor/\n' > "${CTX}/files/Dockerfile"
find "${CTX}/files" -type f -exec touch -d @0 {} +
docker build -q --provenance=false --sbom=false -t "${REGISTRY}/openshell-bootstrap-files:dev" "${CTX}/files" >/dev/null
docker push -q "${REGISTRY}/openshell-bootstrap-files:dev" >/dev/null

digest() { docker inspect --format '{{index .RepoDigests 0}}' "$1"; }
cat > "${OUT}/helpdesk.env" <<ENV
SANDBOX_IMAGE=$(digest "${REGISTRY}/helpdesk-sandbox:dev")
SUPERVISOR_IMAGE=$(digest "${REGISTRY}/openshell-supervisor:dev")
BOOTSTRAP_FILES_IMAGE=$(digest "${REGISTRY}/openshell-bootstrap-files:dev")
MODEL_HOST=${MODEL_HOST}
MODEL_PORT=${MODEL_PORT}
CHILD_ENV=${CHILD_ENV}
ENV
cat "${OUT}/helpdesk.env"
