#!/usr/bin/env bash
# Build the feature-test supervisor image + push to the kind-registry.
# Expects to run on a Linux host with cargo, docker, git, and access to
# the kind-registry at localhost:5001. Prints the resulting digest, which
# `run.sh` reads via `IMAGE_DIGEST=$(./build-image.sh)`.
#
# This script builds the patched openshell-sandbox binary from the
# OpenShell fork pinned in Cargo.toml. Resolution order:
#   1. $OPENSHELL_REPO (operator-provided sibling checkout)
#   2. ../OpenShell relative to this crate (default for monorepo-style
#      developer layouts)
#   3. clone github.com/dims/OpenShell at $OPENSHELL_REV into a temp dir
#
# Override $OPENSHELL_REV to test against a different SHA (defaults to
# the same commit pinned in Cargo.toml).
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

REGISTRY="${KIND_REGISTRY:-localhost:5001}"
IMAGE_TAG="${IMAGE_TAG:-oshl-feature-test:latest}"
IMAGE_REF="$REGISTRY/$IMAGE_TAG"

OPENSHELL_REV_DEFAULT="b6d3a35facab8e597a516ebf4ddd2989ad558ce6"
OPENSHELL_REV="${OPENSHELL_REV:-$OPENSHELL_REV_DEFAULT}"

# Locate (or clone) the OpenShell source tree.
CLONE_TMP=""
cleanup() { [ -n "$CLONE_TMP" ] && rm -rf "$CLONE_TMP"; rm -rf "$BUILD_CTX"; }
trap cleanup EXIT

if [ -n "${OPENSHELL_REPO:-}" ] && [ -d "$OPENSHELL_REPO" ]; then
  echo "[build-image] using OPENSHELL_REPO=$OPENSHELL_REPO" >&2
  SRC="$OPENSHELL_REPO"
elif [ -d "$CRATE_ROOT/../OpenShell" ]; then
  echo "[build-image] using sibling clone $CRATE_ROOT/../OpenShell" >&2
  SRC="$(cd "$CRATE_ROOT/../OpenShell" && pwd)"
else
  CLONE_TMP="$(mktemp -d)"
  echo "[build-image] cloning dims/OpenShell @ $OPENSHELL_REV into $CLONE_TMP" >&2
  git clone --quiet --filter=blob:none https://github.com/dims/OpenShell "$CLONE_TMP" >&2
  ( cd "$CLONE_TMP" && git -c advice.detachedHead=false checkout --quiet "$OPENSHELL_REV" ) >&2
  SRC="$CLONE_TMP"
fi

echo "[build-image] cargo build --release --bin openshell-sandbox (in $SRC)" >&2
( cd "$SRC" && cargo build --release --bin openshell-sandbox ) >&2

BUILD_CTX="$(mktemp -d)"
cp "$HERE/Dockerfile"                       "$BUILD_CTX/Dockerfile"
cp "$HERE/data.yaml"                        "$BUILD_CTX/data.yaml"
cp "$HERE/test-workload.sh"                 "$BUILD_CTX/test-workload.sh"
cp "$HERE/policy.rego"                      "$BUILD_CTX/policy.rego"
cp "$SRC/target/release/openshell-sandbox"  "$BUILD_CTX/openshell-sandbox"

echo "[build-image] docker build $IMAGE_REF" >&2
docker build -t "$IMAGE_REF" "$BUILD_CTX" >&2

echo "[build-image] docker push $IMAGE_REF" >&2
docker push "$IMAGE_REF" >&2

DIGEST=$(docker inspect --format='{{index .RepoDigests 0}}' "$IMAGE_REF")
echo "$DIGEST"
