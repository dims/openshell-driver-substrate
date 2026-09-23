# Minimal non-root container for reproducing the Substrate fixes.
#
# busybox MUST be statically linked. The binary in busybox:latest is
# dynamically linked, and FROM scratch has no loader, so exec fails with
# "no such file or directory" -- which reads like a missing binary and is not.
# Take it from busybox:musl.
#
#   docker create --name bb busybox:musl && docker cp bb:/bin/busybox ./busybox
#   docker rm bb && docker build -t <registry>/nonroot-probe:dev .
#
# There are no applet symlinks and no PATH here, so a script must call
# /busybox explicitly -- bare `sleep` will not resolve.
FROM scratch
COPY busybox /busybox
USER 1000:1000
ENTRYPOINT ["/busybox"]
