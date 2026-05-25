package cmd

import (
	"context"
	"fmt"
	"strings"

	"github.com/spf13/cobra"

	"github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/internal/client"
	"github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/internal/output"
	openshellv1 "github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/pkg/proto/openshell/v1"
	sandboxv1 "github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/pkg/proto/sandbox/v1"
)

type createOpts struct {
	image       string
	template    string
	logLevel    string
	annotations []string
	labels      []string
	envVars     []string
	outputFmt   string
}

func newCreateCmd() *cobra.Command {
	co := &createOpts{}
	cmd := &cobra.Command{
		Use:   "create sandbox NAME",
		Short: "Create a sandbox via the gateway",
		Long: `Create a new sandbox by name. The gateway returns a Sandbox object
whose metadata.id is the substrate actor_id used for routing, suspend,
and pod-lookup operations through kubectl-ate.

The --template flag sets the annotation substrate_actor_template, which
the openshell-driver-substrate reads (M3.16 path) to bind the sandbox to
a pre-provisioned substrate ActorTemplate by name.`,
		Args: cobra.ExactArgs(2), // "sandbox NAME"
		RunE: func(c *cobra.Command, args []string) error {
			if args[0] != "sandbox" {
				return fmt.Errorf("only `create sandbox NAME` is supported in v0.1; got %q", args[0])
			}
			name := args[1]
			return runCreate(c.Context(), name, co)
		},
	}
	cmd.Flags().StringVar(&co.image, "image", "", "OCI image reference for the sandbox supervisor (required)")
	cmd.Flags().StringVar(&co.template, "template", "", "Pre-provisioned substrate ActorTemplate name (M3.16 annotation)")
	cmd.Flags().StringVar(&co.logLevel, "log-level", "info", "Log level inside the sandbox (info|debug|warn|error)")
	cmd.Flags().StringSliceVar(&co.annotations, "annotation", nil, "Extra annotation key=value (repeatable). substrate_actor_template is set automatically when --template is given.")
	cmd.Flags().StringSliceVar(&co.labels, "label", nil, "Label key=value on the sandbox template (repeatable)")
	cmd.Flags().StringSliceVar(&co.envVars, "env", nil, "Environment variable key=value (repeatable)")
	cmd.Flags().StringVarP(&co.outputFmt, "output", "o", "", "Output format: yaml|json|wide. Default: short confirmation line.")
	_ = cmd.MarkFlagRequired("image")
	return cmd
}

func runCreate(ctx context.Context, name string, co *createOpts) error {
	client, conn, err := client.Dial(ctx, opts.gateway, opts.insecure)
	if err != nil {
		return err
	}
	defer conn.Close()

	annotations, err := parseKV(co.annotations)
	if err != nil {
		return fmt.Errorf("--annotation: %w", err)
	}
	labels, err := parseKV(co.labels)
	if err != nil {
		return fmt.Errorf("--label: %w", err)
	}
	envVars, err := parseKV(co.envVars)
	if err != nil {
		return fmt.Errorf("--env: %w", err)
	}
	if co.template != "" {
		// Convenience: --template=X is equivalent to
		// --annotation substrate_actor_template=X. If the operator
		// passed both, the explicit annotation wins.
		if _, present := annotations["substrate_actor_template"]; !present {
			annotations["substrate_actor_template"] = co.template
		}
	}

	req := &openshellv1.CreateSandboxRequest{
		Name:   name,
		Labels: labels,
		Spec: &openshellv1.SandboxSpec{
			LogLevel:    co.logLevel,
			Environment: envVars,
			Policy:      &sandboxv1.SandboxPolicy{Version: 1},
			Template: &openshellv1.SandboxTemplate{
				Image:       co.image,
				Labels:      labels,
				Annotations: annotations,
			},
		},
	}

	resp, err := client.CreateSandbox(ctx, req)
	if err != nil {
		return fmt.Errorf("CreateSandbox: %w", err)
	}
	sb := resp.GetSandbox()

	if co.outputFmt != "" {
		return output.PrintSandbox(cmdOut(), sb, co.outputFmt)
	}
	fmt.Printf("sandbox/%s created (id=%s)\n", sb.GetMetadata().GetName(), sb.GetMetadata().GetId())
	return nil
}

// parseKV turns a slice of "k=v" strings into a map.
func parseKV(pairs []string) (map[string]string, error) {
	out := make(map[string]string, len(pairs))
	for _, p := range pairs {
		k, v, ok := strings.Cut(p, "=")
		if !ok || k == "" {
			return nil, fmt.Errorf("expected key=value, got %q", p)
		}
		out[k] = v
	}
	return out, nil
}
