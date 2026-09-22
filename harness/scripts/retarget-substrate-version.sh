#!/usr/bin/env bash
# Move a node and every WorkerPool onto a newly built substrate version.
#
# An install creates a DaemonSet named atelet-<version> with a nodeSelector on
# ate.dev/substrate-version, and deliberately does NOT move an already-labelled
# node -- during an upgrade the operator owns that value. So a rebuild does
# nothing until the node label and every pool's nodeSelector move. A pool whose
# selector does not match sits in Pending forever.
#
# <version> is what `git describe` produced, including any -dirty suffix.
set -o errexit -o nounset -o pipefail
[[ $# -ge 2 ]] || { echo "usage: $0 <node> <version> [ateom-image]" >&2; exit 1; }
NODE="$1"; VERSION="$2"; ATEOM_IMAGE="${3:-}"

kubectl label node "${NODE}" "ate.dev/substrate-version=${VERSION}" --overwrite

kubectl get workerpool -A -o jsonpath='{range .items[*]}{.metadata.namespace}{" "}{.metadata.name}{"\n"}{end}' |
while read -r ns name; do
  [[ -n "${ns}" ]] || continue
  patch="{\"spec\":{\"template\":{\"nodeSelector\":{\"ate.dev/substrate-version\":\"${VERSION}\"}}"
  if [[ -n "${ATEOM_IMAGE}" ]]; then
    patch="${patch},\"workerImage\":\"${ATEOM_IMAGE}\""
  fi
  patch="${patch}}}"
  echo ">> ${ns}/${name}"
  kubectl patch workerpool "${name}" -n "${ns}" --type=merge -p "${patch}"
  kubectl rollout restart "deployment/${name}" -n "${ns}" || true
done
