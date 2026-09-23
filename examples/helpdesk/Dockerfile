# Sandbox image with a Python workload baked in.
#
# The workload runs in the sandbox container's filesystem, so python3 lives
# here and not in the supervisor image. openshell-sandbox is a static musl
# binary, so any base works.
FROM python:3.12-slim
COPY openshell-sandbox /openshell-sandbox
COPY agent.py /opt/helpdesk/agent.py
ADD bootstrap.tar /
USER 65532:65532
ENTRYPOINT ["/openshell-sandbox"]
