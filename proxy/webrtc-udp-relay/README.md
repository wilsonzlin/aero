# Aero WebRTC → UDP Relay

This directory contains a standalone Go service intended to proxy UDP between:

- the browser (guest networking stack running inside the emulator), and
- a server-side UDP relay reachable from the browser.

See `PROTOCOL.md` for the on-the-wire framing and signaling message shapes.

It also includes a turnkey container deployment story:

- `Dockerfile`: multi-stage Go build → minimal runtime image (distroless, non-root)
- `docker-compose.yml`: run the relay alone, or with a local `coturn` TURN server
- `turn/turnserver.conf`: minimal TURN config (committed)

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

## E2E interoperability test (Playwright)

`e2e/` contains a Playwright test that launches headless Chromium and exercises the WebRTC `udp` DataChannel framing against a local UDP echo server.

```bash
cd proxy/webrtc-udp-relay/e2e
npm ci
npm test
```

`npm test` runs `playwright install chromium` automatically (via `pretest`) to ensure the browser binary is available.

### System dependencies (Playwright)

On Debian/Ubuntu, Playwright can install its required shared libraries automatically:

```bash
npx playwright install --with-deps chromium
```

If Chromium fails to launch in CI, ensure the container/runner includes the Playwright Linux dependencies.

> Note: the E2E test currently runs a small Node-based relay implementation under `e2e/relay-server/` while the Go relay's WebRTC endpoints are under active development.

## HTTP endpoints

- `GET /healthz` → `{"ok":true}`
- `GET /readyz` → readiness (200 once serving, 503 during shutdown or when ICE config is invalid)
- `GET /version` → build metadata (commit/build time may be empty)
- `GET /webrtc/ice` → ICE server list for browser clients: `{"iceServers":[...]}`
  - guarded by the same origin policy as signaling endpoints (to avoid leaking TURN credentials cross-origin)

## Implemented

- Minimal production-oriented HTTP server skeleton + middleware
- Config system (env + flags): listen address, public base URL, log format/level, shutdown timeout, dev/prod mode
- WebRTC network config (env + flags): ICE UDP port range, UDP listen IP, NAT 1:1 public IP advertisement
- Configurable ICE servers (STUN/TURN) + client-facing discovery endpoint (`/webrtc/ice`)
- Relay/policy primitives (not yet wired to WebRTC signaling)
- Protocol documentation (`PROTOCOL.md`)
- Playwright E2E test harness (`e2e/`) that verifies Chromium ↔ relay interoperability for the `udp` DataChannel.

## Pending (future tasks)

- WebRTC signaling (SDP exchange, ICE candidate handling)
- WebRTC peer connection lifecycle management (`pion/webrtc`)
- WebRTC ↔ UDP data plane integration (enforcing policy on every datagram)
- Auth and additional policy controls (rate limits, allowlists, etc)

## Ports

- **HTTP**: configurable via `--listen-addr` / `AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR`
  - Default: `127.0.0.1:8080` (local dev)
  - In containers: set `AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR=0.0.0.0:8080` (done in `docker-compose.yml`)
- **UDP (ICE / relay, upcoming)**: ICE + relay UDP ports will be used once signaling and relay endpoints land.
  - Configure the ICE port range with `WEBRTC_UDP_PORT_MIN/MAX` (and publish/open those ports).
- **TURN (optional, docker-compose `with-turn` profile)**:
  - TURN listening port: `3478/udp`
  - TURN relayed traffic port range: `49152-49200/udp` (must match `turn/turnserver.conf`)

## Configuration

### Service config (env + flags)

The service supports configuration via environment variables and equivalent flags:

- `AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR` / `--listen-addr` (default `127.0.0.1:8080`)
- `AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL` / `--public-base-url` (optional; used for logging)
- `AERO_WEBRTC_UDP_RELAY_LOG_FORMAT` / `--log-format` (`text` or `json`)
- `AERO_WEBRTC_UDP_RELAY_LOG_LEVEL` / `--log-level` (`debug`, `info`, `warn`, `error`)
- `AERO_WEBRTC_UDP_RELAY_SHUTDOWN_TIMEOUT` / `--shutdown-timeout` (default `15s`)
- `AERO_WEBRTC_UDP_RELAY_MODE` / `--mode` (`dev` or `prod`)

### WebRTC / ICE config

The container + client integration uses the following environment variables and equivalent flags:

- `AUTH_MODE`: controls request authentication/authorization (implementation-defined).
- `ALLOWED_ORIGINS`: CORS allow-list for browser clients (comma-separated).
  - Example: `http://localhost:5173,http://localhost:3000`
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
docker build .
```
