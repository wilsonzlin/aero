.PHONY: fmt fmt-check lint vet staticcheck test build docker-build

GIT_COMMIT ?= $(shell git rev-parse HEAD 2>/dev/null || echo "")
BUILD_TIME ?= $(shell date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "")
STATICCHECK_VERSION ?= v0.5.1

fmt:
	gofmt -w .

fmt-check:
	@unformatted="$$(gofmt -l .)"; \
	if [ -n "$$unformatted" ]; then \
	  echo "Files not formatted with gofmt:"; \
	  echo "$$unformatted"; \
	  exit 1; \
	fi

vet:
	go vet ./...

lint:
	$(MAKE) vet

test:
	go test ./...

build:
	go build -trimpath -ldflags "-X main.buildCommit=$(GIT_COMMIT) -X main.buildTime=$(BUILD_TIME)" ./cmd/aero-webrtc-udp-relay

staticcheck:
	go install honnef.co/go/tools/cmd/staticcheck@$(STATICCHECK_VERSION)
	"$$(go env GOPATH)/bin/staticcheck" ./...

docker-build:
	docker build -f Dockerfile --build-arg BUILD_COMMIT="$(GIT_COMMIT)" --build-arg BUILD_TIME="$(BUILD_TIME)" .
