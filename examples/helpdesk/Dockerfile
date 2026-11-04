# The helpdesk image: the runtime image plus the agent and, for the run
# without a gateway, the baked bootstrap. USER and ENTRYPOINT are the runtime's.
ARG RUNTIME_IMAGE
FROM ${RUNTIME_IMAGE}
COPY agent.py /opt/helpdesk/
# The static sandbox credentials, owned by the sandbox user, into the channel
# directory; openshell-sandbox unlinks the bootstrap after reading it.
ADD bootstrap.tar /
