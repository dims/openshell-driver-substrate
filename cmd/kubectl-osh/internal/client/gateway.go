package client

import (
	"context"
	"fmt"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	openshellv1 "github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/pkg/proto/openshell/v1"
)

// Dial connects to the gateway and returns a typed client. The caller
// is responsible for closing the returned ClientConn.
func Dial(ctx context.Context, endpoint string, plaintext bool) (openshellv1.OpenShellClient, *grpc.ClientConn, error) {
	if endpoint == "" {
		return nil, nil, fmt.Errorf("no gateway endpoint configured (use --gateway or $OPENSHELL_GATEWAY)")
	}
	if !plaintext {
		return nil, nil, fmt.Errorf("mTLS not yet supported in v0.1; pass --insecure for now")
	}

	dialCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()

	conn, err := grpc.DialContext(dialCtx, endpoint,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithBlock(),
	)
	if err != nil {
		return nil, nil, fmt.Errorf("dial gateway %s: %w", endpoint, err)
	}
	return openshellv1.NewOpenShellClient(conn), conn, nil
}
