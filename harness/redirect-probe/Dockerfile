# Debian, not Alpine: the probe shells out to iptables to install the nat rule,
# and Alpine's iptables package ships only xtables-nft-multi. gVisor implements
# the legacy xtables setsockopt interface, not nftables netlink.
#
# PROBE_UID selects the image's runtime identity. Substrate's SecurityContext
# has no runAsUser, so a container's uid comes from the image and nowhere else;
# the two roles therefore need two images built from one binary.
FROM debian:bookworm-slim
ARG PROBE_UID=0
RUN apt-get update \
 && apt-get install -y --no-install-recommends iptables util-linux \
 && rm -rf /var/lib/apt/lists/*
COPY redirect-probe /redirect-probe
USER ${PROBE_UID}
ENTRYPOINT ["/redirect-probe"]
