#!/bin/sh
# Substrate main ignores the image USER, sets no sysctls and creates durable
# dirs root-only. Do those three things as root, then become the image's user.
set -e
if [ "$(id -u)" = 0 ]; then
  echo 0 > /proc/sys/net/ipv4/ip_unprivileged_port_start
  mkdir -p /run/openshell-supervisor-ca  # in the rootfs: only the sandbox reads it
  chmod 1777 /run/openshell /run/openshell-supervisor-ca
  exec setpriv --reuid=65532 --regid=65532 --clear-groups \
    --bounding-set=-all --inh-caps=-all --no-new-privs -- "$@"
fi
exec "$@"
