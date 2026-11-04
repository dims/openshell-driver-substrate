#!/usr/bin/env bash
# Render a .tmpl from the environment. Only the ${VAR}s the file names are
# substituted, and each must be set: envsubst would silently substitute nothing.
set -o errexit -o nounset -o pipefail
[[ $# -eq 1 ]] || { echo "usage: $0 <file.tmpl>" >&2; exit 1; }
vars=$(grep -oE '\$\{[A-Z_]+\}' "$1" | sort -u | tr '\n' ' ' || true)
# shellcheck disable=SC2016  # literal characters, not an expansion
for v in $(tr -d '${}' <<<"$vars"); do
  [[ -n ${!v:-} ]] || { echo "$0: \${$v} is not set" >&2; exit 1; }
done
envsubst "$vars" < "$1"
