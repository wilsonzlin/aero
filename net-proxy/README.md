# Aero Network Proxy (`net-proxy`)

`net-proxy` is a standalone WebSocket → TCP/UDP relay service (plus lightweight DNS-over-HTTPS endpoints) that enables
browser-based “guest” networking.

The browser connects to this service via WebSocket, and the proxy opens real TCP/UDP sockets from the server to the requested target.

## Running locally

```bash
# From the repo root (npm workspaces)
npm ci

# Safe-by-default: only allows public/unicast targets.
npm -w net-proxy run dev
```

Health check:

```bash
curl http://127.0.0.1:8081/healthz
```

Metrics (Prometheus text format):

```bash
curl http://127.0.0.1:8081/metrics
```

### DNS-over-HTTPS (DoH) endpoints

`net-proxy` also implements two DNS-over-HTTPS endpoints that are compatible with the production gateway API:

- `GET|POST /dns-query` — RFC 8484 DoH (`application/dns-message`)
- `GET /dns-json` — JSON DoH (`application/dns-json`, Cloudflare-DNS-JSON compatible)

This lets you run local networking (TCP + UDP + DNS) without standing up the full `backend/aero-gateway` service.

#### DoH behavior / limitations

These endpoints are intended for local development and are intentionally lightweight:

- `/dns-query` supports `A` (type `1`) and `AAAA` (type `28`) questions.
  - Other QTYPEs return `NOERROR` with no answers.
- `/dns-json` supports `A`, `AAAA`, and `CNAME`.
  - Other types return `400`.
- TTLs are synthetic (configured by `AERO_PROXY_DOH_ANSWER_TTL_SECONDS`), not authoritative upstream TTLs.

#### Browser client configuration (what URL to set)

Use the **HTTP base URL** for `net-proxy`:

- `http://127.0.0.1:8081`

This is also the value you typically set for Aero’s `proxyUrl` (Settings panel → “Proxy URL”, or URL query param
`?proxy=http%3A%2F%2F127.0.0.1%3A8081`).

If you're using the browser networking clients in `web/src/net`:

- Pass this base URL to the TCP/UDP WebSocket clients (they auto-convert `http://` → `ws://`).
- For DoH, prefer a **same-origin** path (`/dns-query`) to avoid CORS (e.g. via a frontend dev-server proxy).
  - If your frontend is served from the same origin as `net-proxy` (or you have permissive CORS), you can also use an
    absolute DoH URL.

```ts
// Browser-side networking helpers.
// In this repo they live under `web/src/net` (e.g. `src/main.ts` imports from `../web/src/...`).
// Adjust the import path for your setup.
import { WebSocketTcpProxyClient, WebSocketUdpProxyClient, resolveAOverDoh } from "../web/src/net";

const proxyUrl = "http://127.0.0.1:8081";

const tcp = new WebSocketTcpProxyClient(proxyUrl, (ev) => console.log("tcp", ev));
const udp = new WebSocketUdpProxyClient(proxyUrl, (ev) => console.log("udp", ev));

// Same-origin DoH (recommended; e.g. via Vite proxy below):
const result = await resolveAOverDoh("localhost", "/dns-query");

// Or: absolute DoH (requires same-origin or permissive CORS):
// const result = await resolveAOverDoh("localhost", new URL("/dns-query", proxyUrl).toString());
console.log(result);
```

> Tip: prefer an `http://` base URL (not `ws://`) so the same value works for both WebSocket and `fetch()`-based DNS.
>
> Browser note: DoH requests are normal `fetch()` calls, so you generally want `/dns-query` and `/dns-json` to be
> **same-origin** with your frontend (or be served with permissive CORS headers).
>
> If you're using the repo-root Vite dev server (`npm run dev`, default `http://localhost:5173`), the simplest approach
> is to proxy the DoH paths through Vite so they become same-origin (then your browser code can just use `/dns-query`
> and `/dns-json`, i.e. avoid `http://127.0.0.1:8081/...` URLs that would trigger CORS):
>
> ```ts
> // vite.harness.config.ts
> // Add this under `server` (alongside the existing `headers` config):
> server: {
>   proxy: {
>     "/dns-query": "http://127.0.0.1:8081",
>     "/dns-json": "http://127.0.0.1:8081",
>   },
> },
> ```
>
> Alternatively, you can enable CORS on `net-proxy`’s DoH endpoints directly by setting
> `AERO_PROXY_DOH_CORS_ALLOW_ORIGINS` (see config table below). This should generally be limited to your local dev
> origin(s) (e.g. `http://localhost:5173`), not `*`:
>
> ```bash
> AERO_PROXY_DOH_CORS_ALLOW_ORIGINS='http://localhost:5173' npm -w net-proxy run dev
> ```
>
> Note: some browsers enforce **Private Network Access (PNA)** when a secure context (`https://...`) fetches a
> `http://127.0.0.1/...` endpoint. When the CORS allowlist is enabled, `net-proxy` responds to PNA preflight requests
> by returning `Access-Control-Allow-Private-Network: true`.
>
> For convenience, `net-proxy` also:
>
> - caches preflight results (`Access-Control-Max-Age: 600`), and
> - exposes `Content-Length` so cross-origin clients can read it (`Access-Control-Expose-Headers: Content-Length`).

#### Security caveats (open mode vs allowlist)

`net-proxy` is a **local-dev** relay and is intentionally lightweight:

- `/dns-query` and `/dns-json` are **unauthenticated** (no session cookie) and do **not** currently apply the
  `AERO_PROXY_OPEN` / `AERO_PROXY_ALLOW` target policy.
  - They resolve using the host’s DNS resolver and may return private/localhost answers (e.g. `localhost → 127.0.0.1`)
    even when `AERO_PROXY_OPEN=0`.
- The **connection** endpoints (`/tcp`, `/tcp-mux`, `/udp`) *do* enforce the policy:
  - **Default (safe-by-default):** only permits targets that resolve to public/unicast IPs.
  - **Allowlist mode:** `AERO_PROXY_ALLOW` can opt specific targets in (use explicit IP/CIDR entries to allow private ranges).
  - **Open mode:** `AERO_PROXY_OPEN=1` disables restrictions (trusted local dev only).

Do not expose a dev `net-proxy` instance to the public internet; it is an outbound network proxy with an unauthenticated DoH surface.

#### Example `curl` roundtrips

`/dns-json` (human readable):

```bash
curl -sS 'http://127.0.0.1:8081/dns-json?name=localhost&type=A' \
  -H 'accept: application/dns-json'
```

`/dns-query` (binary DNS message; `dns` query parameter is base64url-encoded wire bytes for `A localhost`):

```bash
curl -sS 'http://127.0.0.1:8081/dns-query?dns=AAABAAABAAAAAAAACWxvY2FsaG9zdAAAAQAB' \
  -H 'accept: application/dns-message' \
  | hexdump -C | head
```

`POST /dns-query` (same query, sent as an `application/dns-message` request body):

```bash
node -e 'process.stdout.write(Buffer.from("AAABAAABAAAAAAAACWxvY2FsaG9zdAAAAQAB", "base64url"))' \
  | curl -sS -X POST 'http://127.0.0.1:8081/dns-query' \
    -H 'content-type: application/dns-message' \
    -H 'accept: application/dns-message' \
    --data-binary @- \
  | hexdump -C | head
```

See also: [`docs/07-networking.md`](../docs/07-networking.md) (where DoH fits into the overall networking stack).

### Open mode (trusted local development)

To allow connections to `127.0.0.1`, RFC1918, etc:

```bash
AERO_PROXY_OPEN=1 npm -w net-proxy run dev
```

## Configuration

Environment variables:

| Variable | Default | Description |
| --- | ---: | --- |
| `AERO_PROXY_LISTEN_HOST` | `127.0.0.1` | Address to bind the HTTP/WebSocket server to |
| `AERO_PROXY_PORT` | `8081` | Port to listen on |
| `AERO_PROXY_OPEN` | `0` | Set to `1` to disable target restrictions |
| `AERO_PROXY_ALLOW` | (empty) | Comma-separated allowlist rules, e.g. `example.com:80,example.com:443,10.0.0.0/8:53` |
| `AERO_PROXY_CONNECT_TIMEOUT_MS` | `10000` | TCP connect timeout |
| `AERO_PROXY_DNS_TIMEOUT_MS` | `5000` | DNS lookup timeout |
| `AERO_PROXY_DOH_MAX_QUERY_BYTES` | `512` | Max `/dns-query` DNS message size (decoded GET `dns` param or POST body), in bytes |
| `AERO_PROXY_DOH_MAX_QNAME_LENGTH` | `253` | Max DoH query name length (bytes) |
| `AERO_PROXY_DOH_ANSWER_TTL_SECONDS` | `60` | TTL seconds used for DoH answers (clamped to `AERO_PROXY_DOH_MAX_ANSWER_TTL_SECONDS`) |
| `AERO_PROXY_DOH_MAX_ANSWER_TTL_SECONDS` | `300` | Max TTL seconds allowed for DoH answers |
| `AERO_PROXY_DOH_MAX_ANSWERS` | `16` | Max number of A/AAAA answers returned per DoH query |
| `AERO_PROXY_DOH_CORS_ALLOW_ORIGINS` | (empty) | Comma-separated CORS origin allowlist for `/dns-query` and `/dns-json` (e.g. `http://localhost:5173`). Use `*` only in trusted local dev. Includes basic Private Network Access (PNA) preflight support (`Access-Control-Allow-Private-Network`), preflight caching (`Access-Control-Max-Age`), and exposes `Content-Length` (`Access-Control-Expose-Headers`). |
| `AERO_PROXY_WS_MAX_PAYLOAD_BYTES` | `1048576` | Maximum incoming WebSocket message size |
| `AERO_PROXY_WS_STREAM_HWM_BYTES` | `65536` | Backpressure tuning for the TCP WebSocket stream bridge |
| `AERO_PROXY_UDP_WS_BUFFER_LIMIT_BYTES` | `1048576` | Drop inbound UDP packets when WebSocket bufferedAmount exceeds this limit |
| `AERO_PROXY_TCP_MUX_MAX_STREAMS` | `1024` | Max concurrent multiplexed TCP streams per WebSocket (`/tcp-mux`) |
| `AERO_PROXY_TCP_MUX_MAX_STREAM_BUFFER_BYTES` | `1048576` | Max buffered client→TCP bytes per mux stream before the stream is closed |
| `AERO_PROXY_TCP_MUX_MAX_FRAME_PAYLOAD_BYTES` | `16777216` | Max `aero-tcp-mux-v1` frame payload size |
| `AERO_PROXY_UDP_RELAY_MAX_PAYLOAD_BYTES` | `1200` | Max UDP payload bytes per framed datagram in multiplexed `/udp` mode (v1/v2 framing) |
| `AERO_PROXY_UDP_RELAY_MAX_BINDINGS` | `128` | Max UDP bindings per WebSocket connection in multiplexed `/udp` mode |
| `AERO_PROXY_UDP_RELAY_BINDING_IDLE_TIMEOUT_MS` | `60000` | Idle timeout for UDP bindings in multiplexed `/udp` mode |
| `AERO_PROXY_UDP_RELAY_PREFER_V2` | `0` | Prefer emitting v2 frames for IPv4 once the client has sent at least one v2 frame (IPv6 always uses v2) |
| `AERO_PROXY_UDP_RELAY_INBOUND_FILTER_MODE` | `address_and_port` | In multiplexed `/udp` mode, accept inbound UDP packets from `any` remote, or only from previously-contacted `address_and_port` remotes |

Allowlist rules are `hostOrCidr:port` (port can be `*` or a range like `8000-9000`). Domains can use `*.example.com`.

### Safe-by-default behavior

If `AERO_PROXY_ALLOW` is empty and `AERO_PROXY_OPEN` is not set, the proxy only permits targets that resolve to **public unicast** IPs.

If `AERO_PROXY_ALLOW` contains **domain** rules, they still only apply to targets that resolve to public unicast IPs (to mitigate DNS rebinding). To allow private/localhost targets, use an explicit IP/CIDR allowlist entry (e.g. `127.0.0.1:*`) or run with `AERO_PROXY_OPEN=1`.

## Client URL formation

### TCP

WebSocket URL:

```
ws://<proxy-host>:<proxy-port>/tcp?v=1&host=<target-host>&port=<target-port>
```

Compatibility alias:

```
ws://<proxy-host>:<proxy-port>/tcp?v=1&target=<target-host>:<target-port>
```

`v=1` is optional; `net-proxy` currently ignores it but accepts it for
compatibility with the production `aero-gateway` `/tcp` URL format.

For IPv6 targets, `target` must use brackets, e.g. `target=[2606:4700:4700::1111]:443`.

Example:

```js
const ws = new WebSocket("ws://127.0.0.1:8081/tcp?v=1&host=example.com&port=80");
ws.binaryType = "arraybuffer";

ws.onmessage = (ev) => {
  const bytes = new Uint8Array(ev.data);
  // bytes received from the TCP socket
};

ws.send(new Uint8Array([1, 2, 3])); // writes to the TCP socket
```

### TCP Mux (`/tcp-mux`)

WebSocket URL:

```
ws://<proxy-host>:<proxy-port>/tcp-mux
```

The client MUST negotiate the WebSocket subprotocol:

```
Sec-WebSocket-Protocol: aero-tcp-mux-v1
```

The WebSocket carries a byte stream of `aero-tcp-mux-v1` frames. Each frame is:

- 9-byte header: `msg_type u8`, `stream_id u32 BE`, `length u32 BE`
- followed by `length` bytes of payload

Only **binary** WebSocket messages are supported (text messages are rejected with close code `1003`).

For the canonical spec (including `OPEN`/`DATA`/`CLOSE` payload schemas and error codes), see
[`docs/backend/01-aero-gateway-api.md`](../docs/backend/01-aero-gateway-api.md).

### UDP

#### Per-target UDP (legacy/raw datagrams)

WebSocket URL (connects to a single target):

```
ws://<proxy-host>:<proxy-port>/udp?v=1&host=<target-host>&port=<target-port>
```

Compatibility alias:

```
ws://<proxy-host>:<proxy-port>/udp?v=1&target=<target-host>:<target-port>
```

`v=1` is optional; `net-proxy` currently ignores it but accepts it for
compatibility with the production `aero-gateway` URL format.

Each WebSocket binary message is sent as a single UDP datagram; each received datagram is forwarded as a single WebSocket binary message.

#### Multiplexed UDP relay framing (v1/v2 datagrams over WebSocket)

For local development/testing without WebRTC, `net-proxy` also supports a **multiplexed** UDP mode:

```
ws://<proxy-host>:<proxy-port>/udp
```

When no `host`/`port` (or `target`) query parameters are present, each WebSocket binary message is interpreted as a **UDP relay datagram frame** using the same v1/v2 framing as the production relay:

- `proxy/webrtc-udp-relay/PROTOCOL.md`

This is the mode used by the browser `WebSocketUdpProxyClient`.

Security caveat:

- In multiplexed mode, allowlist checks are applied **per datagram** based on the decoded destination **IP:port**.
- Domain allowlist rules like `example.com:53` cannot be applied because frames contain IP addresses (use IP/CIDR rules like `127.0.0.1:*` / `10.0.0.0/8:*`, or run with `AERO_PROXY_OPEN=1`).
- By default, the proxy only forwards inbound UDP packets from remote endpoints that the guest has previously sent a packet to (address+port). Set `AERO_PROXY_UDP_RELAY_INBOUND_FILTER_MODE=any` to disable this.
