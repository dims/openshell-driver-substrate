# The sandbox consumes (unlinks) its bootstrap after reading it, so the file
# has to sit on the writable rootfs rather than a read-only image volume.
# Substrate discards image file ownership, so the tree is world-writable
# instead of owned by 65532 -- modes are preserved, ownership is not.
# Build bootstrap.tar with: chmod 0777 the dirs, 0666 the files, then tar -cf.
ARG SANDBOX_IMAGE
FROM ${SANDBOX_IMAGE}
ADD bootstrap.tar /
USER 65532:65532
ENTRYPOINT ["/openshell-sandbox"]
# The gateway sends no command, so the image must name its own bootstrap.
CMD ["--bootstrap", "/.openshell/channel/sandbox/bootstrap.json", "--log-level", "info"]
