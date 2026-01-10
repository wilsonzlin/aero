.PHONY: fmt lint test build

fmt:
	gofmt -w .

lint:
	go vet ./...

test:
	go test ./...

build:
	go build ./cmd/aero-webrtc-udp-relay

