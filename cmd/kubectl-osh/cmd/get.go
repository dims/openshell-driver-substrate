package cmd

import (
	"context"
	"fmt"
	"os"

	"github.com/spf13/cobra"

	"github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/internal/client"
	"github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/internal/output"
	openshellv1 "github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/pkg/proto/openshell/v1"
)

type getOpts struct {
	outputFmt string
}

func newGetCmd() *cobra.Command {
	go_ := &getOpts{}
	cmd := &cobra.Command{
		Use:   "get (sandboxes | sandbox NAME)",
		Short: "List sandboxes or fetch a single sandbox by name",
		Args:  cobra.RangeArgs(1, 2),
		RunE: func(c *cobra.Command, args []string) error {
			switch args[0] {
			case "sandboxes", "sb":
				if len(args) != 1 {
					return fmt.Errorf("usage: get sandboxes")
				}
				return runList(c.Context(), go_)
			case "sandbox":
				if len(args) != 2 {
					return fmt.Errorf("usage: get sandbox NAME")
				}
				return runGet(c.Context(), args[1], go_)
			default:
				return fmt.Errorf("unknown resource %q; expected `sandbox` or `sandboxes`", args[0])
			}
		},
	}
	cmd.Flags().StringVarP(&go_.outputFmt, "output", "o", "", "Output format: yaml|json|wide. Default: table.")
	return cmd
}

func runList(ctx context.Context, go_ *getOpts) error {
	cli, conn, err := client.Dial(ctx, opts.gateway, opts.insecure)
	if err != nil {
		return err
	}
	defer conn.Close()

	resp, err := cli.ListSandboxes(ctx, &openshellv1.ListSandboxesRequest{})
	if err != nil {
		return fmt.Errorf("ListSandboxes: %w", err)
	}
	return output.PrintSandboxList(cmdOut(), resp.GetSandboxes(), go_.outputFmt)
}

func runGet(ctx context.Context, name string, go_ *getOpts) error {
	cli, conn, err := client.Dial(ctx, opts.gateway, opts.insecure)
	if err != nil {
		return err
	}
	defer conn.Close()

	resp, err := cli.GetSandbox(ctx, &openshellv1.GetSandboxRequest{Name: name})
	if err != nil {
		return fmt.Errorf("GetSandbox %q: %w", name, err)
	}
	return output.PrintSandbox(cmdOut(), resp.GetSandbox(), go_.outputFmt)
}

func cmdOut() *os.File { return os.Stdout }
