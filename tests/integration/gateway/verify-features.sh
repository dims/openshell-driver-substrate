#!/usr/bin/env bash
# Per-feature §7b verification, post-run-gateway-integration.sh. Grep
# through the evidence directory for each of the five features and emit a
# PASS/FAIL/NOT-OBSERVED summary plus the relevant log excerpts.
#
# Usage: verify-features.sh /tmp/oshl-v3-<TS>
set -uo pipefail   # grep with no matches returns 1, which is expected here

EVIDENCE_DIR="${1:-}"
if [ -z "$EVIDENCE_DIR" ] || [ ! -d "$EVIDENCE_DIR" ]; then
  echo "usage: $0 <evidence-dir>" >&2
  exit 2
fi

GW_LOG="$EVIDENCE_DIR/gateway-pod.log"
SV_LOG="$EVIDENCE_DIR/supervisor-tail.log"
OUT="$EVIDENCE_DIR/features.md"

if [ ! -f "$GW_LOG" ] || [ ! -f "$SV_LOG" ]; then
  echo "[verify] missing evidence files; expected $GW_LOG + $SV_LOG" >&2
  exit 2
fi

verdict() {
  # $1: feature ID
  # $2: feature label
  # $3: PASS / FAIL / NOT-OBSERVED
  # $4: short reason
  # $5: log excerpt path (optional)
  printf "\n### %s — %s\n\n" "$1" "$2"
  printf "**Status:** %s\n\n" "$3"
  printf "**Reason:** %s\n\n" "$4"
  if [ -n "${5:-}" ] && [ -f "$5" ]; then
    printf "**Evidence (\`%s\`):**\n\n" "$(basename "$5")"
    printf '```\n'
    head -20 "$5"
    printf '\n```\n'
  fi
}

{
  echo "# §7b verification — $(basename "$EVIDENCE_DIR")"
  echo
  echo "Evidence: \`$EVIDENCE_DIR\`"

  # ─── F1 Settings poll ─────────────────────────────────────────────────
  F1_HITS=$(grep -cE "GetSandboxConfig|GetGatewayConfig|poll_settings|settings poll" "$GW_LOG" "$SV_LOG" 2>/dev/null | awk -F: '{s+=$NF} END {print s+0}')
  F1_TAIL="$EVIDENCE_DIR/f1-settings-poll.tail"
  grep -E "GetSandboxConfig|GetGatewayConfig|poll_settings|settings poll|fetch.*settings" \
    "$GW_LOG" "$SV_LOG" 2>/dev/null > "$F1_TAIL" || true
  if [ "$F1_HITS" -gt 0 ]; then
    verdict "F1" "Settings poll" "PASS" "$F1_HITS log line(s) reference Settings RPCs." "$F1_TAIL"
  else
    verdict "F1" "Settings poll" "NOT-OBSERVED" "No Settings RPC log lines in the evidence window."
  fi

  # ─── F2 Inference routing ─────────────────────────────────────────────
  # GetInferenceBundle is served on /openshell.inference.v1.Inference/.
  F2_HITS=$(grep -cE "GetInferenceBundle|Fetching inference|inference bundle|inference\\.local|inference route|openshell\\.inference" "$GW_LOG" "$SV_LOG" 2>/dev/null | awk -F: '{s+=$NF} END {print s+0}')
  F2_TAIL="$EVIDENCE_DIR/f2-inference-routing.tail"
  grep -E "GetInferenceBundle|Fetching inference|inference bundle|inference\\.local|inference route|openshell\\.inference" \
    "$GW_LOG" "$SV_LOG" 2>/dev/null > "$F2_TAIL" || true
  if [ "$F2_HITS" -gt 0 ]; then
    verdict "F2" "Inference routing" "PASS" "$F2_HITS log line(s) reference Inference RPCs." "$F2_TAIL"
  else
    verdict "F2" "Inference routing" "NOT-OBSERVED" "No Inference RPC log lines in the evidence window."
  fi

  # ─── F3 Log push ──────────────────────────────────────────────────────
  F3_HITS=$(grep -cE "PushSandboxLogs|log_push|log push|push_logs" "$GW_LOG" "$SV_LOG" 2>/dev/null | awk -F: '{s+=$NF} END {print s+0}')
  F3_TAIL="$EVIDENCE_DIR/f3-log-push.tail"
  grep -E "PushSandboxLogs|log_push|log push|push_logs" \
    "$GW_LOG" "$SV_LOG" 2>/dev/null > "$F3_TAIL" || true
  if [ "$F3_HITS" -gt 0 ]; then
    verdict "F3" "Log push" "PASS" "$F3_HITS log line(s) reference PushSandboxLogs." "$F3_TAIL"
  else
    verdict "F3" "Log push" "NOT-OBSERVED" "No PushSandboxLogs log lines."
  fi

  # ─── F4 SSH attach (manual; not auto-exercised) ───────────────────────
  verdict "F4" "SSH attach via RelayStream" "DEFERRED" \
    "Not exercised by run-gateway-integration.sh — would require a separate gateway-side SSH client. Wiring is present in templates (supervisor has --openshell-endpoint); the harness can be extended later."

  # ─── F5 Cross-sandbox IDOR guard (deferred) ───────────────────────────
  verdict "F5" "Cross-sandbox identity guard" "DEFERRED" \
    "Verification requires two simultaneous actors with mismatched tokens. Single-actor harness only; F5 verification needs a multi-actor variant."

  # ─── Bonus: did the supervisor actually connect? ──────────────────────
  CONN_HITS=$(grep -cE "Server listening|connected to gateway|openshell-gateway\\..*svc|Authorization.*Bearer" "$GW_LOG" "$SV_LOG" 2>/dev/null | awk -F: '{s+=$NF} END {print s+0}')
  if [ "$CONN_HITS" -gt 0 ]; then
    printf "\n### Bonus — supervisor↔gateway channel\n\n**Status:** %s\n\n%d log line(s) suggest supervisor reached the gateway.\n" \
      "OK" "$CONN_HITS"
  else
    printf "\n### Bonus — supervisor↔gateway channel\n\n**Status:** %s\n\nNo evidence of channel establishment.\n" \
      "UNCLEAR"
  fi

  echo
  echo "## Raw files"
  echo
  echo "- gateway pod log: \`$GW_LOG\` ($(wc -l <"$GW_LOG") lines)"
  echo "- supervisor tail: \`$SV_LOG\` ($(wc -l <"$SV_LOG") lines)"

} > "$OUT"

cat "$OUT"
echo
echo "[verify] written to $OUT" >&2
