# OpenShell gateway image built from the dims/OpenShell M3 branch
# (integration/openshell-driver-substrate) — same multi-stage shape as
# Dockerfile.gateway but pulls in the source tree that carries the
# ComputeDriverKind::Substrate wiring landed by M3.14 (209fc46+).
#
# Build context layout (resolved by build-gateway-image.sh):
#   ./OpenShell-driver-substrate/   — Cargo workspace root
#
# Compared with Dockerfile.gateway (which builds the v3 branch and is
# substrate-bypassed), this image's `openshell-gateway` binary statically
# links openshell-driver-substrate and registers it as the
# `compute_drivers = ["substrate"]` arm in gateway.toml.

ARG RUST_VERSION=1.95.0
ARG DEBIAN_VERSION=bookworm

# ─── builder ──────────────────────────────────────────────────────────────
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        protobuf-compiler \
        libsqlite3-dev \
        pkg-config \
        libz3-dev \
        clang \
        libclang-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY OpenShell-driver-substrate /src

# Build the gateway binary only (not the supervisor — long Linux-only build).
# openshell-server transitively depends on openshell-policy -> z3-sys, whose
# bindgen build script needs libz3-dev + libclang, hence the apt deps above.
RUN cargo build --release -p openshell-server --bin openshell-gateway

# ─── runtime ──────────────────────────────────────────────────────────────
FROM debian:${DEBIAN_VERSION}-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        libsqlite3-0 \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/openshell-gateway /usr/local/bin/openshell-gateway
RUN useradd --system --uid 1000 --user-group --no-create-home openshell \
 && mkdir -p /var/openshell /etc/openshell /etc/openshell-jwt /etc/openshell-substrate \
 && chown -R openshell:openshell /var/openshell

USER openshell
ENTRYPOINT ["/usr/local/bin/openshell-gateway"]
