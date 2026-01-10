# Aero WebRTC → UDP Relay (backend scaffold)

This directory contains a standalone Go service that will eventually host the server-side WebRTC signaling and UDP relay logic for Aero.

## Running

From this directory:

```bash
go run ./cmd/aero-webrtc-udp-relay
```

Then:

```bash
curl -sS http://127.0.0.1:8080/healthz
```

## Implemented (this task)

- Minimal production-oriented HTTP server skeleton
  - `GET /healthz` → `{"ok":true}`
  - `GET /readyz` → readiness (200 once serving, 503 during shutdown)
  - `GET /version` → build metadata (commit/build time may be empty)
- Config system (env + flags): listen address, public base URL, log format/level, shutdown timeout, dev/prod mode
- Internal package layout reserved for future work:
  - `internal/signaling`
  - `internal/relay`
  - `internal/policy`

## Pending (future tasks)

- WebRTC signaling (SDP exchange, ICE candidate handling)
- WebRTC peer connection lifecycle management (`pion/webrtc`)
- UDP socket relay logic
- Policy enforcement (rate limits, allowlists, auth, etc)

## Ports

- **HTTP**: configurable via `--listen-addr` (default `127.0.0.1:8080`)
- **UDP (future)**: ICE + relay UDP ports will be introduced once the WebRTC and relay logic lands (expect additional inbound UDP requirements).

## Security model (read before deploying the relay)

A UDP relay is **network egress**. If you run it on an Internet-reachable host without destination controls, it can become an **open proxy / SSRF primitive** that attackers can use to:

- scan internal networks (`10.0.0.0/8`, `192.168.0.0/16`, etc.)
- hit cloud metadata endpoints
- attack link-local services
- abuse your host as a generic UDP reflector

The long-term intent is to enforce an outbound destination policy (`internal/policy`) on **every outbound UDP datagram**.

### Safe defaults (planned)

The intended default policy is **deny-by-default** in production (and denies common private/special IPv4 ranges unless explicitly enabled).

### Planned policy configuration (not yet implemented)

This repository may introduce dedicated environment variables for policy control (CIDR allow/deny rules, port allow/deny rules, "dev" vs "prod" presets, and whether private networks are reachable). When that lands, this section should be updated with the exact names and semantics.
