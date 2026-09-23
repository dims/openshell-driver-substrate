#!/usr/bin/env bash
# Package a minted credential set into the two images the ActorTemplate wants.
#
# The two halves are delivered differently, and it is not arbitrary. The
# supervisor only reads its files, so they ride a read-only image volume. The
# sandbox *consumes* its bootstrap, unlinking it after reading, so a read-only
# volume fails with "Read-only file system" and it has to be baked into the
# writable rootfs instead. Substrate discards image file ownership, so --chown
# has no effect and the tree is made world-writable so the unlink succeeds;
# modes are preserved even though ownership is not.
#
# The workload runs in the sandbox container's filesystem, so busybox is baked
# in alongside the bootstrap. It must be static: the sandbox image is
# distroless and a dynamic binary fails with "no such file or directory".
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

for f in bootstrap.json server.crt server.key runtime-descriptor.json auth.json; do
  [[ -f "${CREDS}/${f}" ]] || { echo "missing ${CREDS}/${f}" >&2; exit 1; }
done

# --- sandbox half: baked into the writable rootfs -------------------------
mkdir -p "${WORK}/bake/.openshell/channel/sandbox"
cp "${CREDS}"/{bootstrap.json,server.crt,server.key} "${WORK}/bake/.openshell/channel/sandbox/"

if [[ -n "${BUSYBOX:-}" ]]; then
  cp "${BUSYBOX}" "${WORK}/bake/busybox"
else
  cid=$(docker create busybox:musl)
  docker cp "${cid}:/bin/busybox" "${WORK}/bake/busybox" >/dev/null
  docker rm "${cid}" >/dev/null
fi
chmod 0755 "${WORK}/bake/busybox"

find "${WORK}/bake/.openshell" -type d -exec chmod 0777 {} +
find "${WORK}/bake/.openshell" -type f -exec chmod 0666 {} +
tar -cf "${WORK}/ctx-bootstrap.tar" -C "${WORK}/bake" .

mkdir -p "${WORK}/ctx-sandbox"
mv "${WORK}/ctx-bootstrap.tar" "${WORK}/ctx-sandbox/bootstrap.tar"
cp "$(dirname "$0")/../images/sandbox-with-bootstrap/Dockerfile" "${WORK}/ctx-sandbox/"
docker build -q --build-arg "SANDBOX_IMAGE=${SANDBOX_IMAGE}" \
  -t "${REGISTRY}/openshell-sandbox-baked:dev" "${WORK}/ctx-sandbox" >/dev/null

# --- supervisor half: a read-only image volume ----------------------------
mkdir -p "${WORK}/ctx-files/supervisor"
cp "${CREDS}"/{runtime-descriptor.json,auth.json} "${WORK}/ctx-files/supervisor/"
cat > "${WORK}/ctx-files/Dockerfile" <<'EOF'
FROM scratch
COPY supervisor/ /supervisor/
EOF
docker build -q -t "${REGISTRY}/openshell-bootstrap-files:dev" "${WORK}/ctx-files" >/dev/null

docker push -q "${REGISTRY}/openshell-sandbox-baked:dev" >/dev/null
docker push -q "${REGISTRY}/openshell-bootstrap-files:dev" >/dev/null

echo "SANDBOX_BAKED_IMAGE=$(docker inspect --format='{{index .RepoDigests 0}}' "${REGISTRY}/openshell-sandbox-baked:dev")"
echo "BOOTSTRAP_FILES_IMAGE=$(docker inspect --format='{{index .RepoDigests 0}}' "${REGISTRY}/openshell-bootstrap-files:dev")"
