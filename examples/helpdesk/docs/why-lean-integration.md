# Why the demo needs Substrate patches, and how the image can replace them

Two ways to run the helpdesk demo exist in this repository. On [`main`](https://github.com/dims/openshell-driver-substrate/tree/main),
Substrate is patched: the commits of four open PRs on the [`lean-integration`](https://github.com/dims/substrate/tree/lean-integration) branch of `dims/substrate`. On
[`lean-zero`](https://github.com/dims/openshell-driver-substrate/tree/lean-zero), Substrate is plain upstream main from [`ed6d2a1f`](https://github.com/agent-substrate/substrate/commit/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c) and the demo image does the
same work itself. This page says why the patches are needed, what each one
does, how the image gets by without them, and what that costs.

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
  Gate 1 fails. [#1918](https://github.com/agent-substrate/substrate/pull/1918) fixes it.
- An ActorTemplate cannot set a sysctl, and the micro-VM runtime does not
  forward `Linux.Sysctl` to the kata agent, so the guest keeps
  `ip_unprivileged_port_start=1024`. Gate 5 fails. [#1904](https://github.com/agent-substrate/substrate/pull/1904) fixes it.
- `no_new_privileges` is not set on micro-VM containers; gVisor gets it from
  `runsc --allow-suid=false`. Gate 3 fails. [#1904](https://github.com/agent-substrate/substrate/pull/1904) fixes it.
- Durable-dir volumes are created root-only, so a non-root process cannot
  write `/tmp` or the shared socket directory. [#1906](https://github.com/agent-substrate/substrate/pull/1906) fixes it.
- atelet drops every capability, so after a checkpoint it cannot delete files
  a non-root process wrote inside a directory it does not own. The golden
  snapshot never gets its tag. [#1910](https://github.com/agent-substrate/substrate/pull/1910) fixes it.
- On gVisor only, the merged rootfs root is `0700`, so a non-root process
  cannot search `/`. The first commit of [#1918](https://github.com/agent-substrate/substrate/pull/1918) fixes it; the micro-VM
  guest already sees `0755`.

## [`lean-integration`](https://github.com/dims/substrate/tree/lean-integration): patch Substrate

The PR commits, cherry-picked in order onto upstream main:

| Commit | Change | PR |
|---|---|---|
| [`d4602e7f`](https://github.com/dims/substrate/commit/d4602e7f7897e120efa9c311c8b2eb596e6817e2) | imagecache reads a file from the image's layers, for `USER` | [#1918](https://github.com/agent-substrate/substrate/pull/1918) |
| [`106cfec9`](https://github.com/dims/substrate/commit/106cfec9f5c0898ea91e32425a9b54402390a063) | atelet takes the bundle path as a parameter; no behavior change | [#1918](https://github.com/agent-substrate/substrate/pull/1918) |
| [`b5c3fdc1`](https://github.com/dims/substrate/commit/b5c3fdc1e022c759ea993fa630c801a5bdac84cd) | atelet honors the image's `USER`, resolved against its passwd and group | [#1918](https://github.com/agent-substrate/substrate/pull/1918) |
| [`f2d7f787`](https://github.com/dims/substrate/commit/f2d7f7878051769a4f56d9d63f9c8e0f7ee38442) | atelet starts the process in the image's `WORKDIR` | [#1918](https://github.com/agent-substrate/substrate/pull/1918) |
| [`0b8d124a`](https://github.com/dims/substrate/commit/0b8d124ae64c2722489eec012f9ec6ee63464143) | durable-dir volumes are `0777` | [#1906](https://github.com/agent-substrate/substrate/pull/1906) |
| [`742e8f1c`](https://github.com/dims/substrate/commit/742e8f1c4ba8ecf28e14808ff89a3cda1321a353) | atelet holds `CAP_DAC_OVERRIDE` to reset actor dirs | [#1910](https://github.com/agent-substrate/substrate/pull/1910) |
| [`b47e9040`](https://github.com/dims/substrate/commit/b47e9040a6c4fe999c16e1d0957a0a85e5d76b51) | micro-VM forwards `Linux.Sysctl` to the kata agent | [#1904](https://github.com/agent-substrate/substrate/pull/1904) |
| [`dc53a35b`](https://github.com/dims/substrate/commit/dc53a35ba72437827198dc7ba921b7ca09f651e2) | micro-VM sets `net.ipv4.ip_unprivileged_port_start=0` | [#1904](https://github.com/agent-substrate/substrate/pull/1904) |
| [`9d6020f5`](https://github.com/dims/substrate/commit/9d6020f51b47968c3d0f6eb27747f4cb4c7682c7) | micro-VM sets `no_new_privileges` | [#1904](https://github.com/agent-substrate/substrate/pull/1904) |

Each PR is necessary. Leave any one out and the demo fails at beat 1 or 2:
without `CAP_DAC_OVERRIDE` the golden snapshot never gets its tag; without any
other, the sandbox never starts and every request is the router's 502. `run.sh`
scores on the log evidence (`Boundary control listener ready`, `PROC:LAUNCH`,
`OCSF NET:OPEN`, a `reply` in beats 4 and 10), so a dead sandbox behind a live
actor fails the run instead of passing it.

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
  Initiative (OCI) spec marks no path read-only, so `/proc/sys` accepts it.
  That replaces the two sysctl commits.
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

Two facts about Substrate make this possible without patching it: the
template API grants capabilities on top of a default set
([`ateapi.proto`](https://github.com/agent-substrate/substrate/blob/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c/pkg/proto/ateapipb/ateapi.proto#L1027)), and the OCI
spec builder sets neither `readonlyPaths` nor `maskedPaths`
([`ocispec.go`](https://github.com/agent-substrate/substrate/blob/ed6d2a1fc8ae8337eb055d51b0b767b023cb3b5c/internal/ocispec/ocispec.go)).

Result: the ten beats pass on plain upstream main with the same `run.sh`.

## What it costs

The sandbox container spends its first milliseconds as root with four
capabilities. `setpriv` gives them all up before the sandbox binary runs, and
the gates verify that, but a Substrate that honors `USER`, sets the sysctl
and `no_new_privileges` itself, and can clean up after a non-root process
does not need to trust a script for it. The PRs remain the right answer;
[`lean-zero`](https://github.com/dims/openshell-driver-substrate/tree/lean-zero) is what runs without them.

The entry script is specific to this demo's image. Another image needs its
own, with its own uid and shared directories.

## An OpenShell supervisor bug

About one run in six stalls once, with or without the Substrate patches: an
actor's first request to the model host after a restore reaches the
supervisor, which logs `OCSF NET:OPEN` but never `HTTP:POST`, and the
connection sits open until the app gives up. `run.sh` fails the demo when it
happens.

The cause is in OpenShell's supervisor, in
[`handle_mediated_connection`](https://github.com/NVIDIA/OpenShell/blob/d3480d2a7efab3fd0217ab67828655617b9af777/crates/openshell-supervisor-network/src/proxy.rs#L2244).
A mediated open reaches the proxy through an in-memory duplex with a
synthesized `CONNECT` header in front of the workload's own bytes. The proxy
reads that header into an 8192-byte buffer through a `BufReader` of the same
size, so one read can return the header and the request behind it, and the
CONNECT branch never looks at the buffer past the header again. When the
workload's bytes arrive before that first read, which mostly happens right
after a VM restore while the supervisor is slow to schedule, the request is
dropped.

The fix is [NVIDIA/OpenShell#3745](https://github.com/NVIDIA/OpenShell/pull/3745):
the proxy takes only the header out of the `BufReader`, so the bytes behind it
stay buffered for the relay. The pinned revision, `0.1.0-pre.8`, has the bug.
Once the fix is in a release, move the pin in `Cargo.toml` past it and rebuild
the images; the stall goes with it.
