# Sandbox image with the Python workload baked in. The workload runs in the
# sandbox container's filesystem, so python3 lives here, not in the supervisor
# image. openshell-sandbox is a static musl binary, so any base works.
ARG SANDBOX_IMAGE
FROM ${SANDBOX_IMAGE} AS sandbox
FROM python:3.12-slim
COPY --from=sandbox /openshell-sandbox /openshell-sandbox
COPY agent.py relay.py /opt/helpdesk/
ADD bootstrap.tar /
USER 65532:65532
ENTRYPOINT ["/openshell-sandbox"]
