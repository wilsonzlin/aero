.PHONY: fmt lint test build

GIT_COMMIT ?= $(shell git rev-parse HEAD 2>/dev/null || echo "")
BUILD_TIME ?= $(shell date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "")

fmt:
	gofmt -w .

lint:
	go vet ./...

test:
	go test ./...

build:
	go build -trimpath -ldflags "-X main.buildCommit=$(GIT_COMMIT) -X main.buildTime=$(BUILD_TIME)" ./cmd/aero-webrtc-udp-relay
