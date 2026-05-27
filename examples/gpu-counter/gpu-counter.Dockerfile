# Layered on the openshell-sandbox base (same BASE the helpdesk uses).
# CUDA libs aren't baked in — runsc-with-nvproxy calls
# nvidia-container-cli configure at sandbox start to inject libcuda.so
# etc. from the host.
ARG BASE
FROM ${BASE}

USER root
COPY gpu-counter-data.yaml /etc/openshell/data.yaml
RUN  mkdir -p /opt/gpu-counter
COPY gpu-counter-agent.py  /opt/gpu-counter/agent.py
RUN  chmod 0644 /etc/openshell/data.yaml \
  && chmod 0755 /opt/gpu-counter/agent.py \
  && chown -R root:root /etc/openshell /opt/gpu-counter
