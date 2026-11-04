# The runtime image every workload on this driver starts from: stock
# openshell-sandbox and openshell-supervisor from the release images, python
# with the launcher and the relay, the sandbox user. A workload image adds its
# files on top and keeps USER 65532; see README.md.
ARG SANDBOX_IMAGE
ARG SUPERVISOR_IMAGE
FROM ${SANDBOX_IMAGE} AS sandbox
FROM ${SUPERVISOR_IMAGE} AS supervisor
FROM python:3.12-slim@sha256:b18b261e1b4974b4f321451ad5251ded85abb22c2b8b262da527dda1d031ab30
COPY --from=sandbox /openshell-sandbox /openshell-sandbox
COPY --from=supervisor /openshell-supervisor /openshell-supervisor
COPY launcher.py relay.py /opt/openshell-substrate/
# The sandbox's channel directory belongs to the sandbox user: the launcher
# writes the bootstrap there, and openshell-sandbox unlinks it after reading.
RUN install -d -o 65532 -g 65532 -m 700 /.openshell /.openshell/channel /.openshell/channel/sandbox
# The sandbox user owns /etc so the launcher can point the resolver at the
# sandbox's DNS relay without root; the workload's Landlock policy keeps /etc
# read-only for the workload. No setuid binary stays: nothing here needs one.
RUN chown 65532:65532 /etc && find / -xdev -perm /6000 -type f -exec chmod a-s {} +
USER 65532:65532
ENTRYPOINT ["python3", "/opt/openshell-substrate/launcher.py", "sandbox"]
