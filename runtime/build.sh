#!/usr/bin/env bash
# Build and push the runtime image every workload on this driver starts from,
# and write its digest to out/runtime.env.
#
#   REGISTRY=localhost:5001 runtime/build.sh
#
# The OpenShell sandbox and supervisor images are mirrored from ghcr.io into
# REGISTRY for this platform only, so build on the architecture the cluster
# runs. Knobs: REGISTRY (required), OPENSHELL_VERSION (default 0.1.7).
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
# Same inputs, same digest: BuildKit stamps the build time into the image and
# records file mtimes, and a new digest means a new template.
export SOURCE_DATE_EPOCH=0
REGISTRY=${REGISTRY:?e.g. localhost:5001}
OPENSHELL_VERSION=${OPENSHELL_VERSION:-0.1.7}

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "${HERE}/.." && pwd)
OUT=${REPO}/out
CTX=${OUT}/runtime
for t in docker go; do command -v "$t" >/dev/null || { echo "$t is not on PATH" >&2; exit 1; }; done
mkdir -p "${OUT}/bin" "${CTX}"

# crane asks the registry for a tag's digest, which is what a template must
# name: Docker's RepoDigests report the upstream index digest for a mirrored tag.
GOBIN=${OUT}/bin go install github.com/google/go-containerregistry/cmd/crane@v0.22.1
CRANE=${OUT}/bin/crane
digest() { echo "$1@$("${CRANE}" digest "$1")"; }

PLATFORM=linux/$(uname -m | sed -e 's/x86_64/amd64/' -e 's/aarch64/arm64/')
for img in sandbox supervisor; do
  "${CRANE}" copy --platform "${PLATFORM}" "ghcr.io/nvidia/openshell/${img}:${OPENSHELL_VERSION}" \
    "${REGISTRY}/openshell-${img}:${OPENSHELL_VERSION}" >/dev/null
done

cp "${HERE}"/{Dockerfile,launcher.py} "${CTX}/"
find "${CTX}" -maxdepth 1 -type f -exec touch -d @0 {} +
docker build -q --provenance=false --sbom=false \
  --build-arg "SANDBOX_IMAGE=${REGISTRY}/openshell-sandbox:${OPENSHELL_VERSION}" \
  --build-arg "SUPERVISOR_IMAGE=${REGISTRY}/openshell-supervisor:${OPENSHELL_VERSION}" \
  -t "${REGISTRY}/openshell-substrate-runtime:${OPENSHELL_VERSION}" "${CTX}" >/dev/null
docker push -q "${REGISTRY}/openshell-substrate-runtime:${OPENSHELL_VERSION}" >/dev/null

cat > "${OUT}/runtime.env" <<ENV
RUNTIME_IMAGE=$(digest "${REGISTRY}/openshell-substrate-runtime:${OPENSHELL_VERSION}")
ENV
cat "${OUT}/runtime.env"
