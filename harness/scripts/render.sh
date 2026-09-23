#!/usr/bin/env bash
# Render a .tmpl from the environment. Refuses to run with any ${VAR} unset,
# because envsubst would silently substitute nothing.
set -o errexit -o nounset -o pipefail
[[ $# -eq 1 ]] || { echo "usage: $0 <file.tmpl>" >&2; exit 1; }
for v in $(grep -oE '\$\{[A-Z_]+\}' "$1" | tr -d '${}' | sort -u); do
  [[ -n "${!v:-}" ]] || { echo "$0: \${$v} is not set" >&2; exit 1; }
done
envsubst < "$1"
