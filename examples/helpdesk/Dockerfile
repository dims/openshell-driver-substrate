# Sandbox image with the Python workload baked in. The workload runs in the
# sandbox container's filesystem, so python3 lives here, not in the supervisor
# image. openshell-sandbox is a static musl binary, so any base works.
ARG SANDBOX_IMAGE
FROM ${SANDBOX_IMAGE} AS sandbox
FROM python:3.12-slim
COPY --from=sandbox /openshell-sandbox /openshell-sandbox
COPY agent.py relay.py sandbox-entry.sh /opt/helpdesk/
ADD bootstrap.tar /
USER 65532:65532
ENTRYPOINT ["/openshell-sandbox"]
# The gateway path sends no command, so the image names its own bootstrap.
CMD ["--bootstrap", "/.openshell/channel/sandbox/bootstrap.json", "--log-level", "info"]
