#!/usr/bin/env bash
# Build the feature-test supervisor image + push to the kind-registry.
# Expects to run on a Linux host with cargo, docker, and access to the
# kind-registry at localhost:5001. Prints the resulting digest, which
# `run.sh` reads via `IMAGE_DIGEST=$(./build-image.sh)`.
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  if [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$PATH:$HOME/.cargo/bin"
  else
    echo "[build-image] cargo not found; install rustup or set PATH" >&2
    exit 1
  fi
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$HERE/../.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$CRATE_ROOT/target}"

REGISTRY="${KIND_REGISTRY:-localhost:5001}"
IMAGE_TAG="${IMAGE_TAG:-oshl-feature-test:latest}"
IMAGE_REF="$REGISTRY/$IMAGE_TAG"

echo "[build-image] cargo build --release --bin openshell-sandbox-substrate" >&2
( cd "$CRATE_ROOT" && cargo build --release --bin openshell-sandbox-substrate ) >&2

BUILD_CTX="$(mktemp -d)"
trap 'rm -rf "$BUILD_CTX"' EXIT
cp "$HERE/Dockerfile"                                "$BUILD_CTX/Dockerfile"
cp "$HERE/data.yaml"                                 "$BUILD_CTX/data.yaml"
cp "$HERE/test-workload.sh"                          "$BUILD_CTX/test-workload.sh"
cp "$HERE/policy.rego"                               "$BUILD_CTX/policy.rego"
cp "$TARGET_DIR/release/openshell-sandbox-substrate" "$BUILD_CTX/openshell-sandbox-substrate"

echo "[build-image] docker build $IMAGE_REF" >&2
docker build -t "$IMAGE_REF" "$BUILD_CTX" >&2

echo "[build-image] docker push $IMAGE_REF" >&2
docker push "$IMAGE_REF" >&2

DIGEST=$(docker inspect --format='{{index .RepoDigests 0}}' "$IMAGE_REF")
echo "$DIGEST"
