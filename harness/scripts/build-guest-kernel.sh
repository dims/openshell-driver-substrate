#!/usr/bin/env bash
# Build a kata guest kernel with CONFIG_CROSS_MEMORY_ATTACH=y.
#
# Stock kata ships it off, so process_vm_readv returns ENOSYS in the guest.
# OpenShell treats ENOSYS as a denial and falls back to /proc/<pid>/mem, which
# a non-root task cannot open once it is nondumpable -- so the sandbox fails
# its seccomp-notification gate with a misleading EACCES.
#
# The same change as a one-line fragment is on
#   https://github.com/dims/kata-containers/tree/enable-cross-memory-attach
#
# Build deps: libelf-dev libssl-dev flex bison bc
set -o errexit -o nounset -o pipefail

KATA_VER="${KATA_VER:-4.1.0}"
ARCH="${ARCH:-x86_64}"
WORKDIR="${WORKDIR:-$(mktemp -d)}"

echo ">> cloning kata ${KATA_VER} into ${WORKDIR}"
git clone --depth 1 --branch "${KATA_VER}" \
  https://github.com/kata-containers/kata-containers.git "${WORKDIR}/kata-containers"
cd "${WORKDIR}/kata-containers"

cat > tools/packaging/kernel/configs/fragments/common/cross_memory_attach.conf <<'FRAG'
CONFIG_CROSS_MEMORY_ATTACH=y
FRAG

cd tools/packaging/kernel
./build-kernel.sh -a "${ARCH}" setup
./build-kernel.sh -a "${ARCH}" build

KDIR="$(find . -maxdepth 1 -type d -name 'kata-linux-*' | head -1)"
echo ">> vmlinux: ${PWD}/${KDIR}/vmlinux"
sha256sum "${KDIR}/vmlinux"
echo ">> verify the option actually landed:"
grep CONFIG_CROSS_MEMORY_ATTACH "${KDIR}/.config"
echo
echo "Do NOT trust 'strings vmlinux | grep process_vm_readv' to check this --"
echo "the syscall table emits a weak alias to sys_ni_syscall either way."
