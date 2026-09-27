# Why the demo needed Substrate patches, and how the image replaced them

Two ways to run the helpdesk demo exist in this repository. On
[`main`](https://github.com/dims/openshell-driver-substrate/tree/main), Substrate is patched: six commits on the
[`lean-integration`](https://github.com/dims/substrate/tree/lean-integration) branch of `dims/substrate`. On
[`lean-zero`](https://github.com/dims/openshell-driver-substrate/tree/lean-zero), Substrate is plain upstream main from
[`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c) and the demo
image does the same work itself. This page records why the patches were
needed, what each one did, how the image got by without them, and what it
cost. State as of 2026-09-26.

## What OpenShell demands of its sandbox

`openshell-sandbox` refuses to run unless the runtime passes every gate in
[`qualify_runtime`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-sandbox/src/main.rs#L158):

| # | Gate | Source |
|---|---|---|
| 1 | non-root UID **and** GID | [`main.rs#L165`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-sandbox/src/main.rs#L165) |
| 2 | all five capability sets zero: inheritable, permitted, effective, bounding, ambient | [`main.rs#L175`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-sandbox/src/main.rs#L175) |
| 3 | `no_new_privs == 1` | [`main.rs#L183`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-sandbox/src/main.rs#L183) |
| 4 | socket virtualization works | [`main.rs#L210`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-sandbox/src/main.rs#L210) |
| 5 | `net.ipv4.ip_unprivileged_port_start` is `0`, then a bind of `127.0.0.53:53` | [`main.rs#L283`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-sandbox/src/main.rs#L283) |
| 6 | Landlock ABI 3 or newer | [`main.rs#L215`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-sandbox/src/main.rs#L215) |

This table numbers only the gates the Substrate patches touch. The root
README's [gates table](../../../README.md#the-gates) lists every check in
source order; there, socket virtualization, the DNS relay bind and the
Landlock ABI are gate 7.

Beyond the gates, the sandbox writes: the Landlock probe builds a tree under
`/tmp`, and the supervisor's certificate authority (CA) is installed into a
directory the sandbox creates and then sets to `0755` itself
([`boundary_server.rs#L2700`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-sandbox/src/boundary_server.rs#L2700)).

## What upstream Substrate does not do

At [`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c):

- atelet ignores the image's `Config.User`; every container process is root.
  Gate 1 fails. Filed as [#1918](https://github.com/agent-substrate/substrate/pull/1918).
- An ActorTemplate cannot set a sysctl, and the micro-VM runtime does not
  forward `Linux.Sysctl` to the kata agent, so the guest keeps
  `ip_unprivileged_port_start=1024`. Gate 5 fails. Filed as
  [#1904](https://github.com/agent-substrate/substrate/pull/1904).
- `no_new_privileges` is not set on micro-VM containers; gVisor gets it from
  `runsc --allow-suid=false`. Gate 3 fails. Filed as [#1904](https://github.com/agent-substrate/substrate/pull/1904).
- Durable-dir volumes are created root-only, so a non-root process cannot
  write `/tmp` or the shared socket directory. Filed as [#1906](https://github.com/agent-substrate/substrate/pull/1906).
- atelet drops every capability, so after a checkpoint it cannot delete files
  a non-root process wrote inside a directory it does not own. The golden
  snapshot never gets its tag. Filed as [#1910](https://github.com/agent-substrate/substrate/pull/1910).
- On gVisor only, the merged rootfs root is `0700`, so a non-root process
  cannot search `/`. Filed as [#1905](https://github.com/agent-substrate/substrate/pull/1905), now closed; the change
  rides as the first commit of [#1918](https://github.com/agent-substrate/substrate/pull/1918). The micro-VM guest already sees `0755`.

## [`lean-integration`](https://github.com/dims/substrate/tree/lean-integration): patch Substrate

Six commits on upstream main. Each carries the same code change as its PR; the
PR versions of #1910 and of the third commit of #1904 add comment and
documentation text that the branch does not:

| Commit | Change | PR |
|---|---|---|
| [`f706c3c0`](https://github.com/dims/substrate/commit/f706c3c0b39574998c98fbe08f519b96c8461825) | atelet honors the image's `USER` | [#1918](https://github.com/agent-substrate/substrate/pull/1918) |
| [`e7e6db21`](https://github.com/dims/substrate/commit/e7e6db2197c562882000652a6d778e720c028f77) | durable-dir volumes are `0777` | [#1906](https://github.com/agent-substrate/substrate/pull/1906) |
| [`61ef7233`](https://github.com/dims/substrate/commit/61ef7233cca5af27dcd7a29cc94aa656e2831b8f) | atelet holds `CAP_DAC_OVERRIDE` to reset actor dirs | [#1910](https://github.com/agent-substrate/substrate/pull/1910) |
| [`85836a7c`](https://github.com/dims/substrate/commit/85836a7c83f961611b65ec316465adb01376f046) | micro-VM forwards `Linux.Sysctl` to the kata agent | [#1904](https://github.com/agent-substrate/substrate/pull/1904) |
| [`776d5241`](https://github.com/dims/substrate/commit/776d5241fc519751929e3563e61f566a51b4470d) | micro-VM sets `net.ipv4.ip_unprivileged_port_start=0` | [#1904](https://github.com/agent-substrate/substrate/pull/1904) |
| [`ae03ebbb`](https://github.com/dims/substrate/commit/ae03ebbbac81311af81c7bed510d539e77e0559c) | micro-VM sets `no_new_privileges` | [#1904](https://github.com/agent-substrate/substrate/pull/1904) |

Each is necessary. On 2026-09-26 every commit was left out in turn, the
cluster rebuilt from scratch, and the ten beats run, scored on the log
evidence (`Boundary control listener ready`, `PROC:LAUNCH`, `OCSF NET:OPEN`,
a `reply` in beats 4 and 10):

| Variant | Result |
|---|---|
| all six, or all six plus the rootfs mode | passes |
| without the rootfs mode | passes: micro-VM does not need it |
| without `USER`, the durable-dir mode, sysctl forwarding, the low-port sysctl or `no_new_privileges` | the sandbox never starts; every request is the router's 502 |
| without `CAP_DAC_OVERRIDE` | the golden snapshot never gets its tag |
| none of them | the sandbox never starts |

The first pass of that experiment reported eight passes out of eight, because
`run.sh` accepted the router's `bad gateway` body as an answer. It now fails
on a dead sandbox; see the troubleshooting table in the
[helpdesk README](../README.md).

## [`lean-zero`](https://github.com/dims/openshell-driver-substrate/tree/lean-zero): let the image do it

The sandbox container starts as root, does what Substrate does not, and
becomes the image's user before `openshell-sandbox` runs. Two changed template
lines (the command and an `add:` list), two volumes dropped (`tmp` and
`supervisor-ca`), and one script:

```yaml
command: ["/opt/helpdesk/sandbox-entry.sh", "/openshell-sandbox"]
securityContext:
  capabilities:
    drop: ["ALL"]
    add: ["SETUID", "SETGID", "SETPCAP", "NET_ADMIN"]
```

```sh
#!/bin/sh
set -e
if [ "$(id -u)" = 0 ]; then
  echo 0 > /proc/sys/net/ipv4/ip_unprivileged_port_start
  mkdir -p /run/openshell-supervisor-ca
  chmod 1777 /run/openshell /run/openshell-supervisor-ca
  exec setpriv --reuid=65532 --regid=65532 --clear-groups \
    --bounding-set=-all --inh-caps=-all --no-new-privs -- "$@"
fi
exec "$@"
```

Step by step, against the list above:

- `NET_ADMIN` lets the script write the sysctl. Substrate's Open Container
  Initiative (OCI) spec marks no path read-only, so `/proc/sys` accepts it. That replaces the two sysctl
  commits.
- `setpriv` drops the bounding set (this needs `SETPCAP`), clears the
  inheritable set, sets `no_new_privs`, and changes to `65532:65532`
  (`SETUID`, `SETGID`). Changing to a non-root user clears the permitted and
  effective sets, and the ambient set is empty already. Gates 1, 2 and 3 pass.
  That replaces `USER` and `no_new_privileges`.
- `/tmp` comes from the image (`python:3.12-slim` ships it as `1777`) and
  lives in the guest's writable rootfs overlay, which the snapshot captures as
  `rootfs-upper.tar`. The supervisor CA directory moved there too: nothing but
  the sandbox reads it, and its installer sets its own mode, so it cannot be a
  root-owned host directory. Only the socket directory the supervisor shares
  is still a durable dir, and the script makes it `1777`. With no non-root
  files on the host, atelet's reset needs no `CAP_DAC_OVERRIDE`. That
  replaces the durable-dir mode and the capability.

Two facts about Substrate made this possible without patching it: the
template API grants capabilities on top of a default set
([`ateapi.proto`](https://github.com/agent-substrate/substrate/blob/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c/pkg/proto/ateapipb/ateapi.proto#L1027)), and the OCI
spec builder sets neither `readonlyPaths` nor `maskedPaths`
([`ocispec.go`](https://github.com/agent-substrate/substrate/blob/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c/internal/ocispec/ocispec.go)).

Result: the ten beats pass on plain upstream main on both test hosts, 53 s,
58 s and 62 s with a fresh golden snapshot, the same strict `run.sh`.

## What it costs

The sandbox container spends its first milliseconds as root with four
capabilities. `setpriv` gives them all up before the sandbox binary runs, and
the gates verify that, but a Substrate that honors `USER`, sets the sysctl
and `no_new_privileges` itself, and can clean up after a non-root process
does not need to trust a script for it. The PRs remain the right answer;
[`lean-zero`](https://github.com/dims/openshell-driver-substrate/tree/lean-zero) is what runs today without them.

The entry script is specific to this demo's image. Another image needs its
own, with its own uid and shared directories.

## Still open: an OpenShell supervisor bug, fix pending upstream

About one run in six stalls once, on either test host, with or without the
Substrate patches: an actor's first request to the model host after a restore
reaches the supervisor, which logs `OCSF NET:OPEN` but never `HTTP:POST`, and
the connection sits open until the app gives up. `run.sh` fails the demo when
it happens.

The cause is in OpenShell's supervisor, in
[`handle_mediated_connection`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-supervisor-network/src/proxy.rs#L2244).
A mediated open reaches the proxy through an in-memory duplex with a
synthesized `CONNECT` header in front of the workload's own bytes. The proxy
reads that header into an 8192-byte buffer through a `BufReader` of the same
size, so one read can return the header and the request behind it, and the
CONNECT branch never looks at the buffer past the header again. When the
workload's bytes arrive before that first read, which mostly happens right
after a VM restore while the supervisor is slow to schedule, the request is
dropped. A one-line change, reading the header one byte at a time so the
trailing bytes stay in the `BufReader`, ran 23 of 23 demo runs clean on
2026-09-27 while the unpatched supervisor stalled 2 in 12 on the other host
in the same minutes. The bug is present at the pinned `0.1.0-pre.8` and at
upstream main `0.0.117-dev.303`; the fix goes to NVIDIA/OpenShell.
