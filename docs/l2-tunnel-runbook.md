# L2 tunnel runbook (Option C)

This document is a practical guide for running Aero’s **Option C** networking path:
**tunnel raw Ethernet frames (L2)** between the browser and a proxy that runs the user-space NAT
stack, using the versioned framing described in [`l2-tunnel-protocol.md`](./l2-tunnel-protocol.md).

**Final decision:** [ADR 0005: Networking via L2 tunnel (Option C) to an unprivileged proxy](./adr/0005-networking-l2-tunnel.md).
For background/tradeoffs, see [`networking-architecture-rfc.md`](./networking-architecture-rfc.md).
For the wire protocol/framing, see [`l2-tunnel-protocol.md`](./l2-tunnel-protocol.md).

## Local development

### 1) Start the L2 proxy (WebSocket data plane)

The recommended dev setup uses a **single WebSocket per VM session** carrying the L2 tunnel framing
as binary messages.

Start the proxy service.

Current implementations in this repo:

- Rust (production target: L2 tunnel termination + user-space NAT stack + egress policy):

```bash
cargo run -p aero-l2-proxy

# Optional: override listen address (default: 0.0.0.0:8090)
# AERO_L2_PROXY_LISTEN_ADDR=127.0.0.1:8090 cargo run -p aero-l2-proxy
```

- Node (WebSocket upgrade policy / quota harness; **does not implement the L2 data plane**; used by `tests/l2_proxy_security.test.js`):

```bash
node --experimental-strip-types proxy/aero-l2-proxy/src/index.ts
```

Expected behavior:

- Proxy listens on `AERO_L2_PROXY_LISTEN_ADDR` (default: `0.0.0.0:8090`).
- Proxy serves a liveness endpoint (typically `GET /healthz`) for basic checks.
- Proxy is configured with a strict egress policy in production; local dev may enable “open” mode.

### 2) (Optional) Start the WebRTC relay (DataChannel transport)

WebRTC is optional. Use it when you want to evaluate loss/latency behavior that differs from
WebSocket (e.g. reduced head-of-line blocking).

If you carry the **L2 tunnel** over WebRTC, the DataChannel must be configured as:

- **reliable** (no frame loss / no partial reliability), and
- unordered is OK.

See ADR 0005 for the rationale.

The existing relay implementation lives at `proxy/webrtc-udp-relay/`:

```bash
cd proxy/webrtc-udp-relay
go run ./cmd/aero-webrtc-udp-relay
```

See [`proxy/webrtc-udp-relay/README.md`](../proxy/webrtc-udp-relay/README.md) for TURN/docker-compose
notes and security controls.

### 3) Run the RFC-style probe (ARP + DHCP + DNS + TCP echo)

The fastest sanity check for an L2 tunnel is to run the RFC prototype probe, which exercises the
expected “minimum viable LAN” behaviors:

- **ARP** (discover gateway MAC)
- **DHCP** (obtain guest IP + gateway + DNS)
- **DNS** (UDP/53 query/response)
- **TCP echo** (SYN/SYN-ACK/ACK + data roundtrip)

Automated probe (ARP + DNS + TCP echo) lives in this repo today:

```bash
node --test tests/networking-architecture-rfc.test.js
```

This test spins up a minimal WebSocket frame-forwarding proxy and a local TCP echo server, then
performs the probe over Ethernet frames wrapped in the `aero-l2-tunnel-v1` framing.

DHCP verification (until the automated probe covers it):

- Boot a guest and confirm it receives a lease (Windows: `ipconfig /all`).
- Force renewal (Windows: `ipconfig /renew`) and capture traffic with the built-in PCAP tracing
  hooks described in [`07-networking.md`](./07-networking.md#network-tracing-pcappcapng-export).

## Production checklist

Treat the L2 proxy as a **high-risk network egress surface**. A secure deployment requires policy
and hardening at the proxy boundary.

Minimum checklist:

- **Origin allowlist**
  - Enforce `Origin` on WebSocket upgrades; enforce strict CORS on any HTTP endpoints.
  - Consider also validating `Host` / `X-Forwarded-Host` when behind a reverse proxy.
- **Auth + session binding**
  - Require a cookie-backed session or short-lived token.
  - Bind tunnel sessions to an authenticated user and enforce per-user quotas.
- **Blocked destination ranges**
  - Deny loopback, link-local, RFC1918, CGNAT, multicast, and other special-use ranges by default.
  - Re-check after DNS resolution to prevent DNS rebinding (hostnames that resolve to internal IPs).
- **Port allowlist**
  - Default-deny outbound ports; allow only what you intend to support (typically 80/443 plus a
    small set of well-known ports if needed).
- **Quotas / rate limits**
  - Max concurrent sessions per user/IP.
  - Max concurrent TCP/UDP flows per session.
  - Byte/packet rate limits and burst limits (protects CPU and upstream bandwidth).

Operational recommendations:

- Log enough metadata to audit abuse (destination IP/port, byte counts, auth principal), but avoid
  logging raw payload bytes by default.
- Provide `/healthz`, `/readyz`, and basic metrics endpoints for monitoring.
- Consider explicit “open dev mode” toggles so production never accidentally runs with permissive
  settings.
