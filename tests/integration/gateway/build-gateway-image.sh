#!/usr/bin/env bash
# Build + push the openshell-gateway image to the kind-registry.
# Expects the OpenShell-v3 source tree to be available; resolves it in this
# order:
#   1. $OPENSHELL_REPO (operator-provided checkout)
#   2. ../OpenShell-v3 relative to this script's repo root
#   3. ~/go/src/github.com/nvidia/OpenShell-v3 (the rsync target on bigbox)
#
# Prints the resulting <repo>@sha256:<digest>.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$HERE/../../.." && pwd)"
REGISTRY="${KIND_REGISTRY:-localhost:5001}"
IMAGE_TAG="${IMAGE_TAG:-openshell-gateway:v3}"
IMAGE_REF="$REGISTRY/$IMAGE_TAG"

# Locate OpenShell-v3 source.
if [ -n "${OPENSHELL_REPO:-}" ] && [ -d "$OPENSHELL_REPO" ]; then
  SRC="$OPENSHELL_REPO"
elif [ -d "$CRATE_ROOT/../OpenShell-v3" ]; then
  SRC="$(cd "$CRATE_ROOT/../OpenShell-v3" && pwd)"
elif [ -d "$HOME/go/src/github.com/nvidia/OpenShell-v3" ]; then
  SRC="$HOME/go/src/github.com/nvidia/OpenShell-v3"
else
  echo "[build-gateway] no OpenShell-v3 source tree found; set OPENSHELL_REPO" >&2
  exit 1
fi

echo "[build-gateway] source: $SRC" >&2
echo "[build-gateway] image:  $IMAGE_REF" >&2

# Assemble a build context that contains the Dockerfile + the source tree.
BUILD_CTX="$(mktemp -d)"
trap 'rm -rf "$BUILD_CTX"' EXIT
cp "$HERE/Dockerfile.gateway" "$BUILD_CTX/Dockerfile"

# Use a hardlink copy where supported to avoid duplicating ~1 GB of source.
if cp -alr "$SRC" "$BUILD_CTX/OpenShell-v3" 2>/dev/null; then
  echo "[build-gateway] source hardlinked into build context" >&2
else
  echo "[build-gateway] hardlink unavailable; rsyncing source (~1 GB)" >&2
  rsync -a --exclude target --exclude .git "$SRC/" "$BUILD_CTX/OpenShell-v3/"
fi

echo "[build-gateway] docker build $IMAGE_REF" >&2
docker build -t "$IMAGE_REF" "$BUILD_CTX" >&2

echo "[build-gateway] docker push $IMAGE_REF" >&2
docker push "$IMAGE_REF" >&2

docker inspect --format='{{index .RepoDigests 0}}' "$IMAGE_REF"
