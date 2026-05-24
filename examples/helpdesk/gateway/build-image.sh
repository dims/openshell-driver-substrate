#!/usr/bin/env bash
# Build + push the substrate-driven openshell-gateway image to the
# kind-registry. Builds from the dims/OpenShell M3 branch
# (integration/openshell-driver-substrate) which carries the M3.14 dispatch arm
# that statically links openshell-driver-substrate.
#
# Source-tree resolution order:
#   1. $OPENSHELL_REPO                                (operator override)
#   2. ../../../OpenShell-driver-substrate             (sibling repo, local)
#   3. ~/go/src/github.com/nvidia/OpenShell-driver-substrate (bigbox layout)
#
# Prints the resulting <repo>@sha256:<digest>.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$HERE/../../.." && pwd)"
REGISTRY="${KIND_REGISTRY:-localhost:5001}"
IMAGE_TAG="${IMAGE_TAG:-openshell-gateway:substrate}"
IMAGE_REF="$REGISTRY/$IMAGE_TAG"

if [ -n "${OPENSHELL_REPO:-}" ] && [ -d "$OPENSHELL_REPO" ]; then
  SRC="$OPENSHELL_REPO"
elif [ -d "$CRATE_ROOT/../OpenShell-driver-substrate" ]; then
  SRC="$(cd "$CRATE_ROOT/../OpenShell-driver-substrate" && pwd)"
elif [ -d "$HOME/go/src/github.com/nvidia/OpenShell-driver-substrate" ]; then
  SRC="$HOME/go/src/github.com/nvidia/OpenShell-driver-substrate"
else
  echo "[build-helpdesk-gateway] no OpenShell-driver-substrate source tree found; set OPENSHELL_REPO" >&2
  exit 1
fi

echo "[build-helpdesk-gateway] source: $SRC" >&2
echo "[build-helpdesk-gateway] image:  $IMAGE_REF" >&2

BUILD_CTX="$(mktemp -d)"
trap 'rm -rf "$BUILD_CTX"' EXIT
cp "$HERE/Dockerfile" "$BUILD_CTX/Dockerfile"

if cp -alr "$SRC" "$BUILD_CTX/OpenShell-driver-substrate" 2>/dev/null; then
  echo "[build-helpdesk-gateway] source hardlinked into build context" >&2
else
  echo "[build-helpdesk-gateway] hardlink unavailable; rsyncing source (~1 GB)" >&2
  rsync -a --exclude target --exclude .git "$SRC/" "$BUILD_CTX/OpenShell-driver-substrate/"
fi

echo "[build-helpdesk-gateway] docker build $IMAGE_REF" >&2
docker build -t "$IMAGE_REF" "$BUILD_CTX" >&2

echo "[build-helpdesk-gateway] docker push $IMAGE_REF" >&2
docker push "$IMAGE_REF" >&2

docker inspect --format='{{index .RepoDigests 0}}' "$IMAGE_REF"
