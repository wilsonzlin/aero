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
#   # (or use the shared name supported by the gateway + WebRTC relay)
#   ALLOWED_ORIGINS=http://localhost:5173 cargo run --locked -p aero-l2-proxy
#   # Optionally append additional origins (comma-prefixed convention):
#   ALLOWED_ORIGINS=https://localhost AERO_L2_ALLOWED_ORIGINS_EXTRA=",http://localhost:5173" cargo run --locked -p aero-l2-proxy
# - Trusted local dev escape hatch (disables Origin enforcement):
#   AERO_L2_OPEN=1 cargo run --locked -p aero-l2-proxy
# - Authentication (recommended for any internet-exposed deployment):
#   - Session cookie (same-origin browser sessions; requires `aero_session` cookie from `POST /session`):
#     AERO_L2_AUTH_MODE=session AERO_L2_SESSION_SECRET=sekrit cargo run --locked -p aero-l2-proxy
#     (The proxy must share the gateway session signing secret: `SESSION_SECRET` / `AERO_GATEWAY_SESSION_SECRET`.)
#     (Legacy alias: `AERO_L2_AUTH_MODE=cookie`.)
#   - Token (simple cross-origin / server-to-server; dev-only if long-lived):
#     AERO_L2_AUTH_MODE=token AERO_L2_API_KEY=sekrit cargo run --locked -p aero-l2-proxy
#     (Deprecated compatibility: legacy `AERO_L2_TOKEN=sekrit` is treated as a token when
#     `AERO_L2_AUTH_MODE` is unset, and is also accepted as a fallback value for `AERO_L2_API_KEY`.)
#     (Legacy alias: `AERO_L2_AUTH_MODE=api_key`.)
#   - JWT (recommended for cross-origin / short-lived tokens):
#     AERO_L2_AUTH_MODE=jwt AERO_L2_JWT_SECRET=sekrit cargo run --locked -p aero-l2-proxy
#     # Optional claim enforcement:
#     # AERO_L2_JWT_AUDIENCE=aero AERO_L2_JWT_ISSUER=aero-gateway
#   - Mixed/hybrid modes:
#     - Cookie + JWT:
#       AERO_L2_AUTH_MODE=cookie_or_jwt AERO_L2_SESSION_SECRET=sekrit AERO_L2_JWT_SECRET=sekrit cargo run --locked -p aero-l2-proxy
#     - Session cookie + token (accept either):
#       AERO_L2_AUTH_MODE=session_or_token AERO_L2_SESSION_SECRET=sekrit AERO_L2_API_KEY=sekrit cargo run --locked -p aero-l2-proxy
#       # (If AERO_L2_API_KEY is omitted, the proxy still accepts session-cookie auth; the token path is simply disabled.)
#     - Session cookie + token (require both):
#       AERO_L2_AUTH_MODE=session_and_token AERO_L2_SESSION_SECRET=sekrit AERO_L2_API_KEY=sekrit cargo run --locked -p aero-l2-proxy
#   - Credential delivery:
#     - query params: `?token=...` (or `?apiKey=...` for compatibility)
#     - subprotocol token: additional `Sec-WebSocket-Protocol` entry `aero-l2-token.<credential>`
#       (offered alongside `aero-l2-tunnel-v1`)
#     - JWTs can also be provided via `Authorization: Bearer <token>` when using a non-browser client.
#
# - Quotas:
#   - AERO_L2_MAX_CONNECTIONS=64                 # process-wide concurrent tunnel cap (`0` disables)
#   - AERO_L2_MAX_CONNECTIONS_PER_SESSION=0      # per-session concurrent tunnel cap (`0` disables; legacy alias: AERO_L2_MAX_TUNNELS_PER_SESSION)
#
# Observability knobs:
# - Optional: per-session PCAPNG capture (writes one file per tunnel session):
#   AERO_L2_CAPTURE_DIR=/tmp/aero-l2-captures cargo run --locked -p aero-l2-proxy
# - Optional: have the proxy send protocol-level PINGs (RTT is recorded in Prometheus metrics):
#   AERO_L2_PING_INTERVAL_MS=1000 cargo run --locked -p aero-l2-proxy
```

Expected behavior:

- The proxy listens on `AERO_L2_PROXY_LISTEN_ADDR` (default: `0.0.0.0:8090`).
- Operational endpoints:
  - `GET /healthz` – liveness
  - `GET /readyz` – readiness
  - `GET /version` – build/version info
  - `GET /metrics` – Prometheus metrics
- The proxy is configured with a strict egress policy in production; local dev may enable “open” mode.

#### Browser: establish the L2 tunnel over WebSocket

In the browser, create a WebSocket L2 tunnel client and connect it to the proxy:

```ts
import { WebSocketL2TunnelClient } from "./net";

// `gatewayBaseUrl` can be:
// - `ws://...` / `wss://...` (explicit WebSocket URL), or
// - `http://...` / `https://...` (auto-converted to ws(s) and `/l2` appended), or
// - a same-origin path like `/l2` when running the full web app.
const l2 = new WebSocketL2TunnelClient("http://127.0.0.1:8090", (ev) => {
  if (ev.type === "frame") nicRx(ev.frame);
  if (ev.type === "error") console.error(ev.error);
});

l2.connect();
// `sendFrame()` returns a boolean indicating whether the frame was accepted into
// the client's outbound queue; most callers can ignore it.
nicTx = (frame) => {
  l2.sendFrame(frame);
};
```

If the proxy requires token-based auth (`AERO_L2_AUTH_MODE=token|jwt`, or the legacy
`AERO_L2_TOKEN` alias), pass a credential and choose how it is transported:

```ts
const l2 = new WebSocketL2TunnelClient("ws://127.0.0.1:8090", sink, {
  token: "sekrit",
  // Default is "query" (adds ?token=...); "subprotocol" uses an additional
  // Sec-WebSocket-Protocol entry `aero-l2-token.<token>` alongside `aero-l2-tunnel-v1`.
  // Prefer "subprotocol" when possible to avoid putting secrets in URLs/logs; use "query" when
  // the credential isn't a valid HTTP token (RFC 7230 `tchar`) or the client cannot set subprotocols.
  tokenTransport: "subprotocol",
});
```

Note: non-browser clients can alternatively provide JWT credentials via an `Authorization: Bearer <token>`
header (supported by `crates/aero-l2-proxy` in `AERO_L2_AUTH_MODE=jwt|cookie_or_jwt`).

#### Browser-side observability (worker runtime)

When running the full worker-based emulator runtime, the network worker emits low-rate `log` events
on the runtime event ring to help debug L2 tunnel bring-up/backpressure:

- Look for `[net] l2: ...` logs in the dev console (e.g. `l2: open tx=... rx=... drop+{...}`).
- Connection transitions (`l2: connecting/open/closed/error`) are logged immediately.
- When drop deltas are non-zero, the periodic stats log is emitted at `WARN` so it also appears in
  the coordinator's nonfatal stream.
- Tunnel transport errors are emitted at `ERROR` (`l2: error: ...`).


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
 # If the backend uses session-cookie auth (`AERO_L2_AUTH_MODE=session` on `crates/aero-l2-proxy`):
 # export L2_BACKEND_FORWARD_AERO_SESSION=1   # forwards Cookie: aero_session=... captured from signaling
 # export L2_BACKEND_AUTH_FORWARD_MODE=query        # default
 # export L2_BACKEND_AUTH_FORWARD_MODE=subprotocol  # offer Sec-WebSocket-Protocol entry aero-l2-token.<credential> alongside aero-l2-tunnel-v1 (credential must be a valid HTTP token / RFC 7230 tchar)
 # If your backend requires a static token (e.g. `AERO_L2_AUTH_MODE=token|jwt` on `crates/aero-l2-proxy`):
 # export L2_BACKEND_TOKEN=sekrit                   # offer Sec-WebSocket-Protocol entry aero-l2-token.<token> alongside aero-l2-tunnel-v1 (token must be a valid HTTP token / RFC 7230 tchar)
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
  // `baseUrl` can be http(s):// or ws(s):// depending on how the relay is
  // exposed (some deployments share a single wss:// origin behind a reverse
  // proxy). The browser client will normalize schemes per endpoint transport.
  baseUrl: "https://relay.example.com", // (or "wss://relay.example.com")
  authToken: "…", // optional
  mode: "ws-trickle", // default
  sink: (ev) => {
    if (ev.type === "frame") {
      nicRx(ev.frame);
    }
  },
});

nicTx = (frame) => {
  l2.sendFrame(frame);
};
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
  hooks described in [`07-networking.md`](./07-networking.md#network-tracing-pcappcapng-export)
  (browser UI panel “Network trace (PCAPNG)” / `window.aero.netTrace.downloadPcapng()`).

## Secure deployment (recommended)

Treat the L2 proxy as a **high-risk network egress surface**. A secure deployment needs:
**Origin enforcement + authentication + egress policy**.

### 1) Origin allowlist (required)

By default, `crates/aero-l2-proxy` requires an `Origin` header on the WebSocket upgrade request and
validates it against an allowlist:

- `AERO_L2_ALLOWED_ORIGINS`: comma-separated list of allowed origins.
  - If unset/empty, falls back to `ALLOWED_ORIGINS` (shared with the gateway + WebRTC relay).
  - `AERO_L2_ALLOWED_ORIGINS_EXTRA` (optional) is appended (comma-prefixed convention used by `deploy/docker-compose.yml`).
  - Origins are normalized before comparison (see `docs/l2-tunnel-protocol.md` for rules/examples).
  - Example: `https://app.example.com,https://staging.example.com`
  - `*` allows any **valid** Origin value (still requires the header to be present).

Dev escape hatch:

- `AERO_L2_OPEN=1` disables Origin enforcement (trusted local development only).

### 2) Authentication (required)

Origin enforcement is not sufficient to protect an internet-exposed L2 endpoint: non-browser
clients can omit or forge `Origin`.

`crates/aero-l2-proxy` supports multiple auth modes via `AERO_L2_AUTH_MODE`:

#### a) Same-origin browser clients (recommended): session-cookie auth

- Set `AERO_L2_AUTH_MODE=session` (legacy alias: `cookie`).
- Ensure the proxy shares the gateway session signing secret:
  - `SESSION_SECRET` (gateway), and
  - `AERO_L2_SESSION_SECRET` (or `SESSION_SECRET` / `AERO_GATEWAY_SESSION_SECRET`) on the L2 proxy must match.

Single-origin flow:

1) Browser calls `POST /session` on the gateway (same origin) to receive the `aero_session` cookie
2) Browser opens `wss://<origin>/l2` (subprotocol `aero-l2-tunnel-v1`) and relies on the cookie

#### b) Cross-origin / non-browser clients: token or JWT

- **JWT** (recommended):
  - Configure: `AERO_L2_AUTH_MODE=jwt` and `AERO_L2_JWT_SECRET=...`
  - Optional claim validation: `AERO_L2_JWT_AUDIENCE` and/or `AERO_L2_JWT_ISSUER`
- **Token** (simpler, but avoid long-lived keys for public deployments):
  - Configure: `AERO_L2_AUTH_MODE=token` and `AERO_L2_API_KEY=...` (legacy alias: `api_key`)
    - Legacy alias: `AERO_L2_TOKEN=...` (used when `AERO_L2_AUTH_MODE` is unset; also accepted as a
      fallback value for `AERO_L2_API_KEY`).
- Optional mixed modes:
  - `AERO_L2_AUTH_MODE=cookie_or_jwt` accepts either a session cookie or a JWT.
  - `AERO_L2_AUTH_MODE=session_or_token` accepts either a session cookie or a token.

Credentials offered via `Sec-WebSocket-Protocol` must be valid WebSocket subprotocol tokens (HTTP token / RFC 7230 `tchar`).
Prefer subprotocol delivery when possible to avoid putting secrets in URLs/logs; use query-string delivery when the credential
cannot be expressed as a subprotocol token.

- Query string: `wss://proxy.example.com/l2?token=<value>` (or `?apiKey=<value>` for compatibility)
- Header: `Authorization: Bearer <token>` (JWT only)
- WebSocket subprotocol: offer an additional `Sec-WebSocket-Protocol` entry `aero-l2-token.<value>`
  alongside `aero-l2-tunnel-v1`.

Missing/incorrect credentials reject the upgrade with **HTTP 401** (no WebSocket).

### 3) WebRTC L2 bridging (relay forwards auth + Origin)

When carrying the L2 tunnel over WebRTC, the browser connects to `proxy/webrtc-udp-relay` and the
relay opens a backend WebSocket to `aero-l2-proxy` and bridges:

- **auth** (cookie and/or token, depending on relay config), and
- **Origin** (forwarded so the backend can enforce the same allowlist).

In the canonical compose stack, enable the backend wiring in `deploy/.env`:

```bash
L2_BACKEND_WS_URL=ws://aero-l2-proxy:8090/l2
L2_BACKEND_AUTH_FORWARD_MODE=query
L2_BACKEND_FORWARD_ORIGIN=1
L2_BACKEND_FORWARD_AERO_SESSION=1  # recommended when `aero-l2-proxy` uses AERO_L2_AUTH_MODE=session
```

Then ensure auth is compatible end-to-end:

- If using session-cookie auth (`AERO_L2_AUTH_MODE=session`), ensure `aero-l2-proxy` shares the gateway `SESSION_SECRET`
  and the relay has `L2_BACKEND_FORWARD_AERO_SESSION=1` enabled.
- If using token auth, configure `aero-l2-proxy` with `AERO_L2_AUTH_MODE=token` and `AERO_L2_API_KEY=...`
  (or the legacy `AERO_L2_TOKEN=...` alias).
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
