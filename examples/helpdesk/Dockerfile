# Sandbox image with the Python workload baked in. The workload runs in the
# sandbox container's filesystem, so python3 lives here, not in the supervisor
# image. openshell-sandbox is a static musl binary, so any base works. The
# supervisor rides along so one entry process can start, stop and replace it.
ARG SANDBOX_IMAGE
ARG SUPERVISOR_IMAGE
FROM ${SANDBOX_IMAGE} AS sandbox
FROM ${SUPERVISOR_IMAGE} AS supervisor
FROM python:3.12-slim@sha256:b18b261e1b4974b4f321451ad5251ded85abb22c2b8b262da527dda1d031ab30
COPY --from=sandbox /openshell-sandbox /openshell-sandbox
COPY --from=supervisor /openshell-supervisor /openshell-supervisor
COPY agent.py relay.py sandbox-entry.sh /opt/helpdesk/
ADD bootstrap.tar /
# The sandbox user owns /etc so sandbox-entry.sh can point the resolver at the
# sandbox's DNS relay without root; the workload's Landlock policy keeps /etc
# read-only for the workload. No setuid binary stays: nothing here needs one.
RUN chown 65532:65532 /etc && find / -xdev -perm /6000 -type f -exec chmod a-s {} +
USER 65532:65532
ENTRYPOINT ["/opt/helpdesk/sandbox-entry.sh", "/openshell-sandbox"]
# The gateway path sends no command, so the image names its own bootstrap.
CMD ["--bootstrap", "/.openshell/channel/sandbox/bootstrap.json", "--log-level", "info"]
