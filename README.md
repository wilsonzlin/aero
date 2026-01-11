# Aero

This repository contains Aero’s architecture/design documentation plus browser-side proofs-of-concept and web scaffolding used to validate feasibility constraints.

## Developer workflow (canonical)

Use `cargo xtask` from the repo root for a **cross-platform** (Windows-friendly)
task runner that orchestrates Rust + WASM + web commands.

If you have bash + [`just`](https://github.com/casey/just), the `justfile` still
provides convenient aliases, but `cargo xtask` is the canonical implementation.

For a reproducible “clone → build → test” environment (including Rust stable+nightly, Node, QEMU, etc.), see [`docs/dev-environment.md`](./docs/dev-environment.md).

### Prerequisites

- Rust (via `rustup`)
- Node.js + npm (version is pinned in [`.nvmrc`](./.nvmrc))
- `wasm-pack` (to build the Rust→WASM packages)
  - Install via `cargo install wasm-pack`
- Optional: `just` (task runner; uses bash)
  - Install via `cargo install just` or your OS package manager.
- Optional: `watchexec` (for `just wasm-watch`)
  - Install via `cargo install watchexec-cli`

### Common workflows (cross-platform)

```bash
cargo xtask wasm both release   # build wasm packages (single + threaded)
cargo xtask web dev             # run the Vite dev server (prints the local URL)
cargo xtask web build           # production web build
cargo xtask test-all --skip-e2e # Rust + WASM + TS (skip Playwright)
```

### Common workflows (bash/just convenience)

```bash
just setup   # install wasm target, install JS deps, sanity-check toolchain
just wasm    # build the Rust→WASM package used by the web app (if present)
just dev     # run the Vite dev server (prints the local URL)
just build   # wasm + web production build
just test    # Rust tests + web unit tests
just fmt     # formatting (if configured)
just lint    # linting (if configured)
```

Tip: if you have `watchexec` installed, `just dev` will also rebuild the threaded/shared-memory WASM variant on changes. Otherwise, run `just wasm-watch` in a second terminal.

To sanity-check your local Node toolchain against CI:

```bash
node scripts/check-node-version.mjs
# or: npm run check:node
```

#### Optional configuration

The `justfile` is intentionally configurable so it can survive repo refactors:

- `WEB_DIR` (default: `web`)

## Repo layout

This repo contains a mix of production code and older prototypes. Start here:

- [`docs/repo-layout.md`](./docs/repo-layout.md) (canonical vs legacy/prototypes)
- [`docs/adr/0001-repo-layout.md`](./docs/adr/0001-repo-layout.md) (why the canonical layout looks the way it does)

Quick map:

- `web/` – **production** browser host app (Vite)
- `crates/` – Rust workspace crates (emulator core + supporting libs)
- `backend/`, `services/` – maintained backend services
- `server/` – **legacy** backend (see `server/LEGACY.md`)
- `poc/`, `prototype/` – experiments / RFC companions (not production)
- Repo root `index.html` + `src/main.ts` – **dev/test harness** (used by Playwright; not production)

## Documentation

- Architecture & subsystem docs: [`AGENTS.md`](./AGENTS.md)
- Deployment/hosting (COOP/COEP, SharedArrayBuffer/WASM threads): [`docs/deployment.md`](./docs/deployment.md)
- Releases (web artifacts + gateway images): [`docs/release.md`](./docs/release.md)
- Windows 7 end-user setup:
  - Install Guest Tools + switch to virtio/Aero GPU: [`docs/windows7-guest-tools.md`](./docs/windows7-guest-tools.md)
  - Driver/signature/boot troubleshooting: [`docs/windows7-driver-troubleshooting.md`](./docs/windows7-driver-troubleshooting.md)

## Architecture Decision Records (ADRs)

Infrastructure decisions are captured as ADRs in [`docs/adr/`](./docs/adr/):

- [`docs/adr/0001-repo-layout.md`](./docs/adr/0001-repo-layout.md)
- [`docs/adr/0002-cross-origin-isolation.md`](./docs/adr/0002-cross-origin-isolation.md)
- [`docs/adr/0003-shared-memory-layout.md`](./docs/adr/0003-shared-memory-layout.md)
- [`docs/adr/0004-wasm-build-variants.md`](./docs/adr/0004-wasm-build-variants.md)
- [`docs/adr/0009-rust-toolchain-policy.md`](./docs/adr/0009-rust-toolchain-policy.md)

## Web (Vite)

The `web/` app is configured for **cross-origin isolation** in both dev and preview mode.

Canonical:

```sh
just setup
just dev
```

Manual equivalent:

```sh
npm ci
npm -w web run dev
```

## Configuration

Aero uses a typed configuration object (`AeroConfig`) that can be sourced from multiple places. The final, effective config is derived by applying these layers in order (lowest → highest precedence):

1. **Defaults** (built-in, with capability-aware defaults where applicable)
2. **Static JSON config** (optional, for deployments): `GET /aero.config.json`
3. **Persisted user settings** (`localStorage`): key `aero:config:v1`
4. **URL query parameters** (highest precedence)

### Query parameters

| Param     | Type | Maps to | Example |
|----------:|------|---------|---------|
| `mem`     | number (MiB) | `guestMemoryMiB` | `?mem=2048` |
| `workers` | bool | `enableWorkers` | `?workers=0` |
| `webgpu`  | bool | `enableWebGPU` | `?webgpu=1` |
| `proxy`   | string \| `null` | `proxyUrl` | `?proxy=wss%3A%2F%2Fproxy.example%2Fws` |
| `disk`    | string \| `null` | `activeDiskImage` | `?disk=win7-sp1.img` |
| `log`     | `trace|debug|info|warn|error` | `logLevel` | `?log=debug` |
| `scale`   | number | `uiScale` | `?scale=1.25` |

### Examples

```text
/?mem=2048&log=debug
/?proxy=wss%3A%2F%2Flocalhost%3A1234%2Fws&workers=0
```

### Notes

- URL overrides are intentionally *highest precedence* and are shown as read-only in the Settings panel.
- Some settings may be forced off at runtime if the browser lacks required capabilities (e.g. workers require `SharedArrayBuffer` + cross-origin isolation).

## WASM in workers

The `web/` package uses module workers for CPU/GPU/I/O/JIT stubs and needs to initialize the same WASM module from both the **main thread** and **workers**.

To avoid worker code accidentally referencing `window` (workers don’t have it), the runtime exposes a single initialization entrypoint that works in either context:

```ts
import { initWasmForContext } from "./runtime/wasm_context";

const { api, variant } = await initWasmForContext();
// api.add(a, b), api.greet(name), ...
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

- Rust (managed by `rustup`). The repo pins stable via `rust-toolchain.toml`.
- `wasm-pack` (`cargo install wasm-pack`)
- For the **threaded/shared-memory** variant: the pinned nightly toolchain declared in `scripts/toolchains.json`
  (`rust.nightlyWasm`) + `rust-src` (used to rebuild `std` with atomics enabled). `just setup` installs this automatically.

Manual install (if needed):

```bash
wasm_nightly="$(node -p "require('./scripts/toolchains.json').rust.nightlyWasm")"
rustup toolchain install "$wasm_nightly"
rustup target add wasm32-unknown-unknown --toolchain "$wasm_nightly"
rustup component add rust-src --toolchain "$wasm_nightly"
```

Recommended (repo root):

```bash
cargo xtask wasm both release  # builds both variants (single + threaded)
# Or (bash/just convenience):
just wasm
```

Manual equivalent (repo root, npm workspaces):

```bash
npm -w web run wasm:build        # builds both variants
```

Or individually:

```bash
npm -w web run wasm:build:threaded
npm -w web run wasm:build:single
```

Generated output is written into `web/src/wasm/pkg-{threaded,single}/` and is gitignored.

### Testing the fallback path (no COOP/COEP)

To test the **single** variant, start the dev server with the headers disabled:

```bash
VITE_DISABLE_COOP_COEP=1 npm -w web run dev
```

In this mode the loader will select the non-shared-memory build automatically, and the UI will report which variant
was loaded (and why).

## Optional guest networking support (TCP/UDP via WebSocket relay)

Browsers cannot open arbitrary TCP/UDP sockets directly. For guest networking, Aero can use a small local proxy that exposes WebSocket endpoints and relays to real TCP/UDP sockets from the server side.

This repo includes a standalone proxy service at [`net-proxy/`](./net-proxy/).

### Local dev workflow (run alongside Vite)

Terminal 1 (network proxy):

```bash
npm ci

# Trusted local development mode: allows localhost + private ranges.
AERO_PROXY_OPEN=1 npm -w net-proxy run dev
```

Terminal 2 (frontend):

```bash
npm -w web run dev
```

The proxy exposes:

- `GET /healthz`
- `WS /tcp?v=1&host=<host>&port=<port>` (or `?v=1&target=<host>:<port>`)
- `WS /udp?v=1&host=<host>&port=<port>` (or `?v=1&target=<host>:<port>`)

See `net-proxy/README.md` for allowlisting and client URL examples.

## Optional disk image gateway (S3 + CloudFront Range streaming)

For large disk images (20GB+), Aero’s browser storage stack can stream missing byte ranges on-demand using HTTP `Range`.

This repo includes a reference backend service at [`services/image-gateway/`](./services/image-gateway/) that:

- supports S3 multipart upload with presigned part URLs
- returns CloudFront signed-cookie (preferred) or signed-URL auth material for a stable, cacheable image URL
- includes a dev-only `Range` proxy fallback for local testing without CloudFront

### Local MinIO object store (Range + CORS)

For a self-contained local environment to validate **HTTP Range** + **CORS/preflight** behavior against an S3-compatible endpoint (with an optional nginx “edge” proxy), see:

- [`infra/local-object-store/README.md`](./infra/local-object-store/README.md)

From the repo root:

```bash
just object-store-up        # MinIO origin on http://localhost:9000 + console on :9001
just object-store-up-proxy  # adds an nginx proxy on http://localhost:9002 (edge/CORS emulation)
just object-store-verify --down
```

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

## Storage I/O microbench (PF-008)

The `web/` app exposes an early, emulator-independent browser storage benchmark:

- OPFS (Origin Private File System) via `navigator.storage.getDirectory()` when available
- IndexedDB fallback when OPFS is unavailable

### In-app API

Run from the browser devtools console:

```js
await window.aero.bench.runStorageBench();
await window.aero.perf.export();
```

### Playwright scenario runner

Run the `storage_io` scenario (writes results under `bench/results/`):

```bash
node --experimental-strip-types bench/runner.ts storage_io
```

Skip the storage I/O scenario (useful for noisy CI environments):

```bash
AERO_BENCH_SKIP_STORAGE_IO=1 node --experimental-strip-types bench/runner.ts storage_io
```

Check default thresholds (informational by default; add `--enforce` for CI gating):

```bash
node --experimental-strip-types bench/compare.ts --input bench/results/<run>/perf_export.json
```

## Disk image manager UI (OPFS)

The `web/` app includes a **Disk Images** panel backed by OPFS (Origin Private File System):

- Import with progress, list/delete, export/download
- Select an image as “active” (persisted in `localStorage`)
- Minimal I/O worker stub to open the active disk via `FileSystemSyncAccessHandle` and report its size

### OPFS smoke test (manual)

In a Chromium-based browser with OPFS support:

1. Open the app.
2. In **Disk Images**, click **Import…** and select a `.img` / `.iso` / raw disk file.
3. The disk should appear in the list with its size.
4. Click **Export** to download the stored image and compare size/hash with the original.
5. Select a disk as **Active** and click **Open active disk in I/O worker**.
   - The worker will attempt to create a `FileSystemSyncAccessHandle` and report the disk size.

If the UI shows “OPFS unavailable”, the app falls back to an in-memory store; images will not persist across reloads and the
I/O worker cannot open a sync access handle.

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
