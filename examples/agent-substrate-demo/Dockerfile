# Patched openshell-sandbox supervisor running on the OpenShell community base
# image. Built for the Agent Substrate gVisor-actor demo (see README.md).
#
# Build context expects:
#   - openshell-sandbox      (patched supervisor binary, sibling of this file)
#   - policy.rego            (Rego ruleset, sibling)
#   - data.yaml              (Rego data, sibling)
FROM ghcr.io/nvidia/openshell-community/sandboxes/base@sha256:e29a8e98c1399aa08a8c624d758d1814b6edbe0302eb570c0468a6d3e811d167

USER root

COPY openshell-sandbox /usr/local/bin/openshell-sandbox
COPY policy.rego       /etc/openshell/policy.rego
COPY data.yaml         /etc/openshell/data.yaml

RUN chmod 0755 /usr/local/bin/openshell-sandbox \
 && mkdir -p /var/log \
 && chown -R root:root /etc/openshell /var/log

ENV OPENSHELL_POLICY_RULES=/etc/openshell/policy.rego \
    OPENSHELL_POLICY_DATA=/etc/openshell/data.yaml \
    OPENSHELL_LOG_LEVEL=info \
    RUST_BACKTRACE=1

# Long-running benign command keeps the supervisor (and so the actor) alive
# until checkpoint/restore is exercised. Replace with a real workload later.
CMD ["openshell-sandbox", "--log-level", "info", "--", "/bin/sh", "-c", "sleep 86400"]
