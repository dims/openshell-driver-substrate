#!/bin/sh
# Three things the sandbox needs that Substrate does not give a container, none
# of which takes root or a capability. The workload's resolver must be the
# sandbox's own DNS relay: Substrate writes the actor gateway into the base
# layer on every activation, so the file is replaced here (the image's /etc
# belongs to the sandbox user) and the edit rides in the snapshot. The
# supervisor's listener must be up before the sandbox dials it once. And
# no_new_privs must be set, which the runtime does not do and prctl allows any
# process to do for itself.
set -e
printf 'nameserver 127.0.0.53\n' > /etc/resolv.conf.sandbox
mv -f /etc/resolv.conf.sandbox /etc/resolv.conf
i=0
until grep -q ' 0211FEA9:4571 [0-9A-F:]* 0A ' /proc/net/tcp; do  # 169.254.17.2:17777 (little-endian hex) in LISTEN
  i=$((i + 1))
  [ "$i" -lt 240 ] || { echo "sandbox-entry: no supervisor listener on 169.254.17.2:17777" >&2; exit 1; }
  sleep 0.5
done
exec setpriv --no-new-privs -- "$@"
