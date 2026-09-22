#!/usr/bin/env bash
# Render a .tmpl from the environment. Every ${VAR} must be set.
set -o errexit -o nounset -o pipefail
[[ $# -eq 1 ]] || { echo "usage: $0 <file.tmpl>" >&2; exit 1; }
envsubst < "$1"
