# Aero WebRTC → UDP Relay

[![webrtc-udp-relay](https://github.com/wilsonzlin/aero/actions/workflows/webrtc-udp-relay.yml/badge.svg)](https://github.com/wilsonzlin/aero/actions/workflows/webrtc-udp-relay.yml)

This directory contains a standalone Go service intended to proxy UDP between:

- the browser (guest networking stack running inside the emulator), and
- a server-side UDP relay reachable from the browser.

See `PROTOCOL.md` for the on-the-wire framing and signaling message shapes.

It also includes a turnkey container deployment story:

- `Dockerfile`: multi-stage Go build → minimal runtime image (distroless, non-root)
- `docker-compose.yml`: run the relay alone, or with a local `coturn` TURN server
- `turn/turnserver.conf`: minimal TURN config (committed)

## CI / local checks

CI is scoped to changes under `proxy/webrtc-udp-relay/**` and runs the same checks you can run locally:

```bash
cd proxy/webrtc-udp-relay
make test
make fmt-check
make vet
make staticcheck
make docker-build
```

To apply formatting locally, run `make fmt`.

## Running (local)

From this directory:

```bash
# For local dev without auth:
AUTH_MODE=none go run ./cmd/aero-webrtc-udp-relay

# Or, to use API key auth (default when AUTH_MODE is unset):
# API_KEY=secret go run ./cmd/aero-webrtc-udp-relay
```

Then:

```bash
curl -sS http://127.0.0.1:8080/healthz
```

## Running (Docker / docker-compose)

### Relay only

```bash
cd proxy/webrtc-udp-relay
docker compose --profile relay-only up --build
```

To populate `GET /version` in the container build, export build args first:

```bash
export BUILD_COMMIT="$(git rev-parse HEAD)"
export BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
docker compose --profile relay-only up --build
```

Health check:

```bash
curl -f http://localhost:8080/healthz
```

### Relay + TURN (coturn)

Use this when clients are behind NAT/firewalls and direct UDP connectivity is unreliable.

```bash
cd proxy/webrtc-udp-relay

# For local dev (browser on the same machine), defaults are OK.
docker compose --profile with-turn up --build
```

For TURN REST (ephemeral) credentials (recommended), use the `with-turn-rest`
profile instead:

```bash
docker compose --profile with-turn-rest up --build
```

If your browser clients are **not** running on the same machine as Docker, you must advertise a
publicly reachable hostname/IP:

```bash
TURN_PUBLIC_HOST=example.com docker compose --profile with-turn up --build
```

And update `turn/turnserver.conf` (`external-ip=...`) to match.

#### TURN credentials (default)

The bundled `coturn` config uses long-term credentials:

- username: `aero`
- password: `aero`

Change these before exposing TURN to the internet.

#### TURN REST (ephemeral) credentials for coturn (recommended)

Avoid embedding long-lived TURN usernames/passwords in the browser. When
`TURN_REST_SHARED_SECRET` is set, the relay will generate short-lived
coturn-compatible TURN REST credentials and inject them into the TURN servers
returned by `GET /webrtc/ice`.

coturn config (CLI flags):

```bash
turnserver \
  --use-auth-secret \
  --static-auth-secret="${TURN_REST_SHARED_SECRET}" \
  --realm="${TURN_REST_REALM:-example.com}"
```

For docker-compose local dev, `docker-compose.yml` provides a `with-turn-rest`
profile that runs coturn with `--use-auth-secret` and passes `--static-auth-secret`
from `TURN_REST_SHARED_SECRET`.

Relay env:

- `TURN_REST_SHARED_SECRET` (required to enable; must match coturn `--static-auth-secret`)
- `TURN_REST_TTL_SECONDS` (default `3600`)
- `TURN_REST_USERNAME_PREFIX` (default `aero`)
- `TURN_REST_REALM` (optional; documented for coturn config parity)

Security note: TURN REST credentials are still usable by any JavaScript running
under an allowed origin. Keep `ALLOWED_ORIGINS` tight (or leave it unset to
default to same host:port only).

## E2E interoperability test (Playwright)

`e2e/` contains Playwright tests that launch headless Chromium and exercise:

- the WebRTC `udp` DataChannel framing, and
- the `/udp` WebSocket UDP relay fallback,

against a local UDP echo server.

```bash
# Install deps once from the repo root (npm workspaces; shared lockfile):
npm ci

# Run from the repo root:
npm -w proxy/webrtc-udp-relay/e2e test

# Or run from this directory (after the root install above):
cd proxy/webrtc-udp-relay/e2e
npm test
```

For local convenience, `npm test` installs the Playwright Chromium browser automatically (via `pretest`) to ensure the browser binary is available.
In CI (or when `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`), the `pretest` hook is a no-op and browser setup should be handled by the shared `.github/actions/setup-playwright` cache/install step.

The E2E test also requires a working Go toolchain: it builds and launches the production relay (`cmd/aero-webrtc-udp-relay`) and performs WebSocket signaling against `GET /webrtc/signal` using the schema documented in `PROTOCOL.md`.

### System dependencies (Playwright)

On Debian/Ubuntu, Playwright can install its required shared libraries automatically:

```bash
node scripts/playwright_install.mjs chromium --with-deps
```

If Chromium fails to launch in CI, ensure the container/runner includes the Playwright Linux dependencies.

## HTTP endpoints

- `GET /healthz` → `{"ok":true}`
- `GET /readyz` → readiness (200 once serving, 503 during shutdown or when ICE config is invalid)
- `GET /version` → build metadata (commit/build time may be empty)
- `GET /metrics` → Prometheus text exposition of internal counters
- `GET /webrtc/ice` → ICE server list for browser clients: `{"iceServers":[...]}`
  - guarded by the same origin policy as signaling endpoints (to avoid leaking TURN credentials cross-origin)
  - when `AUTH_MODE != none`, also requires the same credentials as signaling endpoints (to avoid leaking TURN REST credentials to unauthenticated callers)
  - responses are explicitly **non-cacheable** (`Cache-Control: no-store`, `Pragma: no-cache`, `Expires: 0`) to avoid leaking/staling TURN credentials via browser/proxy caches
- `POST /offer` → signaling: exchange SDP offer/answer (non-trickle ICE) per `PROTOCOL.md` (requires auth when `AUTH_MODE != none`)
- `POST /session` → allocate a **short-lived** server-side session reservation (expires after `SESSION_PREALLOC_TTL`) (primarily for quota enforcement; not required by the v1 offer/answer flow) (requires auth when `AUTH_MODE != none`)
- `GET /webrtc/signal` → WebSocket signaling (trickle ICE) (requires auth when `AUTH_MODE != none`)
- `POST /webrtc/offer` → HTTP offer → answer (non-trickle ICE fallback) (requires auth when `AUTH_MODE != none`)
  - The response includes an opaque `sessionId` for observability. When `AUTH_MODE=jwt`,
    the relay enforces per-session quotas using the JWT `sid` claim internally (not the
    returned `sessionId`) and rejects concurrent sessions with the same `sid` with
    `409 Conflict` (`code: "session_already_active"`).
    Minting a different JWT with the same `sid` does not allow a second concurrent session.
    Recovery: close the existing WebRTC session (or wait for it to end) before retrying, or mint a JWT with a different `sid` for a separate concurrent session.
- `GET /udp` → WebSocket UDP relay fallback (binary datagram frames; see `PROTOCOL.md`) (requires auth when `AUTH_MODE != none`)
  - guarded by the same origin policy as signaling endpoints

## L2 tunnel bridging (optional)

The relay can also bridge a WebRTC DataChannel labeled `l2` to a backend WebSocket
endpoint (typically `aero-l2-proxy` at `/l2`; legacy alias: `/eth`).

The `l2` DataChannel must be **fully reliable and ordered**:

- `ordered = true`
- do **not** set `maxRetransmits` / `maxPacketLifeTime` (partial reliability)

```
browser DataChannel "l2"  <->  webrtc-udp-relay  <->  backend WebSocket /l2 (legacy alias: /eth)
```

The relay does not interpret `l2` messages, but the framing constants and helper codec used by tests live in:
`internal/l2tunnel/` (validated against `crates/conformance/test-vectors/aero-vectors-v1.json`).

Configuration env vars (server → server dialing):

- `L2_BACKEND_WS_URL` (optional): Backend `ws://` / `wss://` URL. When unset/empty,
  `l2` DataChannels are rejected.
- `L2_BACKEND_FORWARD_ORIGIN` (default: `true` when `L2_BACKEND_WS_URL` is set):
  forward a normalized/derived Origin from the client signaling request to the
  backend WebSocket upgrade request.
- `L2_BACKEND_ORIGIN` (optional): Override Origin sent to the backend WebSocket.
  This value must be allowed by the backend (e.g. `AERO_L2_ALLOWED_ORIGINS` or the
  shared `ALLOWED_ORIGINS` fallback for `crates/aero-l2-proxy`).
  - Alias: `L2_BACKEND_ORIGIN_OVERRIDE` (overrides `L2_BACKEND_WS_ORIGIN`).
- `L2_BACKEND_WS_ORIGIN` (optional): Static Origin header value to send when
  dialing the backend WebSocket (overridden by `L2_BACKEND_ORIGIN` /
  `L2_BACKEND_ORIGIN_OVERRIDE`).
- `L2_BACKEND_AUTH_FORWARD_MODE` (default `query`): `none|query|subprotocol` —
  how to forward the relay credential (JWT/token) to the backend:
  - `query`: append `token=<credential>` (and `apiKey=<credential>` for compatibility)
  - `subprotocol`: offer `aero-l2-token.<credential>` (credential must be valid
    for `Sec-WebSocket-Protocol`, i.e. an HTTP token / RFC 7230 `tchar`)
  - `none`: do not forward credentials
- `L2_BACKEND_TOKEN` (optional): If set, offer an additional WebSocket
  subprotocol `aero-l2-token.<token>` when dialing the backend (alongside
  `aero-l2-tunnel-v1`).
  - Alias: `L2_BACKEND_WS_TOKEN`.
- `L2_MAX_MESSAGE_BYTES` (default `4096`): Per-message size limit enforced on the
  DataChannel and backend WebSocket.

These settings are **not** browser auth knobs; they configure only the relay's
outbound connection to the backend. Browser signaling auth is configured via
`AUTH_MODE` (`api_key`/`jwt`).

Security note: If you need to pass a token that cannot be represented as a WebSocket
subprotocol token, you can embed `?token=...` in `L2_BACKEND_WS_URL` instead. This is
less preferred because query strings are more likely to leak into logs/metrics.

### Example: bridge to `crates/aero-l2-proxy` (Docker / Kubernetes)

When the L2 backend is `crates/aero-l2-proxy`, the relay's backend WebSocket dial
must satisfy the proxy's Origin allowlist and auth policy:

- **Origin allowlist:** the backend requires `Origin` (unless `AERO_L2_OPEN=1`) and
  checks it against the configured allowlist (`AERO_L2_ALLOWED_ORIGINS` or
  `ALLOWED_ORIGINS` fallback).
  - By default the relay forwards the client signaling Origin (`L2_BACKEND_FORWARD_ORIGIN=1`).
  - Use `L2_BACKEND_ORIGIN` to pin a specific Origin value (must be in the backend allowlist).
- **Auth:** if the backend uses gateway session cookies (`AERO_L2_AUTH_MODE=session|cookie_or_jwt|session_or_token|session_and_token`;
  legacy aliases: `cookie`, `cookie_or_api_key`, `cookie_and_api_key`), enable `L2_BACKEND_FORWARD_AERO_SESSION=1` so the relay forwards
  `Cookie: aero_session=<...>` extracted from the signaling request.
  - For token/JWT auth (`AERO_L2_AUTH_MODE=token|jwt|cookie_or_jwt|session_or_token|session_and_token`; legacy aliases:
    `api_key`, `cookie_or_api_key`, `cookie_and_api_key`), configure `L2_BACKEND_TOKEN` and/or `L2_BACKEND_AUTH_FORWARD_MODE`.
  - For `AERO_L2_AUTH_MODE=session_and_token` (legacy alias: `cookie_and_api_key`), you typically need **both**
    `L2_BACKEND_FORWARD_AERO_SESSION=1` and a token (`L2_BACKEND_TOKEN` or forwarded client credentials).

#### Docker / docker-compose

Use the backend service name inside the docker network:

```bash
# L2 proxy (backend).
export AERO_L2_ALLOWED_ORIGINS=https://aero.example.com
# (Alternatively, the backend also accepts the shared name `ALLOWED_ORIGINS` when
# `AERO_L2_ALLOWED_ORIGINS` is unset.)
export AERO_L2_AUTH_MODE=token
export AERO_L2_API_KEY='REPLACE_WITH_SECRET'

# WebRTC relay (bridge).
export L2_BACKEND_WS_URL=ws://aero-l2-proxy:8090/l2 # legacy alias: /eth
export L2_BACKEND_ORIGIN=https://aero.example.com
export L2_BACKEND_TOKEN='REPLACE_WITH_SECRET'

# IMPORTANT: If you set L2_BACKEND_TOKEN and also run the relay with AUTH_MODE enabled,
# disable credential forwarding so the backend doesn't see a conflicting ?token=/?apiKey=.
export L2_BACKEND_AUTH_FORWARD_MODE=none
```

The relay sends the token using the WebSocket subprotocol mechanism:

`Sec-WebSocket-Protocol: aero-l2-tunnel-v1, aero-l2-token.<token>`

#### Kubernetes

Use the L2 proxy Service DNS name in `L2_BACKEND_WS_URL`, for example:

```yaml
env:
  - name: L2_BACKEND_WS_URL
    value: ws://aero-l2-proxy.aero.svc.cluster.local:8090/l2 # legacy alias: /eth
  - name: L2_BACKEND_ORIGIN
    value: https://aero.example.com
  - name: L2_BACKEND_TOKEN
    valueFrom:
      secretKeyRef:
        name: aero-l2-bridge
        key: token
  - name: L2_BACKEND_AUTH_FORWARD_MODE
    value: none
```

## Implemented

- Minimal production-oriented HTTP server skeleton + middleware
- Config system (env + flags): listen address, public base URL, log format/level, shutdown timeout, ICE gathering timeout, dev/prod mode
- WebRTC network config (env + flags): ICE UDP port range, UDP listen IP, NAT 1:1 public IP advertisement
- Configurable ICE servers (STUN/TURN) + client-facing discovery endpoint (`/webrtc/ice`)
- WebRTC signaling:
  - `POST /offer` (non-trickle offer/answer, versioned JSON)
  - `GET /webrtc/signal` (WebSocket signaling with trickle ICE)
  - `POST /webrtc/offer` (HTTP offer/answer fallback; non-trickle)
- Signaling authentication (`AUTH_MODE=none|api_key|jwt`) on HTTP + WebSocket endpoints
- WebRTC DataChannel (`udp`) ↔ UDP datagram relay with per-guest-port UDP bindings and destination policy enforcement
- Per-session quota/rate limiting for UDP + relay→client DataChannel traffic (with optional hard-close after repeated violations)
- WebSocket UDP relay fallback (`GET /udp`) using the same datagram framing as the DataChannel
- `/metrics` Prometheus endpoint for internal counters
- Protocol documentation (`PROTOCOL.md`)
- Playwright E2E test harness (`e2e/`) that verifies Chromium ↔ relay interoperability for the `udp` DataChannel.

## Future work

- Expose metrics via a real backend (Prometheus/OTel) instead of the current in-memory counter map
- Additional policy controls (destination allowlists, per-origin restrictions, etc.)

## Ports

- **HTTP**: configurable via `--listen-addr` / `AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR`
  - Default: `127.0.0.1:8080` (local dev)
  - In containers: set `AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR=0.0.0.0:8080` (done in `docker-compose.yml`)
- **UDP (ICE / relay)**: ICE + relay UDP ports are used for WebRTC connectivity and UDP relay traffic.
  - Configure the ICE port range with `WEBRTC_UDP_PORT_MIN/MAX` (and publish/open those ports).
- **TURN (optional, docker-compose `with-turn` profile)**:
  - TURN listening port: `3478/udp`
  - TURN relayed traffic port range: `49152-49200/udp` (must match `turn/turnserver.conf`)

## Configuration

### Service config (env + flags)

The service supports configuration via environment variables and equivalent flags:

- `AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR` / `--listen-addr` (default `127.0.0.1:8080`)
- `AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL` / `--public-base-url` (optional; used for logging)
- `ALLOWED_ORIGINS` / `--allowed-origins` (optional; comma-separated browser origins)
  - Each entry must be an origin of the form `http(s)://host[:port]` (no path/query/fragment).
  - Special values: `*` (allow any origin) and `null` (allow `Origin: null`).
- `AERO_WEBRTC_UDP_RELAY_LOG_FORMAT` / `--log-format` (`text` or `json`)
- `AERO_WEBRTC_UDP_RELAY_LOG_LEVEL` / `--log-level` (`debug`, `info`, `warn`, `error`)
- `AERO_WEBRTC_UDP_RELAY_SHUTDOWN_TIMEOUT` / `--shutdown-timeout` (default `15s`)
- `AERO_WEBRTC_UDP_RELAY_ICE_GATHERING_TIMEOUT` / `--ice-gather-timeout` (default `2s`)
  - Bounds how long the non-trickle HTTP signaling endpoints (`POST /offer`, `POST /webrtc/offer`) wait for ICE gathering to complete before returning an answer SDP. If the timeout is hit, the server returns a best-effort SDP that may be missing candidates.
  - Observability: timeouts increment the `/metrics` event counter `ice_gathering_timeout`.
- `AERO_WEBRTC_UDP_RELAY_MODE` / `--mode` (`dev` or `prod`)

### Relay engine limits (env + flags)

- `MAX_UDP_BINDINGS_PER_SESSION` / `--max-udp-bindings-per-session` (default `128`)
- `UDP_BINDING_IDLE_TIMEOUT` / `--udp-binding-idle-timeout` (default `60s`)
- `UDP_INBOUND_FILTER_MODE` / `--udp-inbound-filter-mode` (default `address_and_port`)
  - `address_and_port` (recommended): only allow inbound UDP packets from remote address+port tuples that the guest has previously sent to (like a typical symmetric NAT).
  - `any`: allow inbound UDP packets from any remote endpoint (full-cone NAT behavior). **Less safe**: attackers can send arbitrary UDP traffic to the relay's UDP sockets and have it forwarded to clients.
- `UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT` / `--udp-remote-allowlist-idle-timeout` (default: `UDP_BINDING_IDLE_TIMEOUT`) — expire inactive remote allowlist entries (used when `UDP_INBOUND_FILTER_MODE=address_and_port`)
- `MAX_ALLOWED_REMOTES_PER_BINDING` / `--max-allowed-remotes-per-binding` (default `1024`) — cap the number of remote endpoints tracked in the per-binding allowlist (one allowlist per guest UDP port binding)
  - When the cap is exceeded, the relay evicts the **least recently seen** remote (oldest by last-seen timestamp).
  - DoS-hardening: bounds memory usage if a client "sprays" many destinations/remotes from a single UDP socket.
  - Observability: cap-based allowlist evictions increment the `/metrics` event counter `udp_remote_allowlist_evictions_total` (expired entries pruned by `UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT` do **not** count as evictions).
  - Observability: inbound packets dropped by allowlist filtering increment `udp_remote_allowlist_overflow_drops_total` (e.g. when the remote is not on the allowlist due to eviction or TTL expiry). These drops are **not** included in the generic `webrtc_udp_dropped*` / `udp_ws_dropped*` counters.
  - Warning: setting this too low can break legitimate UDP patterns that talk to many peers from one local port; replies from evicted remotes will be dropped until the guest sends to them again.
- `UDP_READ_BUFFER_BYTES` / `--udp-read-buffer-bytes` (default: `MAX_DATAGRAM_PAYLOAD_BYTES+1`, e.g. `1201`)
  - Must be `>= MAX_DATAGRAM_PAYLOAD_BYTES+1` so the relay can detect and drop oversized UDP datagrams instead of forwarding a silently truncated payload.
- `DATACHANNEL_SEND_QUEUE_BYTES` / `--datachannel-send-queue-bytes` (default `1048576`)
- `MAX_DATAGRAM_PAYLOAD_BYTES` / `--max-datagram-payload-bytes` (default `1200`) — max UDP payload bytes per relay frame (applies to WebRTC `udp` DataChannel and `/udp` WebSocket fallback)
- `PREFER_V2` / `--prefer-v2` (default `false`) — prefer v2 framing for relay→client packets once the client demonstrates v2 support

### L2 tunnel bridging (env + flags)

When clients create a **reliable and ordered** WebRTC DataChannel labeled `l2`, the relay can bridge it to a
backend WebSocket (typically `aero-l2-proxy`) that speaks subprotocol `aero-l2-tunnel-v1`.

Env vars / flags:

- `L2_BACKEND_WS_URL` / `--l2-backend-ws-url` (optional; when unset, `l2` DataChannels are rejected)
  - Example: `ws://127.0.0.1:8090/l2` (legacy alias: `/eth`)
- `L2_MAX_MESSAGE_BYTES` / `--l2-max-message-bytes` (default `4096`)
- `L2_BACKEND_FORWARD_ORIGIN` / `--l2-backend-forward-origin` (default: `true` when `L2_BACKEND_WS_URL` is set)
  - When enabled, the relay forwards the normalized browser `Origin` (or a derived origin from request Host/scheme)
    to the backend WebSocket dial.
- `L2_BACKEND_ORIGIN_OVERRIDE` / `--l2-backend-origin-override` (optional)
  - When set, this Origin value is used for all backend dials instead of forwarding the client origin.
- `L2_BACKEND_ORIGIN` / `--l2-backend-origin` (optional)
  - Alias for `L2_BACKEND_ORIGIN_OVERRIDE` / `--l2-backend-origin-override`.
- `L2_BACKEND_WS_ORIGIN` / `--l2-backend-ws-origin` (optional)
  - Static Origin header value to send when dialing the backend WebSocket. If
    `L2_BACKEND_ORIGIN_OVERRIDE` / `L2_BACKEND_ORIGIN` is set, it takes priority.
- `L2_BACKEND_TOKEN` / `--l2-backend-token` (optional)
  - Static token to present to the backend as an additional offered WebSocket
    subprotocol `aero-l2-token.<token>` (alongside `aero-l2-tunnel-v1`).
- `L2_BACKEND_WS_TOKEN` / `--l2-backend-ws-token` (optional)
  - Alias for `L2_BACKEND_TOKEN` / `--l2-backend-token`.
- `L2_BACKEND_AUTH_FORWARD_MODE` / `--l2-backend-auth-forward-mode` (default `query`)
  - `none`: do not forward auth credentials
  - `query`: append `token=<credential>` and `apiKey=<credential>` query parameters when dialing the backend
  - `subprotocol`: offer `Sec-WebSocket-Protocol: aero-l2-tunnel-v1, aero-l2-token.<credential>`
    (credential must be valid for `Sec-WebSocket-Protocol`, i.e. an HTTP token / RFC 7230 `tchar`)
- `L2_BACKEND_FORWARD_AERO_SESSION` / `--l2-backend-forward-aero-session` (default `false`)
  - When enabled, the relay extracts the caller's `aero_session` cookie from signaling and forwards **only** that cookie to the backend as `Cookie: aero_session=<value>`.
  - This supports running `aero-l2-proxy` in session-cookie auth mode while the browser uses the WebRTC transport.

The relay requires that the backend negotiates `aero-l2-tunnel-v1` (strict).

The forwarded `<credential>` is the same JWT or API key that authenticated the relay's signaling endpoints
(`AUTH_MODE`). When `AUTH_MODE=none`, no credential is forwarded.

### L2 bridge metrics

The relay exposes internal event counters via `GET /metrics` as:

`aero_webrtc_udp_relay_events_total{event="<name>"} <value>`

For L2 tunnel bridging, the relevant counters include:

- `l2_bridge_dials_total`, `l2_bridge_dial_errors_total`
- `l2_bridge_messages_from_client_total`, `l2_bridge_messages_to_client_total`
- `l2_bridge_bytes_from_client_total`, `l2_bridge_bytes_to_client_total`
- `l2_bridge_dropped_oversized_total` (message exceeded `L2_MAX_MESSAGE_BYTES`)
- `l2_bridge_dropped_rate_limited_total` (relay→client frame dropped by per-session DataChannel quota)

### L2 bridge logs

For production debugging, the relay emits structured logs (via `log/slog`) for key L2 bridge lifecycle events:

- `l2_bridge_backend_dial_succeeded` (INFO): backend WebSocket dial succeeded
  - fields: `session_id`, `backend_ws_url` (query/userinfo stripped), `dial_duration_ms`
- `l2_bridge_backend_dial_failed` (WARN): backend WebSocket dial failed
  - fields: `session_id`, `backend_ws_url` (query/userinfo stripped), `dial_duration_ms`, optional `status_code`, `err` (redacted)
- `l2_bridge_shutdown` (INFO/WARN): bridge torn down (e.g. backend closed, datachannel closed, oversized message)
  - fields: `reason`, `session_id`, optional `direction`, `msg_bytes`, `max_bytes`, optional `ws_close_code`, `ws_close_text` (redacted), `err` (redacted)
- `l2_bridge_dropped_rate_limited` (WARN, emitted once per bridge): backend→client frame dropped by DataChannel quota
  - fields: `session_id`, `msg_bytes`

Note: logs are best-effort redacted to avoid leaking credentials (query tokens, forwarded cookies, etc.).

### Quota + rate limiting (env + flags)

Per-session quotas and rate limits are enforced on the **data plane** (WebRTC DataChannel ↔ UDP):

- `MAX_SESSIONS` / `--max-sessions` (default `0` = unlimited)
- `SESSION_PREALLOC_TTL` / `--session-prealloc-ttl` (default `60s`) — TTL for `POST /session` preallocated session reservations (auto-released if not used)
- `MAX_UDP_PPS_PER_SESSION` / `--max-udp-pps-per-session` (default `0` = unlimited) — outbound UDP packets/sec per session
- `MAX_UDP_BPS_PER_SESSION` / `--max-udp-bps-per-session` (default `0` = unlimited) — outbound UDP bytes/sec per session
- `MAX_UDP_PPS_PER_DEST` / `--max-udp-pps-per-dest` (default `0` = unlimited) — outbound UDP packets/sec per destination per session
- `MAX_UDP_DEST_BUCKETS_PER_SESSION` / `--max-udp-dest-buckets-per-session` (default `1024`; defaults to `MAX_UNIQUE_DESTINATIONS_PER_SESSION` when that is set) — cap the number of per-destination token buckets kept per session (only used when `MAX_UDP_PPS_PER_DEST` is enabled)
  - When the cap is exceeded, the relay evicts the **least recently used (LRU)** destination bucket.
  - DoS-hardening: bounds per-session memory usage under destination spray while per-destination rate limiting is enabled.
  - Observability: bucket evictions increment the `/metrics` event counter `udp_per_dest_bucket_evictions`.
  - Warning: setting this too low can cause frequent bucket churn for legitimate clients that talk to many UDP destinations, making per-destination rate limiting less predictable/consistent.
- `MAX_UNIQUE_DESTINATIONS_PER_SESSION` / `--max-unique-destinations-per-session` (default `0` = unlimited)
- `MAX_DC_BPS_PER_SESSION` / `--max-dc-bps-per-session` (default `0` = unlimited) — relay→client DataChannel bytes/sec per session
- `HARD_CLOSE_AFTER_VIOLATIONS` / `--hard-close-after-violations` (default `0` = disabled) — close the session after N rate/quota violations
- `VIOLATION_WINDOW_SECONDS` / `--violation-window` (default `10s`) — sliding window for `HARD_CLOSE_AFTER_VIOLATIONS`

When a session is hard-closed, the relay terminates the associated WebRTC PeerConnection.

### Signaling config

#### Authentication

Because browsers can't set arbitrary headers on WebSocket upgrade requests, the signaling server
supports multiple auth delivery options for WebSocket endpoints including `/webrtc/signal` and the
`/udp` data plane fallback:

1. **Best for non-browser clients:** send credentials in the WebSocket upgrade request headers (same as HTTP endpoints).

2. **Preferred for browser clients:** send credentials in the first WebSocket message:

```json
{"type":"auth","apiKey":"..."}
```

or:

```json
{"type":"auth","token":"..."}
```

Some clients send both `apiKey` and `token` for compatibility. If both are provided, they must match.

3. **Alternative:** include credentials in the WebSocket URL query string:

- `AUTH_MODE=none` → no credentials required
- `AUTH_MODE=api_key` → `?apiKey=...` (or `?token=...` for compatibility)
- `AUTH_MODE=jwt` → `?token=...` (or `?apiKey=...` for compatibility)

Tradeoff: query parameters can leak into browser history, reverse-proxy logs, and monitoring.
Prefer the first-message `{type:"auth"}` flow when possible.

For HTTP endpoints (`GET /webrtc/ice`, `POST /offer`, `POST /webrtc/offer`, `POST /session`), clients can use headers:

- `AUTH_MODE=api_key`:
  - Preferred: `X-API-Key: ...`
  - Alternative: `Authorization: ApiKey ...`
  - Compatibility: `Authorization: Bearer ...`
  - Fallback: `?apiKey=...` (or `?token=...` for compatibility)
- `AUTH_MODE=jwt`:
  - Preferred: `Authorization: Bearer ...`
  - Compatibility: `X-API-Key: ...` or `Authorization: ApiKey ...`
  - Fallback: `?token=...` (or `?apiKey=...` for compatibility)

#### Auth & resource limits

To prevent resource exhaustion:

- Unauthenticated WebSocket connections must authenticate within `SIGNALING_AUTH_TIMEOUT` (default: `2s`).
- After authentication, WebSocket connections are kept alive with ping/pong and closed if they go idle:
  - `SIGNALING_WS_IDLE_TIMEOUT` / `SIGNALING_WS_PING_INTERVAL` apply to `GET /webrtc/signal` (`SIGNALING_WS_PING_INTERVAL` must be < `SIGNALING_WS_IDLE_TIMEOUT`).
  - `UDP_WS_IDLE_TIMEOUT` / `UDP_WS_PING_INTERVAL` apply to `GET /udp` (`UDP_WS_PING_INTERVAL` must be < `UDP_WS_IDLE_TIMEOUT`).
- Incoming WebSocket control messages (e.g. signaling, `/udp` auth) are limited by:
  - `MAX_SIGNALING_MESSAGE_BYTES` (default: `65536`)
  - `MAX_SIGNALING_MESSAGES_PER_SECOND` (default: `50`)

When `AUTH_MODE=jwt`, the relay currently enforces **at most one active relay
session per JWT `sid` at a time**. A concurrent attempt will fail with
`session_already_active` (`409 Conflict` on HTTP endpoints, or a WebSocket
`{type:"error"}` message). Clients should close the old session (PeerConnection
or `/udp` WebSocket) and retry once it is torn down.

This check is keyed by the JWT `sid` claim (not the raw JWT string), so minting a
new JWT with the same `sid` does not allow a second concurrent session.

Violations close the WebSocket connection with an error.

#### Env vars

- `AUTH_MODE` (`none`, `api_key`, or `jwt`)
- `API_KEY` (used when `AUTH_MODE=api_key`)
- `JWT_SECRET` (used when `AUTH_MODE=jwt`, HS256)
- `SIGNALING_AUTH_TIMEOUT` (Go duration, e.g. `2s`)
- `SIGNALING_WS_IDLE_TIMEOUT` (Go duration, default: `60s`) — close idle `/webrtc/signal` WebSocket connections
- `SIGNALING_WS_PING_INTERVAL` (Go duration, default: `20s`) — send ping frames on `/webrtc/signal` WebSocket connections (must be < `SIGNALING_WS_IDLE_TIMEOUT`)
- `MAX_SIGNALING_MESSAGE_BYTES`
- `MAX_SIGNALING_MESSAGES_PER_SECOND`
- `UDP_WS_IDLE_TIMEOUT` (Go duration, default: `60s`) — close idle `/udp` WebSocket connections
- `UDP_WS_PING_INTERVAL` (Go duration, default: `20s`) — send ping frames on `/udp` WebSocket connections (must be < `UDP_WS_IDLE_TIMEOUT`)

### WebRTC / ICE config

The container + client integration uses the following environment variables and equivalent flags:

- `ALLOWED_ORIGINS`: CORS allow-list for browser clients (comma-separated).
  - Example: `http://localhost:5173,http://localhost:3000`
  - If unset, the relay defaults to allowing only same host:port requests, so sensitive endpoints like `/webrtc/ice` and WebSocket upgrades (`/webrtc/signal`, `/udp`) are not exposed cross-origin.
  - Entries must be `http(s)://host[:port]` (no path/query/fragment). `*` and `null` are also accepted.
- `WEBRTC_UDP_PORT_MIN` / `WEBRTC_UDP_PORT_MAX`: UDP port range used for ICE candidates.
  - Must match your firewall rules and any container port publishing (see below).
  - The relay requires a minimum of 100 ports when a range is configured (rule of thumb: ~100 UDP ports per ~50 concurrent sessions).
  - The provided `docker-compose.yml` defaults the relay to `50000-50100/udp` to avoid colliding
    with the coturn relay range (`49152-49200/udp`).
- `WEBRTC_SESSION_CONNECT_TIMEOUT` / `--webrtc-session-connect-timeout` (Go duration, default `30s`) — max time to wait for newly created WebRTC sessions to connect before the relay closes the server-side PeerConnection.
  - Applies to sessions created via signaling endpoints (`GET /webrtc/signal`, `POST /webrtc/offer`, `POST /offer`, etc.).
  - A session is considered "connected" once **either**:
    - the WebRTC PeerConnection state becomes `connected` (`PeerConnectionStateConnected`), or
    - the ICE connection state becomes `connected` or `completed` (`ICEConnectionStateConnected` / `ICEConnectionStateCompleted`).
  - Observability: timeouts increment the `/metrics` event counter `webrtc_session_connect_timeout`.
  - Warning: setting this too low can break slow networks / delayed ICE or TURN negotiation.
- `WEBRTC_UDP_LISTEN_IP`: local IP address to bind ICE UDP sockets to (default `0.0.0.0`, meaning "use library defaults / all interfaces").
- `WEBRTC_NAT_1TO1_IPS`: comma-separated public IPs to advertise for ICE when the relay is behind NAT.
- `WEBRTC_NAT_1TO1_IP_CANDIDATE_TYPE`: `host` or `srflx` (default: `host`).
- WebRTC DataChannel hardening (pion/SCTP caps; mitigate oversized message DoS):
  - `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` (default: derived from `MAX_DATAGRAM_PAYLOAD_BYTES` and `L2_MAX_MESSAGE_BYTES`)
  - `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES` (default: derived; must be ≥ `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` and ≥ `1500`)
- `AERO_ICE_SERVERS_JSON`: JSON string describing ICE servers that the relay advertises to clients.
  - Flag: `--ice-servers-json`
  - For the `with-turn` profile, `docker-compose.yml` sets this automatically to point at the
    local coturn instance (and uses `TURN_PUBLIC_HOST` for the hostname/IP).
- Convenience env vars (used only when `AERO_ICE_SERVERS_JSON`/`--ice-servers-json` are unset):
  - `AERO_STUN_URLS` / `--stun-urls` (comma-separated)
  - `AERO_TURN_URLS` / `--turn-urls` (comma-separated)
  - `AERO_TURN_USERNAME` / `--turn-username`
  - `AERO_TURN_CREDENTIAL` / `--turn-credential`
- TURN REST (optional; used to inject short-lived TURN credentials into `/webrtc/ice` responses):
  - `TURN_REST_SHARED_SECRET` (required to enable)
  - `TURN_REST_TTL_SECONDS` (default `3600`)
  - `TURN_REST_USERNAME_PREFIX` (default `aero`)
  - `TURN_REST_REALM` (optional; for coturn documentation/config parity)

Equivalent flags:

- `--webrtc-udp-port-min` / `--webrtc-udp-port-max`
- `--webrtc-session-connect-timeout`
- `--webrtc-udp-listen-ip`
- `--webrtc-nat-1to1-ips`
- `--webrtc-nat-1to1-ip-candidate-type`
- `--webrtc-datachannel-max-message-bytes`
- `--webrtc-sctp-max-receive-buffer-bytes`

#### DataChannel hardening (oversized message DoS)

The relay enforces `MAX_DATAGRAM_PAYLOAD_BYTES` and `L2_MAX_MESSAGE_BYTES` in its application-level frame
decoders, but malicious peers can attempt to send extremely large WebRTC DataChannel messages that may be
fully buffered by pion before `DataChannel.OnMessage` is invoked.

To mitigate this, the relay configures hard caps in pion's `SettingEngine`:

- `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` / `--webrtc-datachannel-max-message-bytes` (0 = auto):
  advertised DataChannel max message size (`a=max-message-size`) used by well-behaved peers to avoid
  sending oversized messages. The default is computed as:
  `max(MAX_DATAGRAM_PAYLOAD_BYTES + 24, L2_MAX_MESSAGE_BYTES) + 256` (`24` is the v2 IPv6 UDP relay frame overhead; see `internal/udpproto.MaxFrameOverheadBytes`).
- `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES` / `--webrtc-sctp-max-receive-buffer-bytes` (0 = auto):
  SCTP receive buffer cap (hard receive-side buffering bound; must be ≥ `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` and ≥ `1500`).
  When set to `0` (auto), the default is `max(1048576, 2*WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES)`.

Oversized messages that exceed the SCTP receive buffer cap cannot be fully reassembled and are not delivered to
application-level `DataChannel.OnMessage` handlers. If the SCTP/DataChannel stack reports an error, the relay closes
the session.

Observability (via `GET /metrics`, exposed as `aero_webrtc_udp_relay_events_total{event="<name>"}`):

- `webrtc_datachannel_udp_message_too_large` / `webrtc_datachannel_l2_message_too_large`: a peer sent a WebRTC
  DataChannel message larger than `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` (likely ignoring SDP); the relay closes the
  entire WebRTC session.
- `webrtc_udp_dropped_oversized`: a peer sent an oversized `udp` DataChannel frame larger than the UDP relay framing
  maximum (`MAX_DATAGRAM_PAYLOAD_BYTES` + protocol overhead); the relay drops it and closes the `udp` DataChannel.

#### Example: behind NAT (private IP + known public IP)

```bash
export WEBRTC_UDP_LISTEN_IP=10.0.0.5
export WEBRTC_NAT_1TO1_IPS=203.0.113.10
export WEBRTC_NAT_1TO1_IP_CANDIDATE_TYPE=host
```

Example `AERO_ICE_SERVERS_JSON`:

```json
[
  { "urls": ["stun:stun.l.google.com:19302"] },
  {
    "urls": ["turn:example.com:3478?transport=udp"],
    "username": "aero",
    "credential": "aero"
  }
]
```

When TURN REST is enabled, TURN servers may omit `username` and `credential` in
the configured ICE list; the relay will inject them dynamically when responding
to `GET /webrtc/ice`.

Note: if the relay is publicly reachable and has direct UDP connectivity, host/public ICE candidates may be sufficient. STUN/TURN becomes important when clients or the relay are behind NAT/firewalls.

#### Example: development (public STUN only)

```bash
export AERO_ICE_SERVERS_JSON='[
  { "urls": ["stun:stun.l.google.com:19302"] }
]'
```

#### Example: production (coturn TURN)

```bash
export AERO_ICE_SERVERS_JSON='[
  {
    "urls": ["turn:turn.example.com:3478?transport=udp"],
    "username": "aero",
    "credential": "REPLACE_WITH_SECRET"
  }
]'
```

If using TURN REST credentials:

```bash
export TURN_REST_SHARED_SECRET='REPLACE_WITH_SHARED_SECRET'
export AERO_ICE_SERVERS_JSON='[
  { "urls": ["turn:turn.example.com:3478?transport=udp"] }
]'
```

### Destination policy (UDP egress)

The relay is **network egress**. If you run it on an Internet-reachable host without destination
controls, it can become an **open proxy / SSRF primitive** that attackers can use to:

- scan internal networks (`10.0.0.0/8`, `192.168.0.0/16`, etc.)
- hit cloud metadata endpoints
- attack link-local services
- abuse your host as a generic UDP reflector

To mitigate this, the relay enforces an outbound destination policy
(`internal/policy.DestinationPolicy`) on **every outbound UDP datagram** (and can also drop inbound
datagrams from denied sources).

#### Safe defaults

By default, the policy is **deny-by-default** and denies common private/special IPv4 ranges unless
explicitly enabled.

In other words: if you deploy the relay without any configuration, it should **not** be able to
reach arbitrary network targets.

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

## Why UDP port ranges matter

WebRTC uses ICE candidates, which ultimately require **UDP ports on the server to be reachable
from the browser**.

There are two independent UDP ranges to keep aligned:

1. **Relay ICE ports**: the port range the relay itself binds to (controlled by
   `WEBRTC_UDP_PORT_MIN/MAX`).
2. **TURN relay ports** (when using coturn): the port range coturn allocates for relayed
   connections (`min-port`/`max-port` in `turnserver.conf`).

If the relay is configured to allocate ports in `[WEBRTC_UDP_PORT_MIN, WEBRTC_UDP_PORT_MAX]`,
you must:

1. **Open** that UDP range in your firewall/security group.
2. **Publish** that UDP range to the container (Docker) or hostPorts (Kubernetes), matching the
   same numeric range.
3. Keep the range aligned everywhere (relay config, TURN config, container ports).

Example (Docker):

```bash
docker run -p 50000-50100:50000-50100/udp ...
```

Example (UFW):

```bash
sudo ufw allow 50000:50100/udp
```

If you change one side (e.g. `turnserver.conf` uses `min-port=52000`) without updating the
published ports, ICE will fail in non-obvious ways.

## Smoke verification (manual)

### 1) HTTP liveness

```bash
curl -v http://localhost:8080/healthz
```

### 2) Browser sanity check (TURN candidate)

Open your browser DevTools console and run:

```js
// Replace with your own if not using docker-compose defaults.
const iceServers = [
  { urls: ["stun:stun.l.google.com:19302"] },
  {
    urls: ["turn:localhost:3478?transport=udp"],
    username: "aero",
    credential: "aero",
  },
];

const pc = new RTCPeerConnection({ iceServers });
pc.createDataChannel("smoke");

pc.onicecandidate = (e) => {
  if (e.candidate) console.log("ICE:", e.candidate.candidate);
};

await pc.setLocalDescription(await pc.createOffer());
```

When running with `--profile with-turn`, you should see at least one ICE candidate containing
`typ relay` (a relayed candidate via TURN). If you only see `typ host` candidates, TURN may not
be reachable or may be advertising the wrong external address.

## Security model (read before deploying the relay)

See "Destination policy (UDP egress)" above.

### Startup security warnings

On startup, the relay logs `WARN` lines with prefix `startup security warning:` when it detects
potentially unsafe or suspicious configurations (defense in depth). These warnings are **not fatal**
but are intended to help operators avoid accidental open-proxy or DoS-prone deployments.

Examples of conditions that trigger warnings include:

- `AUTH_MODE=none` (unauthenticated relay)
- `ALLOWED_ORIGINS` contains `*` (allows any Origin)
- `UDP_INBOUND_FILTER_MODE=any` (full-cone inbound UDP; less safe)
- `DESTINATION_POLICY_PRESET=dev` or `ALLOW_PRIVATE_NETWORKS=true` in `--mode=prod` (broad UDP egress)
- `MAX_SESSIONS=0` in `--mode=prod` (unlimited sessions)
- `L2_BACKEND_AUTH_FORWARD_MODE=query` when relay auth is enabled (forwards credentials via query params)
- Unusually large WebRTC/SCTP caps or timeouts (weakens oversized message DoS hardening):
  - `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES`
  - `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES`
  - `WEBRTC_SESSION_CONNECT_TIMEOUT`

## Build verification

```bash
cd proxy/webrtc-udp-relay
make docker-build
# or:
docker build -f Dockerfile .
```
