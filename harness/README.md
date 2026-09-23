# Harness

Everything needed to reproduce a working OpenShell-on-Substrate run that is
not the driver itself. See the root README for the walkthrough.

| Path | What |
|---|---|
| `bootstrap-gen/` | Mints the Ed25519/JWT/TLS bundle the sandbox and supervisor require. Pinned to the same OpenShell rev as the driver. |
| `capability-probe/` | Go probe: uid/gid, all five capability sets, `no_new_privs`, `seccomp(NEW_LISTENER)`, and `/proc/<pid>/mem` + `process_vm_readv` before and after `PR_SET_DUMPABLE(0)`. Answers "which primitive is actually missing". |
| `images/sandbox-with-bootstrap/` | Derived sandbox image with its bootstrap baked into the writable rootfs. Needed because the sandbox unlinks its bootstrap and Substrate discards image file ownership. |
| `manifests/` | ActorTemplate and WorkerPool templates. Render with `scripts/render.sh`. |
| `scripts/` | Guest-kernel build and staging, and retargeting a node + pools onto a rebuilt substrate version. |

Build the probe with `CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -o probe .`
before `docker build`; the image is `FROM scratch`.

`scripts/render.sh` needs `envsubst` (gettext). It ships with most Linux
distros; on macOS it comes from `brew install gettext`.
