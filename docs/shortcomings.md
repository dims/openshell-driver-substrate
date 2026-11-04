# Shortcomings

What this implementation does not do, does only in the harness, or does
because OpenShell v0.1.7 or Substrate leaves it no other way. Each item names
the file that decides it. The last two sections say what needs a change
upstream and what to fix first here.

## What a gateway user loses

1. **Only an image built on the runtime image runs.** The sandbox container
   must carry what `runtime/README.md` lists: both OpenShell binaries,
   `python3`, `setpriv`, the launcher at its path, uid and gid 65532, `/etc`
   owned by that user, the channel directory, and a `--policy` that names the
   workload's paths and binary, because the gateway's default policy allows no
   egress and knows no `/opt` (`runtime/Dockerfile`,
   `harness/gateway/template.yaml.tmpl`, `examples/helpdesk/data.yaml.tmpl`).
   OpenShell's docker driver injects the sandbox binary into any image; this
   driver cannot. A stock OpenShell sandbox image passes
   `validate_sandbox_create` and dies at boot, which shows as Provisioning for
   up to 1800 s (Lifecycle 6).
2. **The workload identity is fixed.** `src/bundle.rs` writes uid and gid
   65532 into every bundle and never reads the spec's `workload_identity` or
   the image's `USER`. A policy's `run_as_user` has no effect, and an image
   with another user fails the sandbox's identity check inside the VM, after
   `create` returned Ok.
3. **The supervisor and relay containers run the operator's runtime image.**
   The fragment names `${RUNTIME_IMAGE}` for all three containers and
   `apply_fragment` replaces only the `sandbox` container's image
   (`src/template.rs`); `gateway-up.sh` freezes that fragment into a
   ConfigMap. Nothing checks that the request image's `openshell-sandbox` and
   the runtime image's `openshell-supervisor` are the same release. A fragment
   without a container named `sandbox` is accepted and the request's image
   lands in the first one.
4. **No GPUs, no user namespaces.** `validate_substrate_sandbox` in
   `src/lib.rs` rejects both. Substrate's `Control` API has no GPU field to
   map a request to.
5. **Spec fields dropped without a word:** the template's `labels`,
   `agent_socket_path` and `platform_config` (the docker driver rejects the
   last two), the spec's `sandbox_token` and `isolation_backend`, and, without
   a launch endpoint, `tty` and `await_main_process_attachment` while only
   `command` is refused. Resource requests are ignored and the limits size the
   whole micro-VM, not a container.
6. **No `openshell sandbox update`.** The driver advertises only
   `openshell.compute.suspend-resume`; without
   `openshell.compute.update-sandbox-configuration` the gateway refuses an
   update of image, command or environment.
7. **Two egress policies, two owners.** Every actor gets the rules of the
   driver's `egress_policy_file` (`src/lib.rs`, `create_sandbox`), written
   once under the name `default` and never updated. The sandbox policy's
   `network_policies` reach the supervisor only. A host the sandbox policy
   allows but the driver's file does not ends in a 403 from atenet-egress,
   which the workload sees as a dropped connection; `--env OPENAI_BASE_URL`
   pointing at another model host fails that way. A changed file reaches new
   actors only.
8. **One atespace for every workspace.** `ensure_workspace` creates one
   configured atespace and `delete_workspace` does nothing, because the trait
   carries workspace identity only on create. Gateway workspaces give no
   isolation on the Substrate side, and two gateways on one atespace see each
   other's actors in every list and watch.
9. **`authenticate_sandbox` is unimplemented** and
   `supports_sandbox_authentication` is false, so the gateway's compute-driver
   sandbox bootstrap authenticator stays off. `rootfs_tar_staging_dir` is
   empty, so the gateway's rootfs-tar path is off too.
10. **A sandbox command needs the launch endpoint.** Without `[launch]` the
    driver rejects `spec.command`, names every sandbox by its id, and runs it
    on whatever credentials the image carries, with the command the fragment
    puts in `WORKLOAD` (`harness/scripts/driver-up.sh`, `runtime/launcher.py`).

## Lifecycle

1. **A resume through the gateway restarts the workload.** The gateway mints
   a new generation for every `start` and `resume`; `openshell-sandbox` keeps
   its first identity in memory, checks the generation in every token and
   answers `SuspendBoundary` with Unimplemented; a dedicated supervisor has no
   takeover. The launcher therefore replaces both processes
   (`runtime/launcher.py`). Only a Substrate-side suspend keeps the
   workload's memory.
2. **A Substrate-side resume of a gateway-stopped sandbox is undone, not
   prevented.** `kubectl-ate resume`, or a connection through `kubectl-ate
   proxy actor`, which the router answers by resuming the actor, restores the
   VM; the driver reports Stopped throughout and suspends it again within a
   watch tick plus one checkpoint. In between the supervisor retries a denied
   logon. An operator who wants the actor up has to `openshell sandbox start`.
3. **A stop cut in flight settles as Stopped.** The mark goes on before
   `SuspendActor`, so the gateway's recovery after a driver restart, a
   gateway restart or a transport error sees `PodTerminating` while the
   checkpoint runs and Stopped after it. If the RPC was lost before Substrate
   acted, the watch loop suspends the still-running actor as in item 2.
4. **`start` ignores `resume`.** The gateway sends `resume=false` after a
   stop and expects a fresh boot, `resume=true` after a suspend. The driver
   restores the last checkpoint either way (`start_sandbox`), so guest state
   from before a stop comes back. Without the launch endpoint start and resume
   are the same operation.
5. **A Substrate-side suspend parks the sandbox in Provisioning.** The driver
   reports `Ready=False (DependenciesNotReady)` so the gateway keeps the
   credentials, but the gateway admits only `delete` in that phase and has no
   deadline for it. Recovery is `kubectl-ate resume`, outside the gateway.
6. **Nothing restarts a dead supervisor or sandbox.** The launcher notices a
   dead child only after its current poll returns, up to 25 s later, and then
   exits; `openshell-sandbox` kills the workload after its own 30 s recovery
   timeout. The container is gone, the actor stays `RUNNING`, Substrate
   reports it so, and the gateway waits in Provisioning on
   `SupervisorNotConnected`: 1800 s before the supervisor's first report, no
   deadline after it. Nothing but the worker pod log names the cause. The fix
   is `delete`.
7. **No capacity handling, no retries.** `ResumeActor` returns
   `ResourceExhausted` when no worker has room; `Aborted` comes back for a
   concurrent operation or a version conflict. The driver retries neither and
   does not queue. A `create` fails and the gateway compensates with
   `delete_sandbox`; a `start` fails with that error. The silent 180 s wait on
   a missing or full pool applies to the first create of a template only.
8. **`suspend` and `stop` block on the upload.** `SuspendActor` runs with
   synchronous durability (`src/lib.rs`, `checkpoint`), so the RPC lasts as
   long as the checkpoint of the whole guest memory takes to reach the object
   store.
9. **The first `create` of a new image shape blocks on the golden snapshot.**
    `ensure_ready` in `src/template.rs` waits for the boot, the wakeup probes
    and the checkpoint, up to `template_ready_timeout_secs`, inside the
    gateway's `CreateSandbox` call. A golden failure deletes the template
    while other creates may be polling the same name, which then see
    `NotFound`; a timeout leaves the template and its golden actor behind.
10. **The golden snapshot proves nothing on the gateway path.** The golden
    actor has no egress policy, so its launchers get 403 every 5 s and never
    start OpenShell; the qualification gates and the boundary attach run per
    actor after every restore. The wakeup probes only say the launchers and
    the relay are up; a probe that never answers fails the activation after
    30 s instead of snapshotting a dead container. `run.sh`'s warm-up check
    has no gateway-path equivalent.

## State and recovery

1. **The stopped mark lives with the bundle store.** It is a `.stopped` file
   next to the bundle, read when the driver starts (`src/launch.rs`). Without
   a launch endpoint it is in memory only, and a second replica would not see
   the first one's marks.
2. **One replica.** Bundles are files on a `ReadWriteOnce` volume plus a
   per-process map, the gateway's database is SQLite on another, the
   Deployment uses `Recreate`, and the launch Service selects every gateway
   pod (`harness/gateway/deployment.yaml.tmpl`). A second replica would answer
   204 or 403 for bundles it does not hold. Every rollout is downtime for the
   API; running sandboxes continue and their supervisors reconnect. The
   gateway's state dir (`XDG_STATE_HOME`) is an emptyDir: its database and
   its credential-store key are pointed at the data volume by name
   (`gateway.toml.tmpl`), and anything else it may keep there is lost on a
   restart.
3. **Little reconciliation.** The driver drops the bundles of vanished actors
   at startup and suspends a resumed actor the gateway stopped, nothing more.
   A lost gateway database leaves actors in Substrate that no one owns. A lost
   bundle store leaves running sandboxes whose launchers get 204 until the
   next `start` mints a bundle, and the driver reports them under their ids,
   which the gateway adopts as their names (it takes the name from every
   snapshot, and a start waiting at that moment aborts). The same rename
   shows for one tick between `CreateActor` and `store_bundle` in
   `create_sandbox`. Ownership is by name: any actor of a
   template named `oshl-*` is reported as a sandbox (`list_driver_actors`),
   and the driver's token holds `editor` on the whole atespace, in the same
   process that serves the launch endpoint.
4. **The watch is a poll.** `watch_sandboxes` lists every page of the
   atespace every 2 s and diffs; cost grows with the atespace, a change
   reaches the gateway up to 2 s plus one list later, and a `ListActors` that
   keeps failing is a warning in the log and silence to the gateway.
5. **No deadlines on Control RPCs.** `control_client` holds the channel lock
   across the dial and sets no connect or request timeout; `ensure_ready`'s
   polls are unbounded inside its loop. A hung `ate-api-server` pins every
   driver RPC, the watch task and every gateway lifecycle call.
6. **Long polls fan out.** One open connection per running sandbox, renewed
   every 20 s; every bundle write wakes every poller; the golden actor of
   every template polls too. A launcher restored in the middle of a poll can
   wait up to 25 s on the dead connection before it asks again (`fetch` in
   `launcher.py`); the lifecycle runs measured 2 to 4 s.
7. **Bundle store housekeeping.** One corrupt file under `state_dir` stops the
   driver from starting, `.json.tmp` leftovers are never removed, writes are
   not fsynced, and a crash between `DeleteActor` and `remove` leaves a bundle
   with live secrets that nothing reaps (`src/launch.rs`, `delete_sandbox`).

## Security

1. **The launch hop is plain HTTP** from atenet-egress to the driver's
   Service (`harness/gateway/egress-policy.yaml.tmpl`), so the actor JWT and
   the bundle cross the cluster network in the clear. Closing it takes two
   changes: the driver must serve TLS on the launch endpoint (it serves only
   HTTP, `src/main.rs`), and the egress must trust an in-cluster origin, which
   it does not today: an `https` rule re-originates against the public roots
   only (`/etc/ssl/certs/ca-certificates.crt` in Substrate's agentgateway
   egress config), so a servicedns or harness-CA certificate would not verify.
   The second part is an upstream change.
2. **A bundle is a bearer secret for the sandbox's lifetime.** The endpoint
   checks the actor JWT and compares generations; there is no one-shot
   delivery, and the bundle stays until `delete`. The driver cannot tell the
   egress from a direct caller, so anyone who captures the clear-text actor
   JWT can fetch the supervisor's gateway token, the sandbox token and the
   boundary private key from any pod for the token's 600 s. A token of another
   incarnation of the actor, the same name after a delete and a recreate, is
   refused, and the sandbox checks the bundle's uid claim against
   `/run/ate/actor-uid`. Inside the actor, any process in any container that
   can open a TCP connection to the launch host gets the same; the workload is
   kept out by OpenShell's policy, not by the driver.
3. **Bundles hold live secrets at rest.** Each file under the driver's state
   directory carries those secrets, mode 0600, unencrypted, until the sandbox
   is deleted (`src/launch.rs`, `BundleStore`).
4. **Every per-actor snapshot carries the generation's credentials.** The
   supervisor's files sit on its container's `/tmp`, a tmpfs in guest memory;
   the sandbox's key sits on the sandbox container's writable rootfs, and
   after a failed qualification gate its bootstrap stays there too. All of it
   is in every checkpoint in the object store. The golden snapshot holds none.
5. **The next generation's sandbox files wait on a shared durable dir while
   the old workload still runs.** `apply_bundle` writes `bootstrap.json`,
   `server.crt` and `server.key` under `/run/openshell/bundle` before it starts
   the new supervisor, publishes the generation once that one listens, up to
   30 s later, and the sandbox role copies and removes the files on its next
   1 s tick. Until then the previous `openshell-sandbox` and its workload run
   as the same uid in a mount namespace that holds the directory. A checkpoint
   in that window captures the files in the durable-dir tar.
6. **Both containers run as uid 65532, and the barrier is the user's.** What
   keeps the supervisor's tokens and the pending sandbox files from the
   workload is separate container filesystems, the sandbox's unlink-on-read
   and the Landlock ruleset built from the `--policy` the user passes; a
   policy that grants `/run` or `/` hands them over. The image gives `/etc` to
   that user so the launcher can rewrite `resolv.conf`; the launcher and the
   supervisor are not confined.
7. **The relay is unauthenticated.** `relay.py` exposes the workload port on
   the actor's address to anything atenet-router admits, outside OpenShell's
   own ingress, without an audit record. Its ports are the fragment's
   (`RELAY_PORT`, `WORKLOAD_PORT`), not the request's.
8. **A removed IdP key verifies for up to five minutes.** The JWKS cache is
   fetched again after that age or on an unknown `kid`, and a cold cache during
   an IdP outage holds the verifier's lock for the 10 s HTTP timeout per
   request.
9. **The harness gateway is open.** `allow_unauthenticated_users = true`, no
   NetworkPolicy on the gateway or launch Services, the whole namespace
   labelled for API access, the CLI running with `--gateway-insecure` because
   verified TLS in this CLI needs a client certificate the harness does not
   mint, the TLS and signing keys copied into the `last-applied` annotation
   by `kubectl apply`, and `ttl_secs` omitted so session tokens never expire
   (`harness/gateway/gateway.toml.tmpl`, `gateway-up.sh`). The last one
   matters: with expiring tokens, a Substrate-side suspend longer than the TTL
   ends in a locked-out supervisor and a sandbox only `delete` can clear.

## Harness and portability

1. **kind only.** `kind-up.sh` makes a single-node cluster with rustfs. GKE
   and multi-node clusters are untested in this version. Ollama keeps its
   model on an emptyDir and pulls it again after every restart.
2. **Two sandboxes at a time.** The pool has two workers of two cores and the
   fragment asks for two cores (`harness/manifests`,
   `harness/gateway/template.yaml.tmpl`). The soak runs are sequential; there
   is no load test.
3. **Build on the target architecture.** `build.sh` mirrors one platform's
   manifests and `gateway-up.sh` wraps a host-built glibc binary in an Ubuntu
   24.04 image, so a host with a newer glibc yields a driver that does not
   start. No image is published.
4. **The OpenShell pin is expensive to move.** `src/bundle.rs` and
   `harness/bootstrap-gen` build `BoundaryConfig`, `SandboxRuntimeDescriptor`
   and `SupervisorAuthBundle` by hand against rev 5d25211.
5. **The boundary address is written three times.** `169.254.17.2:17777`
   lives in `src/bundle.rs`, `harness/bootstrap-gen` and, as a hex pattern
   for `/proc/net/tcp`, in `launcher.py`; the health ports 17901 to 17903 in
   the launcher, the relay and both fragments; the mount paths and the
   `policy-dns-transparent-tcp` capability are the launcher's constants. A
   change in one place is a 120 s wait and `exit 1` with no hint.
6. **The model endpoint is baked in.** `MODEL_HOST` goes into the policy data,
   the egress rules and, without a gateway, the image's bootstrap. A change
   rebuilds the image and the templates. `kind-up.sh` installs Ollama with
   `qwen2.5:1.5b` as that endpoint.
7. **Reruns drift.** `kind-up.sh` moves the Substrate checkout and rebuilds
   `kubectl-ate` while the cluster stays on the installed revision, treats one
   existing Deployment as a finished install and never re-renders the pool;
   `build.sh` downloads `policy.rego` from GitHub with no checksum;
   `gateway-up.sh` accepts any `openshell` on `PATH` whatever its version.
8. **Demo credentials are loose.** `bootstrap-gen` writes its tokens 0644
   under `out/`, and `build.sh` bakes them into two images pushed to the
   registry. Only the demo pair trusts that key.
9. **Small fixed points.** One sandbox per actor on fixed ports; no search
   domains in the sandbox's `resolv.conf`, so workloads need fully qualified
   names; `prep-host.sh` pipes remote installers into a root shell and sets
   host sysctls.

## Operations

1. **No metrics, no health endpoint, no probe on the driver container.** A
   hung driver is not restarted; a crashed one is. Logs are the only signal.
2. **The launcher is brittle.** It reads the identity files and indexes the
   bundle JSON without guards, so a missing file or key ends the container.
   When the new supervisor never listens it exits without stopping that
   supervisor and leaves no current generation. If a newer generation arrives
   while the sandbox role is still terminating the old sandbox, the bundle
   directory it copies from is already gone. It gives `openshell-sandbox`
   10 s before SIGKILL while the sandbox gives its workload the same 10 s and
   then a forced kill and a flush, so a workload that ignores SIGTERM may
   outlive its broker and keep port 8080 from the next generation; not
   reproduced here. It reaps only its direct child.
3. **A driver restart ends the watch stream.** The gateway reopens it after
   2 s; bundles and stopped marks survive on disk.

## Tests

1. **CI runs no cluster.** `cargo test --lib` (25 pure tests), fmt, clippy,
   the proto diff, shellcheck and `py_compile` (`.github/workflows/ci.yml`).
2. **Untested in the unit tests:** `src/bundle.rs`, the launch handler
   (401, 403, 204, 200, the generation compare), `ActorJwtVerifier`, the watch
   diff, `checkpoint`'s revert of a crashed actor, the create sequence, and
   all of `launcher.py` and `relay.py`.
3. **`tests/live.rs` is ignored by default**, returns silently without its
   variables, and covers create, stop, start, list and delete without a
   gateway or a launch endpoint; its "Ready" is Substrate's `RUNNING`.
4. **The gateway path runs only by hand.** `harness/scripts/gateway-test.sh`
   drives it start to finish and `examples/helpdesk/soak.sh` the kubectl-ate
   demo, but CI has no cluster, so neither runs there.

## What needs a change upstream

- OpenShell: a new generation per `start` and `resume`, no supervisor takeover
  in dedicated mode, `SuspendBoundary` unimplemented, no deadline for
  Provisioning after Ready and no `stop` or `start` from it, Stopped and
  Suspended phases that reject every sandbox token, and no way for a
  dedicated supervisor to attach to a running sandbox.
- Substrate: no watch RPC, no labels on actors (ownership by name), templates
  never garbage-collected, no `no_new_privileges` field, `Linux.Seccomp`,
  `ApparmorProfile` and `Sysctl` not settable from a template, `Aborted` on
  concurrent operations, a golden snapshot with no readiness check, and an
  egress that verifies an `https` origin against the public roots only, so no
  in-cluster service can sit behind an `https` rule.

## What to fix first

1. Harden the launcher: guard the reads, stop what it started before it
   exits, tolerate a vanished bundle directory, and give the sandbox longer
   than it gives the workload (Operations 2).
2. A static launcher and relay on a digest-pinned image volume, so the
   workload image needs neither Python nor the runtime image (What a gateway
   user loses, item 1). It also needs a uid and gid strategy instead of the
   fixed 65532, and a way to point the sandbox's resolver at its relay that
   does not need a workload-owned `/etc`.
3. Durable lifecycle records with phases, reconciled against Substrate at
   startup and on a schedule, and fault tests that interrupt each phase,
   race a stop with a start, and recreate an actor under the same name
   (State 3).
4. OpenShell's own ingress for published services, the supervisor's app
   listener behind Substrate's router, instead of the relay (Security 7).
