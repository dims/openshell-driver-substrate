# OpenShell-on-Substrate feature-observation supervisor image.
#
# Self-contained build from the upstream community sandbox base. Bakes
# in:
#   * substrate-aware wrapper binary as /usr/local/bin/openshell-sandbox
#   * canonical policy.rego (from crates/openshell-sandbox/data/)
#   * test-mode data.yaml with non-empty network_policies (one allow,
#     one deny) so the OPA engine is exercised
#   * test-workload.sh as the supervisor's child workload command
#
# Operators using the feature-observation suite reference the resulting
# digest as `SUBSTRATE_LIVE_TEST_IMAGE` and set `SUBSTRATE_LIVE_TEMPLATE_NAME`
# to an ActorTemplate that invokes `/usr/local/bin/test-workload.sh`.
FROM ghcr.io/nvidia/openshell-community/sandboxes/base@sha256:e29a8e98c1399aa08a8c624d758d1814b6edbe0302eb570c0468a6d3e811d167

USER root

COPY openshell-sandbox-substrate /usr/local/bin/openshell-sandbox
COPY policy.rego                 /etc/openshell/policy.rego
COPY data.yaml                   /etc/openshell/data.yaml
COPY test-workload.sh            /usr/local/bin/test-workload.sh

RUN chmod 0755 /usr/local/bin/openshell-sandbox /usr/local/bin/test-workload.sh \
 && chown -R root:root /etc/openshell

ENV OPENSHELL_POLICY_RULES=/etc/openshell/policy.rego \
    OPENSHELL_POLICY_DATA=/etc/openshell/data.yaml \
    OPENSHELL_LOG_LEVEL=info \
    RUST_BACKTRACE=1
