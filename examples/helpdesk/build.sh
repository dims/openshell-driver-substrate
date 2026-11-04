#!/usr/bin/env bash
# Build the runtime image, mint the credentials for the run without a gateway,
# then build and push the two helpdesk images.
#
#   REGISTRY=localhost:5001 examples/helpdesk/build.sh
#
# MODEL_HOST is the DNS name of an OpenAI-compatible endpoint the actor can
# reach; it goes into the workload's environment and is the only host the
# sandbox policy allows.
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
# Same inputs, same digests: BuildKit stamps the build time into the image and
# records file mtimes, and a new digest means a new template. See --provenance below.
export SOURCE_DATE_EPOCH=0
REGISTRY=${REGISTRY:?e.g. localhost:5001}
ATESPACE=${ATESPACE:-ate-openshell-microvm}
MODEL_HOST=${MODEL_HOST:-ollama.${ATESPACE}.svc.cluster.local}
MODEL_PORT=${MODEL_PORT:-11434}
MODEL_NAME=${MODEL_NAME:-qwen2.5:1.5b}
export OPENSHELL_VERSION=${OPENSHELL_VERSION:-0.1.7}

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "${HERE}/../.." && pwd)
OUT=${REPO}/out
CTX=${OUT}/helpdesk
for t in docker cargo go openssl; do command -v "$t" >/dev/null || { echo "$t is not on PATH" >&2; exit 1; }; done
mkdir -p "${CTX}/files/supervisor"

REGISTRY=${REGISTRY} "${REPO}/runtime/build.sh"
# shellcheck source=/dev/null  # RUNTIME_IMAGE
. "${OUT}/runtime.env"
CRANE=${OUT}/bin/crane  # installed by runtime/build.sh
digest() { echo "$1@$("${CRANE}" digest "$1")"; }

# Credentials, minted once per model setting and kept in out/: a mint means
# new digests, a new template and a new golden snapshot. `rm -rf out` mints again.
CHILD_ENV="OPENAI_BASE_URL=http://${MODEL_HOST}:${MODEL_PORT}/v1,HELPDESK_MODEL=${MODEL_NAME}"
if [[ -f ${OUT}/bootstrap.json ]] && grep -qxF "CHILD_ENV=${CHILD_ENV}" "${OUT}/helpdesk.env" 2>/dev/null; then
  echo "reusing the credentials in ${OUT}"
else
  [[ -f ${OUT}/signing.key.pem ]] || (
    umask 077
    openssl genpkey -algorithm ed25519 -out "${OUT}/signing.key.pem"
    openssl pkey -in "${OUT}/signing.key.pem" -pubout -out "${OUT}/signing.pub.pem"
  )
  BOOTSTRAP_CHILD_ENV="${CHILD_ENV}" \
    cargo run -q --manifest-path "${REPO}/Cargo.toml" -p bootstrap-gen -- "${OUT}"
fi

# The sandbox unlinks its bootstrap after reading it, so it sits on the
# writable rootfs, owned by the sandbox user, not on an image volume.
BAKE=${OUT}/bake
rm -rf "${BAKE}"; mkdir -p "${BAKE}/.openshell/channel/sandbox"
cp "${OUT}"/{bootstrap.json,server.crt,server.key} "${BAKE}/.openshell/channel/sandbox/"
tar --sort=name --owner=65532 --group=65532 --numeric-owner --mtime=@0 --mode='u=rwX,go=' \
  -cf "${CTX}/bootstrap.tar" -C "${BAKE}" .
rm -rf "${BAKE}"

# The sandbox image: the runtime image, the agent, its bootstrap.
cp "${HERE}"/{Dockerfile,agent.py} "${CTX}/"
find "${CTX}" -maxdepth 1 -type f -exec touch -d @0 {} +
docker build -q --provenance=false --sbom=false \
  --build-arg "RUNTIME_IMAGE=${RUNTIME_IMAGE}" \
  -t "${REGISTRY}/helpdesk-sandbox:dev" "${CTX}" >/dev/null
docker push -q "${REGISTRY}/helpdesk-sandbox:dev" >/dev/null

# The supervisor's files: descriptor, auth bundle, OpenShell's stock policy at
# the pinned rev, data.
REV=$(grep -oE -m1 'rev = "[0-9a-f]{40}"' "${REPO}/Cargo.toml" | cut -d'"' -f2)
curl -fsSL -o "${CTX}/files/supervisor/policy.rego" \
  "https://raw.githubusercontent.com/NVIDIA/OpenShell/${REV}/crates/openshell-supervisor-network/data/sandbox-policy.rego"
cp "${OUT}"/{runtime-descriptor.json,auth.json} "${CTX}/files/supervisor/"
MODEL_HOST=${MODEL_HOST} MODEL_PORT=${MODEL_PORT} \
  "${REPO}/harness/scripts/render.sh" "${HERE}/data.yaml.tmpl" > "${CTX}/files/supervisor/data.yaml"
printf 'FROM scratch\nCOPY supervisor/ /supervisor/\n' > "${CTX}/files/Dockerfile"
find "${CTX}/files" -type f -exec touch -d @0 {} +
docker build -q --provenance=false --sbom=false -t "${REGISTRY}/openshell-bootstrap-files:dev" "${CTX}/files" >/dev/null
docker push -q "${REGISTRY}/openshell-bootstrap-files:dev" >/dev/null

SANDBOX_IMAGE=$(digest "${REGISTRY}/helpdesk-sandbox:dev")
BOOTSTRAP_FILES_IMAGE=$(digest "${REGISTRY}/openshell-bootstrap-files:dev")
cat > "${OUT}/helpdesk.env" <<ENV
SANDBOX_IMAGE=${SANDBOX_IMAGE}
RUNTIME_IMAGE=${RUNTIME_IMAGE}
BOOTSTRAP_FILES_IMAGE=${BOOTSTRAP_FILES_IMAGE}
MODEL_HOST=${MODEL_HOST}
MODEL_PORT=${MODEL_PORT}
CHILD_ENV=${CHILD_ENV}
ENV
cat "${OUT}/helpdesk.env"
