# kubectl-osh: kubectl plugin for OpenShell sandboxes.
#
# `make` builds the binary.
# `make proto` regenerates Go stubs from ../../proto/*.proto.
# `make install` puts the binary on $PATH so `kubectl osh ...` works.

BINARY  := kubectl-osh
PKG     := github.com/dims/openshell-driver-substrate/cmd/kubectl-osh
PROTO_DIR := ../../proto
GEN_DIR := pkg

.PHONY: all build proto install clean fmt vet test

all: build

build:
	go build -o $(BINARY) .

install: build
	install -m 0755 $(BINARY) $${GOBIN:-$$HOME/go/bin}/$(BINARY)

proto:
	rm -rf $(GEN_DIR)/proto
	mkdir -p $(GEN_DIR)
	protoc \
	  --proto_path=$(PROTO_DIR) \
	  --go_out=$(GEN_DIR) \
	  --go-grpc_out=$(GEN_DIR) \
	  $(PROTO_DIR)/openshell.proto \
	  $(PROTO_DIR)/sandbox.proto \
	  $(PROTO_DIR)/datamodel.proto
	# protoc emits under the full go_package path; flatten back to $(GEN_DIR)/proto.
	mv $(GEN_DIR)/$(PKG)/$(GEN_DIR)/proto $(GEN_DIR)/proto-tmp
	rm -rf $(GEN_DIR)/proto $(GEN_DIR)/github.com
	mv $(GEN_DIR)/proto-tmp $(GEN_DIR)/proto

fmt:
	gofmt -w .

vet:
	go vet ./...

test:
	go test ./...

clean:
	rm -f $(BINARY)
