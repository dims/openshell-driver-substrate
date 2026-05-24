# Derivative image on top of the existing oshl-feature-test supervisor.
# Replaces data.yaml with the helpdesk's policy, adds routes.yaml, and
# bakes in helpdesk-agent.py at /opt/helpdesk/agent.py.
#
# The base image already has:
#   * the patched openshell-sandbox binary (b6d3a35)
#   * OPENSHELL_POLICY_RULES / OPENSHELL_POLICY_DATA / OPENSHELL_LOG_LEVEL /
#     OPENSHELL_BEST_EFFORT_FAILURES=1 env vars
#   * python3 from the openshell-community/sandboxes/base image
ARG BASE
FROM ${BASE}

USER root

COPY helpdesk-data.yaml  /etc/openshell/data.yaml
# routes.local.yaml carries the Ollama Cloud key. It's gitignored in the
# helpdesk source folder; the build context must stage it before this RUN.
COPY routes.local.yaml   /etc/openshell/routes.yaml
RUN  mkdir -p /opt/helpdesk
COPY helpdesk-agent.py   /opt/helpdesk/agent.py

RUN chmod 0644 /etc/openshell/data.yaml /etc/openshell/routes.yaml \
 && chmod 0755 /opt/helpdesk/agent.py \
 && chown -R root:root /etc/openshell /opt/helpdesk
