# Agent Substrate demo

Runs the patched OpenShell `openshell-sandbox` supervisor inside an
Agent Substrate gVisor actor, then exercises a suspend/resume cycle and
verifies the child workload's monotonic-clock state survives the
checkpoint boundary.

The patches that make this work are the four commits in this branch
(`chore/gvisor-degraded-netns`). They turn previously fatal failures
(`unshare(CLONE_NEWNET)`, supervisor seccomp prelude install,
`drop_privileges` when caps are dropped, workload seccomp install) into
best-effort warnings so that the supervisor can boot under gVisor's
reduced syscall surface. **gVisor itself is the enforcing security
boundary in this configuration** -- the supervisor's in-process
restrictions are defense-in-depth and not load-bearing.

## What's here

```
agent-substrate-demo/
├── README.md                       this file
├── Dockerfile                      patched supervisor on the community base image
├── data.yaml                       Rego data tuned for the gVisor-degraded path
├── build-supervisor-image.sh       cargo build + docker build + push to kind-registry
├── dry-run.sh                      end-to-end validator (~125 s, asserts continuity)
├── cleanup.sh                      remove non-golden actors, preserve golden
└── manifests/
    └── supervisor-template.yaml    Substrate ActorTemplate (digest placeholder)
```

## Prerequisites

- Agent Substrate kind cluster with namespace `ate-openshell-m0`,
  WorkerPool `openshell-m0-pool`, and the local `kind-registry` running
  on host port 5001. See the Substrate setup notes if not already up.
- `kubectl-ate` plugin on `$PATH`.
- Rust toolchain that can build `openshell-sandbox`.
- `docker` daemon access.

## Quickstart

```bash
# 1. Build + push the supervisor image; capture the digest.
bash build-supervisor-image.sh

# 2. Patch the digest into the ActorTemplate.
sed -i "s|REPLACE_WITH_DIGEST_REFERENCE|<digest_from_step_1>|" \
  manifests/supervisor-template.yaml

# 3. Apply the template (Substrate creates the golden actor + snapshot,
#    takes ~70 s to reach STATUS_READY).
kubectl apply -f manifests/supervisor-template.yaml

# 4. Run the validator.
bash dry-run.sh

# 5. Cleanup when done.
bash cleanup.sh
```

## Known degradations under gVisor

Set explicitly so the demo is honest about its security envelope:

- Network namespace creation fails (`EPERM` on `unshare(CLONE_NEWNET)`).
  Egress is enforced only by the loopback proxy + cooperative
  `HTTP_PROXY` env injection. Non-cooperating workloads can still reach
  anything gVisor's outer network policy permits.
- Landlock filesystem sandbox unavailable. Logged as an OCSF HIGH
  finding. `landlock.compatibility: best_effort` keeps the supervisor
  alive.
- Supervisor and workload seccomp filters fail to install (`EINVAL` on
  `seccomp(SECCOMP_SET_MODE_FILTER)`). The supervisor logs and
  continues.
- `drop_privileges` is a no-op because the policy's `run_as_user`
  matches the current effective uid (root).

## Sharp edges to know about

- **Image references must be content-addressed.** `atelet` resolves bare
  tag references against Docker Hub and gets `UNAUTHORIZED`. Always use
  `localhost:5001/<name>@sha256:<digest>` in the ActorTemplate. The
  build script prints the right form.
- **Stuck `STATUS_RESUMING` actors.** If an actor's initial Resume fails
  (e.g. its image was overwritten mid-iteration), `runsc checkpoint`
  cannot snapshot a sandbox that never booted, so `suspend` returns
  `Internal` and `delete` returns `FailedPrecondition`. The cleanup
  script skips these with a warning rather than blocking. Do not run
  `kubectl ate admin debug-flush-redis` to clear them -- it wipes
  *every* actor's state, including healthy golden snapshots.
- **Worker capacity.** A `WorkerPool` allocates one slot per replica.
  Stuck actors hold slots indefinitely. If `kubectl ate resume actor`
  returns `no free workers available`, bump the replica count:
  ```bash
  kubectl -n ate-openshell-m0 patch workerpool openshell-m0-pool \
    --type=merge -p '{"spec":{"replicas":4}}'
  ```

## What this demo does NOT cover

- No gateway. The supervisor loads policy from local files baked into
  the image; no OCSF events leave the actor.
- No real egress policy. `network_policies` is empty.
- No SSH client/session into the actor.
- No real workload. The child is a `sleep 30` loop whose only purpose is
  to demonstrate that monotonic state survives Substrate's
  checkpoint/restore.
