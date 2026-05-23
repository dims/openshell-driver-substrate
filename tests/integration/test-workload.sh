#!/bin/bash
# Workload driver for the OpenShell-on-Substrate feature observation
# suite. Runs a sequence of probes that exercise supervisor features
# and prints findings as `[oshl-test] <key>: <value>` lines so they are
# greppable from `kubectl logs` of the worker pod.
#
# Final `sleep infinity` keeps the actor alive long enough for
# checkpoint/restore tests and for an operator to dump additional state
# from the supervisor's own log files (which live inside the actor at
# /var/log/openshell.log + /var/log/openshell-ocsf.log).

set -u

emit() { printf '[oshl-test] %s\n' "$*" >&2; }

emit "test-workload starting"
emit "uname: $(uname -a)"
emit "id: $(id)"
emit "pwd: $(pwd)"

# --- filesystem allow / deny ----------------------------------------------
emit "fs-allow-write-tmp begin"
if echo "from inside actor $(date -Iseconds)" > /tmp/oshl-allow.txt 2>&1; then
  emit "fs-allow-write-tmp: OK contents=$(cat /tmp/oshl-allow.txt)"
else
  emit "fs-allow-write-tmp: FAIL"
fi

emit "fs-deny-write-etc begin"
if echo "should-fail" > /etc/oshl-deny.txt 2>err; then
  emit "fs-deny-write-etc: WROTE (landlock not enforcing)"
  rm -f /etc/oshl-deny.txt 2>/dev/null
else
  emit "fs-deny-write-etc: blocked $(cat err 2>/dev/null)"
fi
rm -f err 2>/dev/null

# --- proxy + policy -------------------------------------------------------
emit "proxy-reachable begin"
proxy_status=$(curl -sS -o /dev/null -w '%{http_code}' \
  --max-time 8 -x http://127.0.0.1:3128 http://example.com/ 2>err \
  || echo "curl-error: $(cat err 2>/dev/null)")
emit "proxy-reachable: $proxy_status"
rm -f err

emit "proxy-denied-host begin"
denied_status=$(curl -sS -o /dev/null -w '%{http_code}' \
  --max-time 8 -x http://127.0.0.1:3128 http://forbidden.example/ 2>err \
  || echo "curl-error: $(cat err 2>/dev/null)")
emit "proxy-denied-host: $denied_status"
rm -f err

emit "direct-bypass begin"
direct_status=$(curl -sS -o /dev/null -w '%{http_code}' \
  --max-time 8 http://example.com/ 2>err \
  || echo "curl-error: $(cat err 2>/dev/null)")
emit "direct-bypass: $direct_status"
rm -f err

# --- supervisor log files inside the actor -------------------------------
# tracing-appender rotates daily: filenames are openshell.YYYY-MM-DD.log
emit "supervisor-log begin"
shopt -s nullglob
sup_logs=(/var/log/openshell.*.log)
if [ ${#sup_logs[@]} -gt 0 ]; then
  emit "supervisor-log-files: ${sup_logs[*]}"
  for f in "${sup_logs[@]}"; do
    emit "supervisor-log-size: $f $(wc -c < "$f")"
  done
  emit "supervisor-log-tail:"
  tail -20 "${sup_logs[-1]}" 2>&1 | sed 's/^/[oshl-test:log] /' >&2
else
  emit "supervisor-log: missing under /var/log/openshell.*.log; ls below"
  ls -la /var/log/ 2>&1 | sed 's/^/[oshl-test:varlog] /' >&2
fi

emit "ocsf-log begin"
ocsf_logs=(/var/log/openshell-ocsf.*.log)
if [ ${#ocsf_logs[@]} -gt 0 ]; then
  for f in "${ocsf_logs[@]}"; do
    emit "ocsf-log-size: $f $(wc -c < "$f")"
  done
  emit "ocsf-log-tail:"
  tail -10 "${ocsf_logs[-1]}" 2>&1 | sed 's/^/[oshl-test:ocsf] /' >&2
else
  emit "ocsf-log: missing under /var/log/openshell-ocsf.*.log"
fi
shopt -u nullglob

emit "test-workload done; sleeping forever for state-preservation tests"
exec sleep infinity
