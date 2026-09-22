#!/usr/bin/env bash
# Stage a rebuilt guest kernel over the micro-VM asset set and repoint the
# cluster SandboxConfig at its digest. atelet content-addresses assets by
# sha256, so the new digest is what makes it re-fetch.
set -o errexit -o nounset -o pipefail
[[ $# -eq 2 ]] || { echo "usage: $0 <vmlinux> <substrate-repo>" >&2; exit 1; }
VMLINUX="$1"; SUBSTRATE="$2"
ARCH="${ARCH:-amd64}"
BUCKET="${BUCKET:-ate-snapshots}"
OUT="${SUBSTRATE}/bin/microvm-assets/${ARCH}"

cp "${VMLINUX}" "${OUT}/vmlinux"
SHA="$(sha256sum "${OUT}/vmlinux" | cut -d' ' -f1)"
echo ">> new kernel sha256: ${SHA}"

( cd "${SUBSTRATE}" && BUCKET="${BUCKET}" OUT="${OUT}" ./hack/microvm-assets/stage-to-rustfs.sh )

kubectl patch sandboxconfig microvm --type=json \
  -p "[{\"op\":\"replace\",\"path\":\"/spec/assets/${ARCH}/kata-kernel/sha256\",\"value\":\"${SHA}\"}]"
