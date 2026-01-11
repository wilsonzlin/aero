# Aero Network Proxy (`net-proxy`)

`net-proxy` is a standalone WebSocket → TCP/UDP relay service that enables browser-based “guest” networking.

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
| `AERO_PROXY_WS_MAX_PAYLOAD_BYTES` | `1048576` | Maximum incoming WebSocket message size |
| `AERO_PROXY_WS_STREAM_HWM_BYTES` | `65536` | Backpressure tuning for the TCP WebSocket stream bridge |
| `AERO_PROXY_UDP_WS_BUFFER_LIMIT_BYTES` | `1048576` | Drop inbound UDP packets when WebSocket bufferedAmount exceeds this limit |
| `AERO_PROXY_TCP_MUX_MAX_STREAMS` | `1024` | Max concurrent multiplexed TCP streams per WebSocket (`/tcp-mux`) |
| `AERO_PROXY_TCP_MUX_MAX_STREAM_BUFFER_BYTES` | `1048576` | Max buffered client→TCP bytes per mux stream before the stream is closed |
| `AERO_PROXY_TCP_MUX_MAX_FRAME_PAYLOAD_BYTES` | `16777216` | Max `aero-tcp-mux-v1` frame payload size |
| `AERO_PROXY_UDP_RELAY_MAX_PAYLOAD_BYTES` | `1200` | Max UDP payload bytes per framed datagram in multiplexed `/udp` mode (v1/v2 framing) |
| `AERO_PROXY_UDP_RELAY_MAX_BINDINGS` | `128` | Max UDP bindings per WebSocket connection in multiplexed `/udp` mode |
| `AERO_PROXY_UDP_RELAY_BINDING_IDLE_TIMEOUT_MS` | `60000` | Idle timeout for UDP bindings in multiplexed `/udp` mode |

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
- The proxy only forwards inbound UDP packets from remote endpoints that the guest has previously sent a packet to (address+port), to avoid acting as a full-cone UDP forwarder.
