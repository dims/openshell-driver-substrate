#!/usr/bin/env bash
# Generate (or reuse) the Ed25519 JWT signing material the gateway needs
# and render the gateway-secret.yaml manifest to stdout.
#
# Idempotent: if the keys already exist at $JWT_DIR, they are reused; if
# not, fresh ones are generated. The rendered YAML is printed to stdout
# so it can be piped directly into `kubectl apply`:
#
#   ./generate-jwt-keys.sh | kubectl apply -f -
#
# The private key never enters the repo — only the template
# (gateway-secret.yaml.template, placeholders only) is tracked.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="${TEMPLATE:-$HERE/gateway-secret.yaml.template}"
JWT_DIR="${OPENSHELL_JWT_DIR:-/tmp}"
KID_PREFIX="${KID_PREFIX:-openshell-poc}"

SIGNING_KEY="$JWT_DIR/openshell-jwt-signing.pem"
PUBLIC_KEY="$JWT_DIR/openshell-jwt-public.pem"
KID_FILE="$JWT_DIR/openshell-jwt-kid"

if [ ! -f "$SIGNING_KEY" ] || [ ! -f "$PUBLIC_KEY" ] || [ ! -f "$KID_FILE" ]; then
  echo "[generate-jwt-keys] minting fresh Ed25519 keypair at $JWT_DIR" >&2
  openssl genpkey -algorithm ED25519 -out "$SIGNING_KEY" 2>/dev/null
  openssl pkey -in "$SIGNING_KEY" -pubout -out "$PUBLIC_KEY" 2>/dev/null
  printf '%s-%s' "$KID_PREFIX" "$(date +%s)" > "$KID_FILE"
  chmod 0600 "$SIGNING_KEY"
else
  echo "[generate-jwt-keys] reusing existing keys at $JWT_DIR" >&2
fi

# Render — base64 with no line breaks so each k8s data field is a single
# line. `base64 -w 0` works on GNU coreutils; macOS needs the explicit
# -i + tr fallback.
b64() {
  if base64 --help 2>&1 | grep -q -- '-w'; then
    base64 -w 0 < "$1"
  else
    base64 -i "$1" | tr -d '\n'
  fi
}

SIGNING_B64="$(b64 "$SIGNING_KEY")"
PUBLIC_B64="$(b64 "$PUBLIC_KEY")"
KID_B64="$(b64 "$KID_FILE")"

sed -e "s|__SIGNING_PEM_B64__|$SIGNING_B64|" \
    -e "s|__PUBLIC_PEM_B64__|$PUBLIC_B64|" \
    -e "s|__KID_B64__|$KID_B64|" \
    "$TEMPLATE"
