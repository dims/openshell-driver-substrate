# Upstream changes

What this repository needs from its upstreams beyond their `main` branches,
where each change stands, and what to do when it lands.

## agent-substrate/substrate

The demo runs against upstream `main`; no branch of `dims/substrate` is
needed. The changes it once carried are merged: image `USER` and `WORKDIR`
([#2392](https://github.com/agent-substrate/substrate/pull/2392)), writable
durable dirs ([#1906](https://github.com/agent-substrate/substrate/pull/1906)),
`CAP_DAC_OVERRIDE` for atelet ([#1910](https://github.com/agent-substrate/substrate/pull/1910)),
sysctl forwarding and the guest's privileged-port limit on micro-VM
([#1904](https://github.com/agent-substrate/substrate/pull/1904)), and the node
state root rename ([#1926](https://github.com/agent-substrate/substrate/pull/1926)).

One gap remains. [#1904](https://github.com/agent-substrate/substrate/pull/1904)
merged without its commit that set `no_new_privileges` on every micro-VM
container, and `SecurityContext` has no field for it, so OpenShell's third
qualification gate (`no_new_privs == 1`) cannot be met from a template. The
helpdesk image meets it itself: `sandbox-entry.sh` starts as root with
`SETUID`, `SETGID` and `SETPCAP` and hands off with `setpriv --no-new-privs`
to uid 65532. A change adding `securityContext.allowPrivilegeEscalation`
(Kubernetes shape; `false` sets `no_new_privileges` on both runtimes) is on
the `dims/substrate` fork as branch `allow-privilege-escalation`, pending a
pull request. When it merges, the sandbox container can declare
`allowPrivilegeEscalation: false`, keep the image's `USER 65532`, and drop the
entry script's privilege work; the DNS and listener waits in the script stay.

The vendored [`proto/ateapi.proto`](../proto/ateapi.proto) matches upstream at
the commit in [`proto/upstream-rev`](../proto/upstream-rev), which CI checks;
[`proto/README.md`](../proto/README.md) says how to refresh it.

### Related

- Every Substrate actor has the address `169.254.17.2` and its gateway at
  `169.254.17.1` (`internal/ateworker/actornet`). The boundary channel and the
  DNS arrangement in the root README rest on both.
- Substrate's egress rules name hosts, not addresses, so the demo's model
  endpoint is a Service in the cluster, reached by DNS name.

## kata-containers

No changes. The guest kernel is stock.

## NVIDIA/OpenShell

No changes carried. The driver depends on upstream unmodified
(`openshell-core`, pinned to the v0.1.7 release in `Cargo.toml`), and the
sandbox and supervisor images are the release images from
`ghcr.io/nvidia/openshell`.

Two facts about the release shape the harness. A boundary TLS listener may not
bind a loopback address, and a listener on any other address rejects a peer
whose address is its own ([#4150](https://github.com/NVIDIA/OpenShell/pull/4150),
[#4279](https://github.com/NVIDIA/OpenShell/pull/4279)), so the supervisor
listens and the sandbox dials (`RuntimeInitiatedTlsTcp`). The sandbox serves
the workload's DNS at `127.0.0.53` and refuses other nameservers, so the entry
script points `/etc/resolv.conf` there.
