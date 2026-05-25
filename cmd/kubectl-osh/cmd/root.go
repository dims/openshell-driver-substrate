package cmd

import (
	"os"

	"github.com/spf13/cobra"
)

// Global flags shared across subcommands.
type globalOpts struct {
	gateway  string
	insecure bool
}

var opts = &globalOpts{}

func NewRootCmd() *cobra.Command {
	root := &cobra.Command{
		Use:   "kubectl-osh",
		Short: "kubectl plugin for OpenShell sandboxes on substrate",
		Long: `kubectl-osh is a kubectl plugin that talks to an OpenShell gateway
running on top of the substrate compute driver. It provides operator-shaped
CRUD for sandboxes, including support for substrate-specific annotations
(substrate_actor_template) that the upstream openshell CLI does not expose.

Gateway endpoint resolution (highest priority first):
  --gateway HOST:PORT
  OPENSHELL_GATEWAY env var
  (no default; you must provide one for now — auto-port-forward is on the roadmap)`,
		SilenceUsage:  true,
		SilenceErrors: true,
	}

	root.PersistentFlags().StringVar(&opts.gateway, "gateway", os.Getenv("OPENSHELL_GATEWAY"),
		"OpenShell gateway endpoint host:port. Falls back to $OPENSHELL_GATEWAY.")
	root.PersistentFlags().BoolVar(&opts.insecure, "insecure", true,
		"Skip TLS (plaintext gRPC). v0.1 default; mTLS support coming later.")

	root.AddCommand(newCreateCmd())
	root.AddCommand(newGetCmd())
	root.AddCommand(newDeleteCmd())

	return root
}
