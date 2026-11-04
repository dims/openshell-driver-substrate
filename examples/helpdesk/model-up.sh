#!/usr/bin/env bash
# The model endpoint for the LLM demo: Ollama in the atespace, serving
# MODEL_NAME on MODEL_PORT, which the actors reach as
# ollama.<atespace>.svc.cluster.local. Needs a cluster from kind-up.sh.
#
#   examples/helpdesk/model-up.sh
#
# Environment: ATESPACE (default ate-openshell-microvm), MODEL_NAME (default
# qwen2.5:1.5b), MODEL_PORT (default 11434).
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "${HERE}/../.." && pwd)
export ATESPACE=${ATESPACE:-ate-openshell-microvm}
export MODEL_NAME=${MODEL_NAME:-qwen2.5:1.5b}
export MODEL_PORT=${MODEL_PORT:-11434}
"${REPO}/harness/scripts/render.sh" "${HERE}/ollama.yaml.tmpl" | kubectl apply -f - >/dev/null
kubectl -n "${ATESPACE}" rollout status deploy/ollama --timeout=900s >/dev/null
echo "ollama.${ATESPACE}.svc.cluster.local:${MODEL_PORT} serves ${MODEL_NAME}"
