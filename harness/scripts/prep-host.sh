#!/usr/bin/env bash
# Prepare an Ubuntu 24.04 host (amd64 or arm64) for the demo: the packages,
# Docker, Go, kind, kubectl and the Rust toolchain. Run as root. Idempotent.
#
#   sudo harness/scripts/prep-host.sh
#
# Environment: GO_VERSION and KIND_VERSION (default: the versions Substrate's
# go.mod files pin, read from upstream main).
set -o errexit -o nounset -o pipefail
shopt -s inherit_errexit
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }
export DEBIAN_FRONTEND=noninteractive
case $(uname -m) in
  x86_64) ARCH=amd64 ;;
  aarch64) ARCH=arm64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
SUBSTRATE_RAW=https://raw.githubusercontent.com/agent-substrate/substrate/main

step() { printf '\n== %s %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }

step "packages"
apt-get update -q
apt-get install -y -q build-essential pkg-config libssl-dev git curl jq gettext-base openssl ca-certificates zstd rsync >/dev/null

step "docker"
if ! command -v docker >/dev/null; then
  curl -fsSL https://get.docker.com | sh >/dev/null
fi
systemctl enable --now docker >/dev/null
docker version --format '{{.Server.Version}}'

step "go"
GO_VERSION=${GO_VERSION:-$(curl -fsSL "$SUBSTRATE_RAW/go.mod" | awk '$1 == "go" {v = $2} END {print v}')}
if ! /usr/local/go/bin/go version 2>/dev/null | grep -q "go${GO_VERSION} "; then
  curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-${ARCH}.tar.gz" -o /tmp/go.tgz
  rm -rf /usr/local/go && tar -C /usr/local -xzf /tmp/go.tgz && rm -f /tmp/go.tgz
fi
ln -sf /usr/local/go/bin/go /usr/local/bin/go
go version

step "kind and kubectl"
KIND_VERSION=${KIND_VERSION:-$(curl -fsSL "$SUBSTRATE_RAW/hack/tools/kind/go.mod" | awk '$1 == "sigs.k8s.io/kind" {print $2}')}
if ! kind version 2>/dev/null | grep -q "${KIND_VERSION}"; then
  curl -fsSL "https://kind.sigs.k8s.io/dl/${KIND_VERSION}/kind-linux-${ARCH}" -o /usr/local/bin/kind && chmod +x /usr/local/bin/kind
fi
kind version
if ! command -v kubectl >/dev/null; then
  curl -fsSL "https://dl.k8s.io/release/$(curl -fsSL https://dl.k8s.io/release/stable.txt)/bin/linux/${ARCH}/kubectl" -o /usr/local/bin/kubectl && chmod +x /usr/local/bin/kubectl
fi
kubectl version --client | head -1

step "rust"
if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
fi
ln -sf "$HOME/.cargo/bin/cargo" "$HOME/.cargo/bin/rustc" /usr/local/bin/  # on PATH for every shell
cargo --version

step "sysctls"
sysctl -q -w fs.inotify.max_user_instances=8192 fs.inotify.max_user_watches=1048576
printf 'fs.inotify.max_user_instances=8192\nfs.inotify.max_user_watches=1048576\n' > /etc/sysctl.d/90-kind.conf

step "done"
