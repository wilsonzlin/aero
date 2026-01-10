##
## WebRTC UDP Relay - container image
##
## Ports:
## - TCP 8080: HTTP signaling / control plane (see README; configurable in the app)
## - UDP: ICE candidate port range (must match WEBRTC_UDP_PORT_MIN/MAX and firewall rules)
##

FROM golang:1.22-bookworm AS builder

WORKDIR /src

# Cache deps first.
COPY go.mod ./
RUN go mod download

COPY . .

# Build a static binary for a minimal runtime image.
# The main package is expected at ./cmd/aero-webrtc-udp-relay, but we fall back to other common
# scaffolds for resilience.
RUN set -eux; \
    target="./cmd/aero-webrtc-udp-relay"; \
    if [ ! -d "${target}" ]; then target="./cmd/webrtc-udp-relay"; fi; \
    if [ ! -d "${target}" ]; then target="."; fi; \
    CGO_ENABLED=0 GOOS=linux go build -trimpath -ldflags="-s -w" -o /out/aero-webrtc-udp-relay "${target}"

FROM gcr.io/distroless/static-debian12:nonroot

WORKDIR /app

# Copy CA certs for outbound HTTPS (STUN/TURN discovery, metrics exports, etc.).
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

COPY --from=builder /out/aero-webrtc-udp-relay /app/aero-webrtc-udp-relay

USER nonroot:nonroot

EXPOSE 8080/tcp
# Default ICE UDP port range; override via WEBRTC_UDP_PORT_MIN/MAX and expose/publish accordingly.
EXPOSE 50000-50100/udp

ENTRYPOINT ["/app/aero-webrtc-udp-relay"]
