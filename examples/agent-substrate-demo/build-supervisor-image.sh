#!/usr/bin/env bash
# Build the patched openshell-sandbox supervisor image for the Substrate demo
# and push it to the local kind-registry. Prints the resulting digest, which
# the operator pastes into manifests/supervisor-template.yaml in place of
# REPLACE_WITH_DIGEST_REFERENCE.
#
# Prereqs:
#   - the worktree on this branch (chore/gvisor-degraded-netns)
#   - cargo + Rust toolchain that builds openshell-sandbox
#   - docker daemon with access to localhost:5001 (kind-registry host port)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel)"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
REGISTRY="${KIND_REGISTRY:-localhost:5001}"
IMAGE="${IMAGE:-openshell-sandbox-m0:degraded}"

cd "$ROOT"
echo "[1/4] cargo build -p openshell-sandbox"
RUSTC_WRAPPER= cargo build -p openshell-sandbox

echo "[2/4] assemble Docker build context in $HERE"
cp "$TARGET/debug/openshell-sandbox" "$HERE/openshell-sandbox"
cp "$ROOT/crates/openshell-sandbox/data/sandbox-policy.rego" "$HERE/policy.rego"

echo "[3/4] docker build $IMAGE"
docker build -t "$IMAGE" "$HERE"

echo "[4/4] push to $REGISTRY/$IMAGE"
docker tag "$IMAGE" "$REGISTRY/$IMAGE"
docker push "$REGISTRY/$IMAGE" | tail -2

DIGEST=$(curl -sI \
  -H "Accept: application/vnd.docker.distribution.manifest.v2+json" \
  "http://$REGISTRY/v2/${IMAGE%:*}/manifests/${IMAGE##*:}" \
  | awk -F': ' 'tolower($1)=="docker-content-digest" {print $2}' | tr -d "\r\n")

echo
echo "image:  $REGISTRY/${IMAGE%:*}@$DIGEST"
echo
echo "Update manifests/supervisor-template.yaml:"
echo "  sed -i \"s|REPLACE_WITH_DIGEST_REFERENCE|$REGISTRY/${IMAGE%:*}@$DIGEST|\" \\"
echo "    $HERE/manifests/supervisor-template.yaml"
