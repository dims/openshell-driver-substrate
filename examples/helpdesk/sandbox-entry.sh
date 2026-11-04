#!/bin/sh
# The sandbox container's first process, uid 65532 with no capability. It
# points the workload's resolver at the sandbox's DNS relay (Substrate writes
# the actor gateway into the base layer on every activation; the image's /etc
# belongs to the sandbox user, and the edit rides in the snapshot), starts the
# supervisor, waits for its listener, and runs openshell-sandbox under
# no_new_privs, which the runtime does not set and prctl lets any process set
# for itself. It stays PID 1 so it can reap and replace either child.
set -e
printf 'nameserver 127.0.0.53\n' > /etc/resolv.conf.sandbox
mv -f /etc/resolv.conf.sandbox /etc/resolv.conf
files=/opt/supervisor-files/supervisor
OPENSHELL_ADMITTED_ISOLATION_BACKEND=openshell-sandbox /openshell-supervisor \
  --role isolation-backend --sandbox-id test-sandbox-1 \
  --backend-descriptor-file "$files/runtime-descriptor.json" --auth-bundle-file "$files/auth.json" \
  --policy-rules "$files/policy.rego" --policy-data "$files/data.yaml" \
  --log-level info -- python3 /opt/helpdesk/agent.py &
i=0
until grep -q ' 0211FEA9:4571 [0-9A-F:]* 0A ' /proc/net/tcp; do  # 169.254.17.2:17777 (little-endian hex) in LISTEN
  i=$((i + 1))
  [ "$i" -lt 240 ] || { echo "sandbox-entry: no supervisor listener on 169.254.17.2:17777" >&2; exit 1; }
  sleep 0.5
done
setpriv --no-new-privs -- "$@" &
wait
