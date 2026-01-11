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
go run ./cmd/aero-webrtc-udp-relay
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
default to same-host only).

## E2E interoperability test (Playwright)

`e2e/` contains Playwright tests that launch headless Chromium and exercise:

- the WebRTC `udp` DataChannel framing, and
- the `/udp` WebSocket UDP relay fallback,

against a local UDP echo server.

```bash
# From the repo root (npm workspaces)
npm ci
npm -w proxy/webrtc-udp-relay/e2e test
```

For local convenience, `npm test` runs `playwright install chromium` automatically (via `pretest`) to ensure the browser binary is available.
In CI (or when `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`), the `pretest` hook is a no-op and browser setup should be handled by the shared `.github/actions/setup-playwright` cache/install step.

This E2E test also requires a working Go toolchain (it builds a small local relay helper).

### System dependencies (Playwright)

On Debian/Ubuntu, Playwright can install its required shared libraries automatically:

```bash
npx playwright install --with-deps chromium
```

If Chromium fails to launch in CI, ensure the container/runner includes the Playwright Linux dependencies.

The E2E test builds and runs a small Go relay helper under `e2e/relay-server-go/` (using `pion/webrtc`) to exercise Chromium interoperability end-to-end.

## HTTP endpoints

- `GET /healthz` → `{"ok":true}`
- `GET /readyz` → readiness (200 once serving, 503 during shutdown or when ICE config is invalid)
- `GET /version` → build metadata (commit/build time may be empty)
- `GET /metrics` → Prometheus text exposition of internal counters
- `GET /webrtc/ice` → ICE server list for browser clients: `{"iceServers":[...]}`
  - guarded by the same origin policy as signaling endpoints (to avoid leaking TURN credentials cross-origin)
  - when `AUTH_MODE != none`, also requires the same credentials as signaling endpoints (to avoid leaking TURN REST credentials to unauthenticated callers)
- `POST /offer` → signaling: exchange SDP offer/answer (non-trickle ICE) per `PROTOCOL.md`
- `POST /session` → allocate a server-side session (primarily for quota enforcement; not required by the v1 offer/answer flow)
- `GET /webrtc/signal` → WebSocket signaling (trickle ICE)
- `POST /webrtc/offer` → HTTP offer → answer (non-trickle ICE fallback)
- `GET /udp` → WebSocket UDP relay fallback (binary datagram frames; see `PROTOCOL.md`)

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
- `AERO_WEBRTC_UDP_RELAY_MODE` / `--mode` (`dev` or `prod`)

### Relay engine limits (env + flags)

- `MAX_UDP_BINDINGS_PER_SESSION` / `--max-udp-bindings-per-session` (default `128`)
- `UDP_BINDING_IDLE_TIMEOUT` / `--udp-binding-idle-timeout` (default `60s`)
- `UDP_READ_BUFFER_BYTES` / `--udp-read-buffer-bytes` (default `65535`)
- `DATACHANNEL_SEND_QUEUE_BYTES` / `--datachannel-send-queue-bytes` (default `1048576`)
- `PREFER_V2` / `--prefer-v2` (default `false`) — prefer v2 framing for relay→client packets once the client demonstrates v2 support

### Quota + rate limiting (env + flags)

Per-session quotas and rate limits are enforced on the **data plane** (WebRTC DataChannel ↔ UDP):

- `MAX_SESSIONS` / `--max-sessions` (default `0` = unlimited)
- `MAX_UDP_PPS_PER_SESSION` / `--max-udp-pps-per-session` (default `0` = unlimited) — outbound UDP packets/sec per session
- `MAX_UDP_BPS_PER_SESSION` / `--max-udp-bps-per-session` (default `0` = unlimited) — outbound UDP bytes/sec per session
- `MAX_UDP_PPS_PER_DEST` / `--max-udp-pps-per-dest` (default `0` = unlimited) — outbound UDP packets/sec per destination per session
- `MAX_UNIQUE_DESTINATIONS_PER_SESSION` / `--max-unique-destinations-per-session` (default `0` = unlimited)
- `MAX_DC_BPS_PER_SESSION` / `--max-dc-bps-per-session` (default `0` = unlimited) — relay→client DataChannel bytes/sec per session
- `HARD_CLOSE_AFTER_VIOLATIONS` / `--hard-close-after-violations` (default `0` = disabled) — close the session after N rate/quota violations
- `VIOLATION_WINDOW_SECONDS` / `--violation-window` (default `10s`) — sliding window for `HARD_CLOSE_AFTER_VIOLATIONS`

When a session is hard-closed, the relay terminates the associated WebRTC PeerConnection.

### Signaling config

#### Authentication

Because browsers can't set arbitrary headers on WebSocket upgrade requests, the signaling server
supports two auth delivery options:

1. **Preferred:** send credentials in the first WebSocket message:

```json
{"type":"auth","apiKey":"..."}
```

or:

```json
{"type":"auth","token":"..."}
```

2. **Alternative:** include credentials in the WebSocket URL query string:

- `AUTH_MODE=none` → no credentials required
- `AUTH_MODE=api_key` → `?apiKey=...`
- `AUTH_MODE=jwt` → `?token=...`

Tradeoff: query parameters can leak into browser history, reverse-proxy logs, and monitoring.
Prefer the first-message `{type:"auth"}` flow when possible.

For HTTP endpoints (`GET /webrtc/ice`, `POST /offer`, `POST /webrtc/offer`, `POST /session`), clients can use headers:

- `AUTH_MODE=api_key`:
  - Preferred: `X-API-Key: ...`
  - Fallback: `?apiKey=...`
- `AUTH_MODE=jwt`:
  - Preferred: `Authorization: Bearer ...`
  - Fallback: `?token=...`

#### Auth & resource limits

To prevent resource exhaustion:

- Unauthenticated WebSocket connections must authenticate within `SIGNALING_AUTH_TIMEOUT` (default: `2s`).
- Incoming signaling messages are limited by:
  - `MAX_SIGNALING_MESSAGE_BYTES` (default: `65536`)
  - `MAX_SIGNALING_MESSAGES_PER_SECOND` (default: `50`)

Violations close the WebSocket connection with an error.

#### Env vars

- `AUTH_MODE` (`none`, `api_key`, or `jwt`)
- `API_KEY` (used when `AUTH_MODE=api_key`)
- `JWT_SECRET` (used when `AUTH_MODE=jwt`, HS256)
- `SIGNALING_AUTH_TIMEOUT` (Go duration, e.g. `2s`)
- `MAX_SIGNALING_MESSAGE_BYTES`
- `MAX_SIGNALING_MESSAGES_PER_SECOND`

### WebRTC / ICE config

The container + client integration uses the following environment variables and equivalent flags:

- `ALLOWED_ORIGINS`: CORS allow-list for browser clients (comma-separated).
  - Example: `http://localhost:5173,http://localhost:3000`
  - If unset, the relay defaults to allowing only same-host origins (so TURN credentials from `/webrtc/ice` are not exposed cross-origin).
  - Entries must be `http(s)://host[:port]` (no path/query/fragment). `*` and `null` are also accepted.
- `WEBRTC_UDP_PORT_MIN` / `WEBRTC_UDP_PORT_MAX`: UDP port range used for ICE candidates.
  - Must match your firewall rules and any container port publishing (see below).
  - The relay requires a minimum of 100 ports when a range is configured (rule of thumb: ~100 UDP ports per ~50 concurrent sessions).
  - The provided `docker-compose.yml` defaults the relay to `50000-50100/udp` to avoid colliding
    with the coturn relay range (`49152-49200/udp`).
- `WEBRTC_UDP_LISTEN_IP`: local IP address to bind ICE UDP sockets to (default `0.0.0.0`, meaning "use library defaults / all interfaces").
- `WEBRTC_NAT_1TO1_IPS`: comma-separated public IPs to advertise for ICE when the relay is behind NAT.
- `WEBRTC_NAT_1TO1_IP_CANDIDATE_TYPE`: `host` or `srflx` (default: `host`).
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
- `--webrtc-udp-listen-ip`
- `--webrtc-nat-1to1-ips`
- `--webrtc-nat-1to1-ip-candidate-type`

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

## Build verification

```bash
cd proxy/webrtc-udp-relay
make docker-build
# or:
docker build -f Dockerfile .
```
