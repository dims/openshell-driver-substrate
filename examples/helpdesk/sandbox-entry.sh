#!/bin/sh
# Substrate's micro-VM containers run without no_new_privs, and the sandbox's
# third gate requires it, so the container starts as root and sets it while
# becoming the image's user. The supervisor listens on the actor address and
# the sandbox dials it once, so wait for that listener first.
set -e
if [ "$(id -u)" = 0 ]; then
  # The sandbox answers the workload's DNS at 127.0.0.53 and its broker refuses
  # any other nameserver. Substrate writes the actor gateway into the base
  # layer and keeps the actor's own edits, so this one survives in the snapshot.
  printf 'nameserver 127.0.0.53\n' > /etc/resolv.conf
  i=0
  until grep -qE ':4571 [0-9A-F:]+ 0A ' /proc/net/tcp; do  # 169.254.17.2:17777 in LISTEN
    i=$((i + 1))
    [ "$i" -lt 240 ] || { echo "sandbox-entry: no supervisor listener on 169.254.17.2:17777" >&2; exit 1; }
    sleep 0.5
  done
  exec setpriv --reuid=65532 --regid=65532 --clear-groups \
    --bounding-set=-all --inh-caps=-all --no-new-privs -- "$@"
fi
exec "$@"
