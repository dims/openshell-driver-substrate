// kubectl-osh is a kubectl plugin for talking to an OpenShell gateway.
//
// Lives in the substrate-driver repo because the operator surface this
// plugin targets — pre-provisioned ActorTemplate via the M3.16
// substrate_actor_template annotation, in-cluster gateway via Service
// discovery — is substrate-specific. The upstream openshell CLI is
// developer-shaped; this plugin is operator-shaped. See the design
// rationale in the helpdesk example README.
//
// v0.1 surface:
//
//   kubectl osh create sandbox NAME --image=IMG --template=ACTOR_TEMPLATE
//   kubectl osh get sandbox NAME [-o yaml|json]
//   kubectl osh get sandboxes [-o yaml|json|wide]
//   kubectl osh delete sandbox NAME [NAME...] [--ignore-not-found]
//
// Gateway resolution: --gateway flag > OPENSHELL_GATEWAY env var. No
// auto-port-forward yet; operator runs `kubectl port-forward` manually
// against svc/openshell-gateway-substrate.
package main

import (
	"fmt"
	"os"

	"github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/cmd"
)

func main() {
	if err := cmd.NewRootCmd().Execute(); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}
