# L2 tunnel runbook (Option C)

This document is a practical guide for running Aero’s **Option C** networking path:
**tunnel raw Ethernet frames (L2)** between the browser and a proxy that runs the user-space NAT
stack, using the versioned framing described in [`l2-tunnel-protocol.md`](./l2-tunnel-protocol.md).

**Final decision:** [ADR 0013: Networking via L2 tunnel (Option C) to an unprivileged proxy](./adr/0013-networking-l2-tunnel.md).
For background/tradeoffs, see [`networking-architecture-rfc.md`](./networking-architecture-rfc.md).
For the wire protocol/framing, see [`l2-tunnel-protocol.md`](./l2-tunnel-protocol.md).

## Local development

### 1) Start the L2 proxy (WebSocket data plane)

The recommended dev setup uses a **single WebSocket per VM session** carrying the L2 tunnel framing
as binary messages.

Start the proxy service.

Current implementation in this repo (production target: L2 tunnel termination + user-space NAT stack + egress policy):

```bash
cargo run --locked -p aero-l2-proxy

# Optional: override listen address (default: 0.0.0.0:8090)
# AERO_L2_PROXY_LISTEN_ADDR=127.0.0.1:8090 cargo run --locked -p aero-l2-proxy

# Security knobs (Rust `crates/aero-l2-proxy`):
# - Origin is enforced by default; configure an allowlist for your dev origin:
#   AERO_L2_ALLOWED_ORIGINS=http://localhost:5173 cargo run --locked -p aero-l2-proxy
# - Trusted local dev escape hatch (disables Origin enforcement):
#   AERO_L2_OPEN=1 cargo run --locked -p aero-l2-proxy
# - Optional token auth (recommended for internet-exposed deployments):
#   AERO_L2_TOKEN=sekrit cargo run --locked -p aero-l2-proxy
#   Clients provide the token via `?token=...` or `Sec-WebSocket-Protocol: aero-l2-token.<token>`
#   (in addition to `aero-l2-tunnel-v1`).
#
# Observability knobs:
# - Optional: per-session PCAPNG capture (writes one file per tunnel session):
#   AERO_L2_CAPTURE_DIR=/tmp/aero-l2-captures cargo run --locked -p aero-l2-proxy
# - Optional: have the proxy send protocol-level PINGs (RTT is recorded in Prometheus metrics):
#   AERO_L2_PING_INTERVAL_MS=1000 cargo run --locked -p aero-l2-proxy
```

Expected behavior:

- Proxy listens on `AERO_L2_PROXY_LISTEN_ADDR` (default: `0.0.0.0:8090`).
- Operational endpoints:
  - `GET /healthz` – liveness
  - `GET /readyz` – readiness
  - `GET /version` – build/version info
  - `GET /metrics` – Prometheus metrics
- The proxy is configured with a strict egress policy in production; local dev may enable “open” mode.

### 2) (Optional) Start the WebRTC relay (DataChannel transport)

WebRTC is optional. Use it when you want a UDP-based tunnel transport for experimentation and
evaluation under loss.

If you carry the **L2 tunnel** over WebRTC, the DataChannel must be configured as:

- **reliable** (no frame loss / no partial reliability)
- **ordered** (`ordered = true`)
- leave `maxRetransmits` / `maxPacketLifeTime` unset (default reliable)

See ADR 0013 for the rationale.

The existing relay implementation lives at `proxy/webrtc-udp-relay/`:

```bash
cd proxy/webrtc-udp-relay

 # Bridge WebRTC DataChannel "l2" to the L2 proxy WebSocket endpoint.
 # (The relay will forward the client's Origin + AUTH_MODE credential by default.)
 export L2_BACKEND_WS_URL=ws://127.0.0.1:8090/l2
 
 # Optional knobs:
 # export L2_BACKEND_AUTH_FORWARD_MODE=query        # default
 # export L2_BACKEND_AUTH_FORWARD_MODE=subprotocol  # Sec-WebSocket-Protocol: aero-l2-token.<credential> (credential must be a valid HTTP token / RFC 7230 tchar)
 # If your backend requires a static token (e.g. `AERO_L2_AUTH_MODE=api_key|jwt` on `crates/aero-l2-proxy`):
 # export L2_BACKEND_TOKEN=sekrit                   # Sec-WebSocket-Protocol: aero-l2-token.<token> (token must be a valid HTTP token / RFC 7230 tchar)
 # export L2_BACKEND_AUTH_FORWARD_MODE=none         # don't also forward client creds in ?token=/?apiKey=
 # export L2_BACKEND_ORIGIN_OVERRIDE=https://example.com
 # export L2_BACKEND_ORIGIN=https://example.com # alias for L2_BACKEND_ORIGIN_OVERRIDE
 go run ./cmd/aero-webrtc-udp-relay
```

See [`proxy/webrtc-udp-relay/README.md`](../proxy/webrtc-udp-relay/README.md) for TURN/docker-compose
notes and security controls.

#### Browser: establish the L2 tunnel over WebRTC

In the browser, use the helper in `web/src/net/l2RelaySignalingClient.ts` to negotiate a
`RTCPeerConnection` against the relay and obtain a **fully reliable and ordered** `RTCDataChannel` labeled `l2`:

```ts
import { connectL2Relay } from "./net";

const { l2, close } = await connectL2Relay({
  baseUrl: "https://relay.example.com",
  authToken: "…", // optional
  mode: "ws-trickle", // default
  sink: (ev) => {
    if (ev.type === "frame") {
      nicRx(ev.frame);
    }
  },
});

nicTx = (frame) => l2.sendFrame(frame);
// Later: close();
```

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

## Secure deployment (recommended)

Treat the L2 proxy as a **high-risk network egress surface**. A secure deployment needs:
**Origin enforcement + authentication + egress policy**.

### 1) Origin allowlist (required)

By default, `crates/aero-l2-proxy` requires an `Origin` header on the WebSocket upgrade request and
validates it against an allowlist:

- `AERO_L2_ALLOWED_ORIGINS`: comma-separated list of allowed origins (exact match).
  - Example: `https://app.example.com,https://staging.example.com`
  - `*` allows any Origin value (still requires the header to be present).

Dev escape hatch:

- `AERO_L2_OPEN=1` disables Origin enforcement (trusted local development only).

### 2) Token authentication (recommended)

Origin enforcement is not sufficient to protect an internet-exposed L2 endpoint: non-browser
clients can omit or forge `Origin`.

If `AERO_L2_TOKEN` is set, the L2 proxy requires a matching token during the WebSocket upgrade:

- Recommended: `wss://proxy.example.com/l2?token=<value>`
- Optional: `Sec-WebSocket-Protocol: aero-l2-token.<value>` (in addition to `aero-l2-tunnel-v1`)

Missing/incorrect tokens reject the upgrade with **HTTP 401**.

### 3) WebRTC L2 bridging (relay forwards token + Origin)

When carrying the L2 tunnel over WebRTC, the browser connects to `proxy/webrtc-udp-relay` and the
relay opens a backend WebSocket to `aero-l2-proxy` and bridges:

- **token** (forwarded to the backend, e.g. via query string), and
- **Origin** (forwarded so the backend can enforce the same allowlist).

In the canonical compose stack, enable the backend wiring in `deploy/.env`:

```bash
L2_BACKEND_WS_URL=ws://aero-l2-proxy:8090/l2
L2_BACKEND_AUTH_FORWARD_MODE=query
L2_BACKEND_FORWARD_ORIGIN=1
```

Then ensure auth is compatible end-to-end:

- Configure `aero-l2-proxy` with `AERO_L2_TOKEN=...`.
- Configure the relay auth mode (`AUTH_MODE=jwt` or `AUTH_MODE=api_key`) so the browser presents a
  credential that the relay can forward to the backend as `?token=...` (or via `aero-l2-token.*` when
  `L2_BACKEND_AUTH_FORWARD_MODE=subprotocol` is used).

## Production checklist

Treat the L2 proxy as a **high-risk network egress surface**. A secure deployment requires policy
and hardening at the proxy boundary.

Minimum checklist:

- **Origin allowlist**
  - Enforce `Origin` on WebSocket upgrades; enforce strict CORS on any HTTP endpoints.
  - Consider also validating `Host` / `X-Forwarded-Host` when behind a reverse proxy.
- **Auth + session binding**
  - Browser clients: require a cookie-backed gateway session (`aero_session`).
  - Non-browser/internal clients: require a short-lived token (prefer the WebSocket subprotocol form).
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
