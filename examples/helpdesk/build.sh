#!/usr/bin/env bash
# Build the runtime image, then the helpdesk image on top of it, and write
# their digests to out/helpdesk.env.
#
#   REGISTRY=localhost:5001 examples/helpdesk/build.sh
#
# Knobs: REGISTRY (required), OPENSHELL_VERSION (default 0.1.7).
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
# Same inputs, same digests: BuildKit stamps the build time into the image and
# records file mtimes, and a new digest means a new template. See --provenance below.
export SOURCE_DATE_EPOCH=0
REGISTRY=${REGISTRY:?e.g. localhost:5001}
export OPENSHELL_VERSION=${OPENSHELL_VERSION:-0.1.7}

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "${HERE}/../.." && pwd)
OUT=${REPO}/out
CTX=${OUT}/helpdesk
for t in docker go; do command -v "$t" >/dev/null || { echo "$t is not on PATH" >&2; exit 1; }; done
mkdir -p "${CTX}"

REGISTRY=${REGISTRY} "${REPO}/runtime/build.sh"
# shellcheck source=/dev/null  # RUNTIME_IMAGE
. "${OUT}/runtime.env"
CRANE=${OUT}/bin/crane  # installed by runtime/build.sh

cp "${HERE}"/{Dockerfile,agent.py} "${CTX}/"
find "${CTX}" -maxdepth 1 -type f -exec touch -d @0 {} +
docker build -q --provenance=false --sbom=false \
  --build-arg "RUNTIME_IMAGE=${RUNTIME_IMAGE}" \
  -t "${REGISTRY}/helpdesk-sandbox:dev" "${CTX}" >/dev/null
docker push -q "${REGISTRY}/helpdesk-sandbox:dev" >/dev/null

# The digest from the registry: a template must name the one it serves.
cat > "${OUT}/helpdesk.env" <<ENV
SANDBOX_IMAGE=${REGISTRY}/helpdesk-sandbox:dev@$("${CRANE}" digest "${REGISTRY}/helpdesk-sandbox:dev")
RUNTIME_IMAGE=${RUNTIME_IMAGE}
ENV
cat "${OUT}/helpdesk.env"
