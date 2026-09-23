#!/usr/bin/env bash
# Bake a minted credential set into the sandbox image.
#
# The sandbox consumes its bootstrap, unlinking it after reading, so a read-only
# image volume fails with "Read-only file system" and the files have to sit on
# the writable rootfs. Substrate discards image file ownership, so --chown has
# no effect and the tree is made world-writable so the unlink succeeds; modes
# are preserved even though ownership is not.
#
# The same tar is left next to the credentials for an image that bakes its own
# workload beside the bootstrap (examples/helpdesk).
set -o errexit -o nounset -o pipefail

[[ $# -eq 3 ]] || {
  echo "usage: $0 <credential-dir> <sandbox-image> <registry>" >&2
  echo "  credential-dir  output of bootstrap-gen (bootstrap.json, server.*, auth.json, ...)" >&2
  exit 1
}
CREDS=$1
SANDBOX_IMAGE=$2
REGISTRY=$3
WORK=$(mktemp -d)
trap 'rm -rf "${WORK}"' EXIT

for f in bootstrap.json server.crt server.key; do
  [[ -f "${CREDS}/${f}" ]] || { echo "missing ${CREDS}/${f}" >&2; exit 1; }
done

mkdir -p "${WORK}/bake/.openshell/channel/sandbox"
cp "${CREDS}"/{bootstrap.json,server.crt,server.key} "${WORK}/bake/.openshell/channel/sandbox/"
find "${WORK}/bake/.openshell" -type d -exec chmod 0777 {} +
find "${WORK}/bake/.openshell" -type f -exec chmod 0666 {} +
tar -cf "${CREDS}/bootstrap.tar" -C "${WORK}/bake" .

mkdir -p "${WORK}/ctx"
cp "${CREDS}/bootstrap.tar" "$(dirname "$0")/../images/sandbox-with-bootstrap/Dockerfile" "${WORK}/ctx/"
docker build -q --build-arg "SANDBOX_IMAGE=${SANDBOX_IMAGE}" \
  -t "${REGISTRY}/openshell-sandbox-baked:dev" "${WORK}/ctx" >/dev/null
docker push -q "${REGISTRY}/openshell-sandbox-baked:dev" >/dev/null

echo "SANDBOX_BAKED_IMAGE=$(docker inspect --format='{{index .RepoDigests 0}}' "${REGISTRY}/openshell-sandbox-baked:dev")"
echo "BOOTSTRAP_TAR=${CREDS}/bootstrap.tar"
