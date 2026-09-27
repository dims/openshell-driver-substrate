# Harness

Everything needed to reproduce a working OpenShell-on-Substrate run that is
not the driver itself. See the [root README](../README.md) for the walkthrough.

| Path | What |
|---|---|
| `bootstrap-gen/` | Mints the Ed25519/JWT/TLS bundle the sandbox and supervisor require. A workspace member, so it builds against the same OpenShell rev as the driver. |
| `manifests/` | The WorkerPool. Render with `scripts/render.sh`. The workload template is `examples/helpdesk`. |
| `scripts/render.sh` | Renders a `.tmpl` from the environment; refuses to run with a variable unset. |

`scripts/render.sh` needs `envsubst` (gettext). It ships with most Linux
distros; on macOS it comes from `brew install gettext`.
