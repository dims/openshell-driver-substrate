# The driver as a container, from the binary cargo built on this host (same
# glibc as the build host). Context: a directory holding that binary.
FROM ubuntu:24.04@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852
RUN apt-get update -q && apt-get install -y -q --no-install-recommends ca-certificates >/dev/null && rm -rf /var/lib/apt/lists/*
COPY openshell-driver-substrate /usr/local/bin/openshell-driver-substrate
# The gateway image's user: the two share the socket's directory.
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/openshell-driver-substrate"]
