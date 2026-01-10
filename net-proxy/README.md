# Aero Network Proxy (`net-proxy`)

`net-proxy` is a standalone WebSocket → TCP/UDP relay service that enables browser-based “guest” networking.

The browser connects to this service via WebSocket, and the proxy opens real TCP/UDP sockets from the server to the requested target.

## Running locally

```bash
cd net-proxy
npm ci

# Safe-by-default: only allows public/unicast targets.
npm run dev
```

Health check:

```bash
curl http://127.0.0.1:8081/healthz
```

### Open mode (trusted local development)

To allow connections to `127.0.0.1`, RFC1918, etc:

```bash
AERO_PROXY_OPEN=1 npm run dev
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

### UDP

WebSocket URL:

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
