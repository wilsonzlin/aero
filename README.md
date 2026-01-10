# Aero (design docs + browser PoCs)

This repository contains Aero’s architecture/design documentation plus browser-side
proofs-of-concept and web scaffolding used to validate feasibility constraints.

## Documentation

- Architecture & subsystem docs: [`AGENTS.md`](./AGENTS.md)
- Deployment/hosting (COOP/COEP, SharedArrayBuffer/WASM threads): [`docs/deployment.md`](./docs/deployment.md)

## Web (Vite)

The `web/` app is configured for **cross-origin isolation** in both dev and preview mode.

```sh
cd web
npm install
npm run dev
```

## Optional guest networking support (TCP/UDP via WebSocket relay)

Browsers cannot open arbitrary TCP/UDP sockets directly. For guest networking, Aero can use a small local proxy that exposes WebSocket endpoints and relays to real TCP/UDP sockets from the server side.

This repo includes a standalone proxy service at [`net-proxy/`](./net-proxy/).

### Local dev workflow (run alongside Vite)

Terminal 1 (network proxy):

```bash
cd net-proxy
npm ci

# Trusted local development mode: allows localhost + private ranges.
AERO_PROXY_OPEN=1 npm run dev
```

Terminal 2 (frontend):

```bash
cd web
npm ci
npm run dev
```

The proxy exposes:

- `GET /healthz`
- `WS /tcp?host=<host>&port=<port>` (or `?target=<host>:<port>`)
- `WS /udp?host=<host>&port=<port>` (or `?target=<host>:<port>`)

See `net-proxy/README.md` for allowlisting and client URL examples.

## Browser memory model PoC (SharedArrayBuffer + WebAssembly.Memory)

Modern browsers impose practical limits around **wasm32** addressability and `SharedArrayBuffer` usage:

- `SharedArrayBuffer` requires a **cross-origin isolated** page (`COOP` + `COEP` response headers).
- `WebAssembly.Memory` (wasm32) is **≤ 4GiB** addressable, and many browsers cap shared memories below that in practice.

This PoC allocates a configurable-size shared `WebAssembly.Memory` for guest RAM **plus** separate `SharedArrayBuffer`s
for control/command/event data, then demonstrates cross-thread reads/writes and `Atomics` synchronization between the
main thread and a worker.

### Run

```sh
node poc/browser-memory/server.mjs
```

Then open:

```
http://localhost:8080/
```

If allocation fails, try a smaller guest RAM size (browser/OS dependent).
