# Harness

Everything needed to reproduce a working OpenShell-on-Substrate run that is
not the driver itself. See the root README for the walkthrough.

| Path | What |
|---|---|
| `bootstrap-gen/` | Mints the Ed25519/JWT/TLS bundle the sandbox and supervisor require. Pinned to the same OpenShell rev as the driver. |
| `images/sandbox-with-bootstrap/` | Derived sandbox image with its bootstrap baked into the writable rootfs. Needed because the sandbox unlinks its bootstrap and Substrate discards image file ownership. |
| `manifests/` | The WorkerPool and the capability-probe ActorTemplate. Render with `scripts/render.sh`. The workload template is `examples/helpdesk`. |
| `scripts/` | Credential baking, template rendering, and retargeting a node + pools onto a rebuilt substrate version. |

`scripts/render.sh` needs `envsubst` (gettext). It ships with most Linux
distros; on macOS it comes from `brew install gettext`.
