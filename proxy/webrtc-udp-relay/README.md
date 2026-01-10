# Aero WebRTC → UDP Relay

This directory contains a standalone Go service intended to proxy UDP between:

- the browser (guest networking stack running inside the emulator), and
- a server-side UDP relay reachable from the browser.

See `PROTOCOL.md` for the on-the-wire framing and signaling message shapes.

## Running

From this directory:

```bash
go run ./cmd/aero-webrtc-udp-relay
```

Then:

```bash
curl -sS http://127.0.0.1:8080/healthz
```

## HTTP endpoints

- `GET /healthz` → `{"ok":true}`
- `GET /readyz` → readiness (200 once serving, 503 during shutdown)
- `GET /version` → build metadata (commit/build time may be empty)

## Implemented

- Minimal production-oriented HTTP server skeleton + middleware
  - `GET /healthz` → `{"ok":true}`
  - `GET /readyz` → readiness (200 once serving, 503 during shutdown)
  - `GET /version` → build metadata (commit/build time may be empty)
- Config system (env + flags): listen address, public base URL, log format/level, shutdown timeout, dev/prod mode
- Relay/policy primitives (not yet wired to WebRTC signaling)
- Protocol documentation (`PROTOCOL.md`)

## Pending (future tasks)

- WebRTC signaling (SDP exchange, ICE candidate handling)
- WebRTC peer connection lifecycle management (`pion/webrtc`)
- WebRTC ↔ UDP data plane integration (enforcing policy on every datagram)
- Auth and additional policy controls (rate limits, allowlists, etc)

## Ports

- **HTTP**: configurable via `--listen-addr` (default `127.0.0.1:8080`)
- **UDP (future)**: ICE + relay UDP ports will be introduced once the WebRTC and relay logic lands (expect additional inbound UDP requirements).

## Configuration

### Service config (env + flags)

The service supports configuration via environment variables and equivalent flags:

- `AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR` / `--listen-addr` (default `127.0.0.1:8080`)
- `AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL` / `--public-base-url` (optional; used for logging)
- `AERO_WEBRTC_UDP_RELAY_LOG_FORMAT` / `--log-format` (`text` or `json`)
- `AERO_WEBRTC_UDP_RELAY_LOG_LEVEL` / `--log-level` (`debug`, `info`, `warn`, `error`)
- `AERO_WEBRTC_UDP_RELAY_SHUTDOWN_TIMEOUT` / `--shutdown-timeout` (default `15s`)
- `AERO_WEBRTC_UDP_RELAY_MODE` / `--mode` (`dev` or `prod`)

### Destination policy (UDP egress)

The relay is **network egress**. If you run it on an Internet-reachable host without destination controls, it can become an **open proxy / SSRF primitive** that attackers can use to:

- scan internal networks (`10.0.0.0/8`, `192.168.0.0/16`, etc.)
- hit cloud metadata endpoints
- attack link-local services
- abuse your host as a generic UDP reflector

To mitigate this, the relay enforces an outbound destination policy (`internal/policy.DestinationPolicy`) on **every outbound UDP datagram** (and can also drop inbound datagrams from denied sources).

#### Safe defaults

By default, the policy is **deny-by-default** and denies common private/special IPv4 ranges unless explicitly enabled.

In other words: if you deploy the relay without any configuration, it should **not** be able to reach arbitrary network targets.

#### Policy configuration

The destination policy is configured via environment variables:

- `DESTINATION_POLICY_PRESET`:
  - `production` / `prod` (default): deny by default (requires explicit allow rules)
  - `dev`: allow by default (still applies deny rules)
- `ALLOW_PRIVATE_NETWORKS` (`true`/`false`, default depends on preset): when `false`, the policy denies at minimum:
  - `127.0.0.0/8` (loopback)
  - `169.254.0.0/16` (link-local)
  - `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` (RFC1918)
  - `100.64.0.0/10` (CGNAT)
  - `224.0.0.0/4` (multicast)
  - `0.0.0.0/8`, `240.0.0.0/4` (reserved)
  - `255.255.255.255/32` (broadcast)
- `ALLOW_UDP_CIDRS`: comma-separated CIDRs to allow (e.g. `1.1.1.1/32,8.8.8.0/24`)
- `DENY_UDP_CIDRS`: comma-separated CIDRs to deny (evaluated before allow)
- `ALLOW_UDP_PORTS`: comma-separated ports/ranges to allow (e.g. `53,123,30000-30100`)
- `DENY_UDP_PORTS`: comma-separated ports/ranges to deny (evaluated before allow)

##### Examples

Allow only public DNS in production:

```bash
export DESTINATION_POLICY_PRESET=production
export ALLOW_PRIVATE_NETWORKS=false
export ALLOW_UDP_CIDRS="1.1.1.1/32,8.8.8.8/32"
export ALLOW_UDP_PORTS="53"
```

Allow any destination (development only):

```bash
export DESTINATION_POLICY_PRESET=dev
```

## Security model (read before deploying the relay)

See "Destination policy (UDP egress)" above.
