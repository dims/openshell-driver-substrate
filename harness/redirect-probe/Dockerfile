# Debian, not Alpine: the probe needs the legacy iptables backend, and Alpine's
# iptables package ships only xtables-nft-multi. gVisor implements the legacy
# xtables setsockopt interface, not nftables netlink.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends iptables \
 && rm -rf /var/lib/apt/lists/*
COPY redirect-probe /redirect-probe
ENTRYPOINT ["/redirect-probe"]
