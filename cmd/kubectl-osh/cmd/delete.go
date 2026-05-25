package cmd

import (
	"context"
	"errors"
	"fmt"

	"github.com/spf13/cobra"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/internal/client"
	openshellv1 "github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/pkg/proto/openshell/v1"
)

type deleteOpts struct {
	ignoreNotFound bool
}

func newDeleteCmd() *cobra.Command {
	do := &deleteOpts{}
	cmd := &cobra.Command{
		Use:   "delete sandbox NAME [NAME...]",
		Short: "Delete one or more sandboxes by name",
		Args:  cobra.MinimumNArgs(2),
		RunE: func(c *cobra.Command, args []string) error {
			if args[0] != "sandbox" {
				return fmt.Errorf("only `delete sandbox NAME [NAME...]` is supported in v0.1; got %q", args[0])
			}
			return runDelete(c.Context(), args[1:], do)
		},
	}
	cmd.Flags().BoolVar(&do.ignoreNotFound, "ignore-not-found", false, "Treat NotFound on delete as success")
	return cmd
}

func runDelete(ctx context.Context, names []string, do *deleteOpts) error {
	cli, conn, err := client.Dial(ctx, opts.gateway, opts.insecure)
	if err != nil {
		return err
	}
	defer conn.Close()

	var firstErr error
	for _, name := range names {
		_, err := cli.DeleteSandbox(ctx, &openshellv1.DeleteSandboxRequest{Name: name})
		if err != nil {
			if do.ignoreNotFound && isNotFound(err) {
				fmt.Printf("sandbox/%s not found (ignored)\n", name)
				continue
			}
			fmt.Fprintf(cmdErr(), "sandbox/%s delete failed: %v\n", name, err)
			if firstErr == nil {
				firstErr = err
			}
			continue
		}
		fmt.Printf("sandbox/%s deleted\n", name)
	}
	return firstErr
}

func isNotFound(err error) bool {
	if err == nil {
		return false
	}
	st, ok := status.FromError(err)
	if !ok {
		return false
	}
	if st.Code() == codes.NotFound {
		return true
	}
	// Some gateway versions return InvalidArgument or FailedPrecondition
	// for "no such sandbox" — keep this guard tight to avoid swallowing
	// real config errors.
	return errors.Is(err, status.Error(codes.NotFound, ""))
}
