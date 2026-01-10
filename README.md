# Aero

This repository contains Aero’s architecture/design documentation plus browser-side proofs-of-concept and web scaffolding used to validate feasibility constraints.

## Developer workflow (canonical)

Use [`just`](https://github.com/casey/just) from the repo root so contributors don’t need to remember multi-step Rust + WASM + web commands.

### Prerequisites

- Rust (via `rustup`)
- Node.js + npm
- `just` (task runner)
  - Install via `cargo install just` or your OS package manager.

### Common workflows

```bash
just setup   # install wasm target, install JS deps, sanity-check toolchain
just wasm    # build the Rust→WASM package used by the web app (if present)
just dev     # run the Vite dev server (prints the local URL)
just build   # wasm + web production build
just test    # Rust tests + web unit tests
just fmt     # formatting (if configured)
just lint    # linting (if configured)
```

#### Optional configuration

The `justfile` is intentionally configurable so it can survive repo refactors:

- `WEB_DIR` (default: `web`)

## Documentation

- Architecture & subsystem docs: [`AGENTS.md`](./AGENTS.md)
- Deployment/hosting (COOP/COEP, SharedArrayBuffer/WASM threads): [`docs/deployment.md`](./docs/deployment.md)

## Web (Vite)

The `web/` app is configured for **cross-origin isolation** in both dev and preview mode.

Canonical:

```sh
just setup
just dev
```

Manual equivalent:

```sh
cd web
npm ci
npm run dev
```

## WASM in workers

The `web/` package uses module workers for CPU/GPU/I/O/JIT stubs and needs to initialize the same WASM module from both the **main thread** and **workers**.

To avoid worker code accidentally referencing `window` (workers don’t have it), the runtime exposes a single initialization entrypoint that works in either context:

```ts
import { initWasmForContext } from "./runtime/wasm_context";

const { api, variant } = await initWasmForContext();
// api.version(), api.sum(a, b), ...
```

This pattern:

- Uses `globalThis` (no direct `window` access) so it can run in workers.
- Selects a WASM **variant** (`single` vs `threaded`) based on runtime capabilities.
- Loads the `.wasm` via `new URL(..., import.meta.url)` so Vite bundles it correctly for both the main app and worker bundles.

The CPU worker posts a `WASM_READY` message back to the main thread with the selected variant and a value computed by calling exported WASM functions.

## Graphics backend fallback (WebGPU → WebGL2)

Aero prefers **WebGPU** when available, but can fall back to **WebGL2** (reduced capability) in environments where WebGPU is unavailable or disabled.

The fallback backend is implemented under `web/src/graphics/` and includes a standalone demo page at `web/webgl2_fallback_demo.html`. CI covers this path via a Playwright smoke test that forces `navigator.gpu` to be unavailable and verifies WebGL2 rendering still works.

## WASM builds (threaded vs single fallback)

Browsers only enable `SharedArrayBuffer` (and therefore WASM shared memory / threads) in **cross-origin isolated**
contexts (`COOP` + `COEP` headers). To keep the web app usable even without those headers, we build two wasm-pack
packages from the same Rust crate (`crates/aero-wasm`):

- `web/src/wasm/pkg-threaded/` – shared-memory build (SAB + Atomics), intended for `crossOriginIsolated` contexts.
- `web/src/wasm/pkg-single/` – non-shared-memory build that can run without COOP/COEP (degraded functionality is OK).

At runtime, `web/src/runtime/wasm_loader.ts` selects the best variant and returns a stable API surface.

### Build WASM

Prereqs:

- Rust toolchain with `wasm32-unknown-unknown`
- `wasm-pack` (`cargo install wasm-pack`)
- For the **threaded/shared-memory** variant: nightly toolchain + `rust-src` (used to rebuild `std` with atomics enabled)

```bash
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
```

Recommended (repo root):

```bash
just wasm                # builds both variants (single + threaded)
```

Manual equivalent (from `web/`):

```bash
npm run wasm:build        # builds both variants
```

Or individually:

```bash
npm run wasm:build:threaded
npm run wasm:build:single
```

Generated output is written into `web/src/wasm/pkg-{threaded,single}/` and is gitignored.

### Testing the fallback path (no COOP/COEP)

To test the **single** variant, start the dev server with the headers disabled:

```bash
VITE_DISABLE_COOP_COEP=1 npm run dev
```

In this mode the loader will select the non-shared-memory build automatically, and the UI will report which variant
was loaded (and why).

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

This PoC allocates a configurable-size shared `WebAssembly.Memory` for guest RAM **plus** separate `SharedArrayBuffer`s for control/command/event data, then demonstrates cross-thread reads/writes and `Atomics` synchronization between the main thread and a worker.

### Run

```sh
node poc/browser-memory/server.mjs
```

Then open:

```txt
http://localhost:8080/
```

If allocation fails, try a smaller guest RAM size (browser/OS dependent).

## Troubleshooting

### `crossOriginIsolated` is `false` / `SharedArrayBuffer` is not defined

Aero relies on WASM threads + `SharedArrayBuffer`, which requires the page to be **cross-origin isolated**. In practice that means:

- Serve over a secure context (`https://…` or `http://localhost`)
- Send these response headers:
  - `Cross-Origin-Opener-Policy: same-origin`
  - `Cross-Origin-Embedder-Policy: require-corp`

If you see errors like:

- `SharedArrayBuffer is not defined`
- `crossOriginIsolated is false`
- `WebAssembly.Memory(): shared requires SharedArrayBuffer`

…then the page isn’t cross-origin isolated.

**Things to check:**

1. **Verify headers** in DevTools → Network → your document response headers.
2. **Avoid third-party subresources** (CDN scripts, images, fonts) unless they’re served with
   CORS/CORP that satisfies COEP. A single non-compliant subresource can break isolation.
3. **Vite dev server headers:** if the dev server doesn’t set COOP/COEP by default, add headers in
   `vite.config.*` via `server.headers` and (for preview) `preview.headers`.
