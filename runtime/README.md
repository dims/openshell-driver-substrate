# The runtime image

What runs inside every actor this driver makes, whatever the workload: the
stock OpenShell binaries and the launcher that starts them on the bundle the
gateway minted. `build.sh` mirrors the OpenShell release images for this
platform, builds the image and writes its digest to `out/runtime.env` as
`RUNTIME_IMAGE`.

```sh
REGISTRY=localhost:5001 runtime/build.sh
```

| In the image | Why |
|---|---|
| `/openshell-sandbox`, `/openshell-supervisor` | the release binaries, static musl, copied from `ghcr.io/nvidia/openshell/{sandbox,supervisor}` |
| `/opt/openshell-substrate/launcher.py` | the first process of the sandbox and supervisor containers |
| `python:3.12-slim`, with `setpriv` | the launcher is Python; `setpriv --no-new-privs` sets the flag Substrate has no field for |
| `USER 65532:65532`, `/etc` owned by that user | the identity every bundle names; the launcher rewrites `resolv.conf` without root |
| `/.openshell/channel/sandbox`, mode 700, owned by 65532 | where the sandbox's bootstrap and TLS pair are written and consumed |
| no setuid binary | `openshell-sandbox` refuses any privilege |
| `GET /healthz` on 17901 (supervisor launcher) and 17902 (sandbox launcher) | Substrate's wakeup probes; an activation completes only when they answer, and the golden snapshot is taken as soon as they do |

## A workload image

Start from the runtime image, add the workload, keep the user and the
entrypoint:

```dockerfile
ARG RUNTIME_IMAGE
FROM ${RUNTIME_IMAGE}
COPY agent.py /opt/myagent/
```

The gateway's `--policy` must list the workload's directory as read-only and
name the workload's binary in its network policy; the gateway's default policy
allows no egress and knows no `/opt`. `examples/helpdesk/data.yaml.tmpl` is a
working one. The sandbox's resolver is `127.0.0.53` with no search domains,
so the workload uses fully qualified names.

The workload is reached through the gateway: `openshell sandbox exec` runs a
command inside the sandbox over the supervisor's SSH relay, `openshell
sandbox ssh-config` prints an SSH host entry for the same relay, and
`openshell forward start <port>` forwards a local port over it (not tried
through this driver yet). Substrate's router cannot reach it: the sandbox's
network broker refuses `accept(2)` from a non-loopback peer.

## The launcher

One file, two roles, each the first process of its container, both with
`LAUNCH_URL`, the driver's launch endpoint (`harness/gateway/template.yaml.tmpl`
is the fragment the driver uses).

The supervisor role reads the actor's name and atespace from the `systemInfo`
mount at `/run/ate`, long-polls `$LAUNCH_URL/v1/launch/<atespace>/<name>`
(atenet-egress puts the actor JWT on the request), writes the supervisor's
files under its own `/tmp`, lays the sandbox's bootstrap and TLS pair out
under `/run/openshell/bundle/<generation>` on the shared durable dir, starts
`openshell-supervisor`, and publishes the generation in
`/run/openshell/current`. The sandbox role waits for that file, copies the
files into the channel directory, removes them from the shared dir and starts
`openshell-sandbox` under `setpriv --no-new-privs`. A newer generation, which
every gateway `start` and `resume` brings, replaces both processes.

The supervisor listens on `169.254.17.2:17777`, the address of every Substrate
actor, and the sandbox dials it; the launcher waits for that listener in
`/proc/net/tcp`. The same constant lives in `src/bundle.rs`.

The health endpoint answers 200 once the launcher polls. The sandbox
container also mounts the identity volume: the bundle names the actor's uid
in a `substrate.actor_uid` claim, and `openshell-sandbox` checks it against
`/run/ate/actor-uid` before it opens its listener.
