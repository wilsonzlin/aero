# Aero

This repository contains Aero’s architecture/design documentation plus browser-side proofs-of-concept and web scaffolding used to validate feasibility constraints.

## Developer workflow (canonical)

Use `cargo xtask` from the repo root for a **cross-platform** (Windows-friendly)
task runner that orchestrates Rust + WASM + web commands.

If you have bash + [`just`](https://github.com/casey/just), the `justfile` still
provides convenient aliases, but `cargo xtask` is the canonical implementation.

For a reproducible “clone → build → test” environment (including pinned Rust toolchains, Node, QEMU, etc.), see [`docs/dev-environment.md`](./docs/dev-environment.md).

### Prerequisites

- Rust (via `rustup`)
- Node.js + npm (version is pinned in [`.nvmrc`](./.nvmrc))
- `wasm-pack` (to build the Rust→WASM packages)
  - Install via `cargo install --locked wasm-pack`
- Optional: `just` (task runner; uses bash)
  - Install via `cargo install --locked just` or your OS package manager.
- Optional: `watchexec` (for `just wasm-watch`)
  - Install via `cargo install --locked watchexec-cli`

### Reproducible Rust builds (`Cargo.lock`)

Rust dependency versions are pinned via checked-in `Cargo.lock` files, and CI runs Rust commands with `--locked`.
See [ADR 0012](./docs/adr/0012-cargo-lock-policy.md) for the full policy.

If you update Rust dependencies (or see a `--locked` failure), regenerate the lockfile and include it in your PR:

```bash
# Root workspace
cargo generate-lockfile

# Standalone tools that have their own Cargo.lock (use the tool's Cargo.toml)
cargo generate-lockfile --manifest-path path/to/tool/Cargo.toml
```

### Common workflows (cross-platform)

```bash
cargo xtask wasm both release   # build wasm packages (single + threaded)
cargo xtask web dev             # run the Vite dev server (prints the local URL)
cargo xtask web build           # production web build
cargo xtask test-all --skip-e2e # Rust + WASM + TS (skip Playwright)
cargo xtask wasm-check          # compile-check wasm32 compatibility (no JS runtime required)
```

### Common workflows (bash/just convenience)

```bash
just setup   # install wasm target, install JS deps, sanity-check toolchain
just wasm    # build the Rust→WASM package used by the web app (if present)
just wasm-check # compile-check wasm32 compatibility (no JS runtime required)
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

If you need to run tooling with a different Node version (unsupported), you can bypass the hard error:

```bash
AERO_ALLOW_UNSUPPORTED_NODE=1 node scripts/check-node-version.mjs
```

#### Optional configuration

The `justfile` is intentionally configurable so it can survive repo refactors:

- `AERO_NODE_DIR` / `WEB_DIR` (default: auto-detected via `scripts/ci/detect-node-dir.mjs`)

## Repo layout

This repo contains a mix of production code and older prototypes. Start here:

- [`docs/repo-layout.md`](./docs/repo-layout.md) (canonical vs legacy/prototypes)
- [`docs/adr/0001-repo-layout.md`](./docs/adr/0001-repo-layout.md) (why the canonical layout looks the way it does)

Quick map:

- Repo root `index.html` + `src/` – **canonical** browser host app (Vite; used by CI/Playwright)
- `vite.harness.config.ts` – Vite config for the repo-root app (sets COOP/COEP + CSP for tests)
- `web/` – shared runtime modules + WASM build tooling (and a legacy/experimental Vite entrypoint at `web/index.html`)
- `crates/` – Rust workspace crates (canonical `aero-machine` stack + supporting libs; `crates/emulator` is legacy/compat)
- `backend/`, `services/` – maintained backend services
- `proxy/` – maintained networking relays (e.g. `proxy/webrtc-udp-relay`)
- `net-proxy/` – local-dev WebSocket TCP/UDP relay + DNS-over-HTTPS endpoints (run alongside `vite dev`)
- `server/` – **legacy** backend (see `server/LEGACY.md`)
- `poc/`, `prototype/` – experiments / RFC companions (not production)

## Documentation

- Architecture & subsystem docs: [`AGENTS.md`](./AGENTS.md)
- Graphics status (implemented vs missing for Win7 UX): [`docs/graphics/status.md`](./docs/graphics/status.md)
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
- [`docs/adr/0005-aerogpu-pci-ids-and-abi.md`](./docs/adr/0005-aerogpu-pci-ids-and-abi.md)
- [`docs/adr/0006-node-monorepo-tooling.md`](./docs/adr/0006-node-monorepo-tooling.md)
- [`docs/adr/0009-rust-toolchain-policy.md`](./docs/adr/0009-rust-toolchain-policy.md)
- [`docs/adr/0012-cargo-lock-policy.md`](./docs/adr/0012-cargo-lock-policy.md)
- [`docs/adr/0013-networking-l2-tunnel.md`](./docs/adr/0013-networking-l2-tunnel.md)
- [`docs/adr/0014-canonical-machine-stack.md`](./docs/adr/0014-canonical-machine-stack.md)
- [`docs/adr/0015-canonical-usb-stack.md`](./docs/adr/0015-canonical-usb-stack.md)

## Web (Vite)

The repo-root Vite app is configured for **cross-origin isolation** in both dev and preview mode.

Canonical:

```sh
just setup
just dev
```

Manual equivalent:

```sh
npm ci
npm run dev
```

To run the legacy/experimental `web/` Vite app explicitly:

```sh
npm run dev:web
# or:
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
| `vram`    | number (MiB) | `vramMiB` | `?vram=64` |
| `workers` | bool | `enableWorkers` | `?workers=0` |
| `webgpu`  | bool | `enableWebGPU` | `?webgpu=1` |
| `machineAerogpu` | bool | `machineEnableAerogpu` (machine runtime only) | `?machineAerogpu=0` |
| `proxy`   | string \| `null` | `proxyUrl` | `?proxy=https%3A%2F%2Fgateway.example.com` |
| `disk`    | string \| `null` | `activeDiskImage` (deprecated; legacy mount hint) | `?disk=win7-sp1.img` |
| `log`     | `trace|debug|info|warn|error` | `logLevel` | `?log=debug` |
| `scale`   | number | `uiScale` | `?scale=1.25` |
| `vm`      | `legacy|machine` | `vmRuntime` | `?vm=machine` |
| `kbd`     | `auto|ps2|usb|virtio` | `forceKeyboardBackend` | `?kbd=ps2` |
| `mouse`   | `auto|ps2|usb|virtio` | `forceMouseBackend` | `?mouse=virtio` |

### Examples

```text
/?mem=2048&log=debug
/?proxy=http%3A%2F%2F127.0.0.1%3A8081&workers=0
```

### Notes

- URL overrides are intentionally *highest precedence* and are shown as read-only in the Settings panel.
- Some settings may be forced off at runtime if the browser lacks required capabilities (e.g. workers require `SharedArrayBuffer` + cross-origin isolation).
- `proxy` (`proxyUrl`) may be either an absolute `ws(s)://` / `http(s)://` URL or a same-origin path like `/l2` (legacy alias: `/eth`).
- `vmRuntime` can also be set via `?machine=1` (shorthand for `?vm=machine`).
- `machineAerogpu` only applies when `vmRuntime=machine` and selects the machine runtime’s graphics adapter:
  - `1` (default): AeroGPU (canonical)
  - `0`: legacy VGA/VBE (debug/compat)
- `disk` / `activeDiskImage` is deprecated. Disk selection now flows through the DiskManager mounts + `setBootDisks`, and VM/demo policies are keyed off `vmRuntime` + boot-disk presence. For compatibility, the legacy `web/` UI may treat `activeDiskImage` as a best-effort initial mount hint (it does **not** act as a VM-mode toggle).

### VM runtime modes (`vmRuntime`)

Aero currently exposes two browser VM runtimes:

- **Legacy mode** (`vmRuntime=legacy`, default): the historical split runtime where the CPU loop runs in WASM and port I/O / MMIO are forwarded to TypeScript device shims.
- **Machine mode** (`vmRuntime=machine`): runs the canonical full-system VM (`api.Machine`, backed by `aero_machine::Machine`) and uses its canonical device topology.

Selection:

- Config: `vmRuntime: "legacy" | "machine"`
- URL: `?vm=legacy|machine` (alias: `?vmRuntime=...`)

Machine mode is required for Windows 7 storage bring-up because Windows Setup expects a compatibility-first controller topology:

- **AHCI (ICH9)** for the primary HDD
- **IDE (PIIX3) + ATAPI** for the install media (CD-ROM)

See [`docs/05-storage-topology-win7.md`](./docs/05-storage-topology-win7.md) for the normative PCI BDFs, attachment points, and snapshot `disk_id` mapping.

#### OPFS disk locking caveat (SyncAccessHandle)

In browsers, OPFS `FileSystemSyncAccessHandle` is **exclusive**: only **one** SyncAccessHandle may exist for a given file at a time.
Accidentally opening the same disk image twice (even in the same origin) typically fails with an `InvalidStateError` due to the file lock.

The machine runtime avoids this in two ways:

- **Single owner:** only the worker that runs the canonical `api.Machine` opens OPFS-backed disks; other workers avoid opening competing handles to the same file.
- **Single attachment inside the VM:** `api.Machine` uses the canonical `SharedDisk` wiring so BIOS INT13 and storage controllers observe the same bytes without independently opening the disk file.

#### Disk overlay-ref strings (snapshots)

Machine snapshots store disk references in the `DISKS` section as `DiskOverlayRefs` entries: `{ disk_id, base_image, overlay_image }`.
In the browser machine runtime, these strings are interpreted as **OPFS-relative paths** (e.g. `aero/disks/win7.img`), which the host re-opens and re-attaches on restore.
The stable `disk_id` values and their attachment points are defined in [`docs/05-storage-topology-win7.md`](./docs/05-storage-topology-win7.md).

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

## WebUSB diagnostics (bulk device feasibility / bug reports)

To inspect what WebUSB can “see” on a developer machine (and why a device might not appear in the chooser), open:

- `webusb_diagnostics.html`

The diagnostics page:

- Calls `navigator.usb.requestDevice(...)` via a broad filter.
- Prints the selected device’s configurations / interfaces / endpoints.
- Marks interfaces that are likely **WebUSB-protected** (e.g. HID / Mass Storage) vs **claimable**.
- Offers a best-effort “open + claim” probe (prefers bidirectional bulk endpoints when available).
- Can list `navigator.usb.getDevices()` and copy a JSON summary to include in bug reports.

## WASM builds (threaded vs single fallback)

Browsers only enable `SharedArrayBuffer` (and therefore WASM shared memory / threads) in **cross-origin isolated**
contexts (`COOP` + `COEP` headers). To keep the web app usable even without those headers, we build two WASM
variants (**threaded** and **single-threaded**). Each variant produces one or more wasm-pack packages under
`web/src/wasm/`:

- Core VM/runtime (`crates/aero-wasm`):
  - `web/src/wasm/pkg-threaded/` – shared-memory build (SAB + Atomics), intended for `crossOriginIsolated` contexts.
  - `web/src/wasm/pkg-single/` – non-shared-memory build that can run without COOP/COEP (degraded functionality is OK).
- GPU runtime (`crates/aero-gpu-wasm`):
  - `web/src/wasm/pkg-threaded-gpu/`
  - `web/src/wasm/pkg-single-gpu/`
- Tier-1 compiler / JIT support (`crates/aero-jit-wasm`, when present):
  - `web/src/wasm/pkg-jit-threaded/`
  - `web/src/wasm/pkg-jit-single/`

At runtime, `web/src/runtime/wasm_loader.ts` selects the best variant and returns a stable API surface.

### Build WASM

Prereqs:

- Rust (managed by `rustup`). The repo pins stable via `rust-toolchain.toml`.
- `wasm-pack` (`cargo install --locked wasm-pack`)
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

Generated output is written into `web/src/wasm/pkg-*` and is gitignored.

### Testing the fallback path (no COOP/COEP)

To test the **single** variant, start the dev server with the headers disabled:

```bash
VITE_DISABLE_COOP_COEP=1 npm run dev
```

In this mode the loader will select the non-shared-memory build automatically, and the UI will report which variant
was loaded (and why).

## Optional guest networking support (TCP over WebSocket, DNS-over-HTTPS, UDP over WebRTC)

Browsers cannot open arbitrary TCP/UDP sockets directly. Aero’s guest networking support therefore relies on server-side relays.

### Production / deployment (recommended)

- **TCP + DNS:** [`backend/aero-gateway`](./backend/aero-gateway/) provides a policy-driven gateway with:
  - `WS /tcp` (one TCP connection per WebSocket)
  - `WS /tcp-mux` (multiplexed TCP over one WebSocket; subprotocol `aero-tcp-mux-v1`) for scaling high connection counts
  - `POST /session` + `GET/POST /dns-query` (DNS-over-HTTPS)

  Wire contracts: [`docs/backend/01-aero-gateway-api.md`](./docs/backend/01-aero-gateway-api.md)

- **L2 tunnel (Option C):** [`crates/aero-l2-proxy`](./crates/aero-l2-proxy/) provides an Ethernet (L2) tunnel over WebSocket:
  - `WS /l2` (legacy alias: `/eth`; subprotocol `aero-l2-tunnel-v1`)
  - Browser clients should call `POST /session` first and use the gateway response for discovery (`endpoints.l2`) and tuning (`limits.l2`) instead of hardcoding paths.
  - Deployment note: the canonical `deploy/docker-compose.yml` stack enforces an Origin allowlist for `/l2` (and legacy `/eth`) by default.
    Authentication is opt-in via `AERO_L2_AUTH_MODE` (recommended: `session` for same-origin browser clients; legacy alias:
    `cookie`). When using session-cookie auth (`AERO_L2_AUTH_MODE=session|cookie_or_jwt|session_or_token|session_and_token`;
    legacy aliases: `cookie_or_api_key`, `cookie_and_api_key`), the gateway and L2 proxy must share `SESSION_SECRET` (set it
    explicitly for production, or let the deploy stack generate and persist a random secret in a Docker volume so sessions
    survive restarts until `docker compose down -v`).

  Wire contract: [`docs/l2-tunnel-protocol.md`](./docs/l2-tunnel-protocol.md)

- **UDP:** [`proxy/webrtc-udp-relay`](./proxy/webrtc-udp-relay/) is the primary UDP path. It relays UDP over a WebRTC DataChannel (`label="udp"`, `ordered=false`, `maxRetransmits=0`) using versioned v1/v2 datagram framing and signaling defined in:
  - [`proxy/webrtc-udp-relay/PROTOCOL.md`](./proxy/webrtc-udp-relay/PROTOCOL.md)

  The same service also exposes `GET /udp` as a WebSocket UDP relay fallback using the same v1/v2 framing. WebSockets are reliable and ordered, so this cannot preserve true UDP loss/reordering semantics; treat it as a fallback/debug path when WebRTC isn’t available.

  Inbound filtering note: by default the relay only forwards inbound UDP from remote address+port tuples that the guest previously sent to (`UDP_INBOUND_FILTER_MODE=address_and_port`). You can switch to full-cone behavior with `UDP_INBOUND_FILTER_MODE=any` (**less safe**; see the relay README).

  DoS hardening note: the relay configures pion/SCTP message-size caps to prevent malicious peers from sending extremely large WebRTC DataChannel messages that could otherwise be buffered/allocated before `DataChannel.OnMessage` runs. These are configurable on the relay via:
  - `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` (SDP `a=max-message-size` hint; 0 = auto)
  - `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES` (hard receive-side cap; 0 = auto; must be ≥ `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` and ≥ `1500`)
  - `WEBRTC_SESSION_CONNECT_TIMEOUT` (close server-side PeerConnections that never connect; default `30s`)

  When deploying the relay separately, `backend/aero-gateway` can optionally mint short-lived relay credentials via the `udpRelay` field in `POST /session` (or `POST /udp-relay/token`).

### Local dev workflow (run alongside Vite)

This repo includes a standalone proxy service at [`net-proxy/`](./net-proxy/) that’s convenient to run next to `vite dev`.

Terminal 1 (network proxy):

```bash
npm ci

# Trusted local development mode: allows localhost + private ranges.
AERO_PROXY_OPEN=1 npm -w net-proxy run dev
```

Terminal 2 (frontend):

```bash
npm run dev
```

The proxy exposes:

- `GET /healthz`
- `GET|POST /dns-query` + `GET /dns-json` — DNS-over-HTTPS (RFC 8484 + JSON) for local dev
- `WS /tcp?v=1&host=<host>&port=<port>` (or `?v=1&target=<host>:<port>`) — compatible with the gateway `/tcp` URL format.
- `WS /tcp-mux` (subprotocol `aero-tcp-mux-v1`) — multiplexed TCP over a single WebSocket (framing spec in [`docs/backend/01-aero-gateway-api.md`](./docs/backend/01-aero-gateway-api.md)).
- `WS /udp` — **multiplexed** UDP relay datagrams (v1/v2 framing per [`proxy/webrtc-udp-relay/PROTOCOL.md`](./proxy/webrtc-udp-relay/PROTOCOL.md)). This is the mode used by the browser `WebSocketUdpProxyClient`.
- `WS /udp?v=1&host=<host>&port=<port>` (or `?v=1&target=<host>:<port>`) — **legacy per-target** UDP relay (one destination per WebSocket; raw UDP payload bytes).

Note: the DoH endpoints are `fetch()`-based HTTP requests, so browser clients generally need them to be **same-origin**
with the frontend dev server (or be served with permissive CORS). The easiest approach during local dev is to proxy
`/dns-query` and `/dns-json` through Vite. Alternatively, `net-proxy` can serve those endpoints with an explicit CORS
allowlist via `AERO_PROXY_DOH_CORS_ALLOW_ORIGINS`. See [`net-proxy/README.md`](./net-proxy/README.md).

### Networking architecture choices

- [`docs/networking-architecture-rfc.md`](./docs/networking-architecture-rfc.md)
- [`docs/07-networking.md`](./docs/07-networking.md)

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

## Storage I/O microbench

The `web/` app exposes an early, emulator-independent browser storage benchmark:

- OPFS (Origin Private File System) via `navigator.storage.getDirectory()` when available
- IndexedDB fallback when OPFS is unavailable

Note: IndexedDB storage is async-only and does not currently back the synchronous Rust
disk/controller path (`aero_storage::{StorageBackend, VirtualDisk}`); see
[`docs/19-indexeddb-storage-story.md`](./docs/19-indexeddb-storage-story.md) and
[`docs/20-storage-trait-consolidation.md`](./docs/20-storage-trait-consolidation.md).

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

## Guest CPU instruction throughput microbench (PF-008)

PF-008 is a deterministic guest instruction throughput suite that runs small x86 instruction streams directly inside the CPU core (no OS / disk image required) and reports IPS/MIPS.

Run from the browser devtools console:

```js
await window.aero.bench.runGuestCpuBench({ variant: 'alu32', mode: 'interpreter', seconds: 0.25 });
await window.aero.perf.export();
```

Notes:

- Only `mode: "interpreter"` is expected to work initially; JIT modes may be unimplemented.
- The benchmark validates a payload checksum and throws on mismatch (correctness guardrail).

## Disk image manager UI (OPFS + IndexedDB fallback)

The `web/` app includes a **Disk Images** panel backed by OPFS (Origin Private File System)
when available, with an IndexedDB fallback when OPFS sync access handles are unavailable.

- Import with progress, list/delete, export/download
- Select an image as “active” (persisted in `localStorage`)
- Minimal I/O worker stub (OPFS only) to open the active disk via `FileSystemSyncAccessHandle` and report its size

### OPFS smoke test (manual)

In a Chromium-based browser with OPFS support:

1. Open the app.
2. In **Disk Images**, click **Import…** and select a `.img` / `.iso` / raw disk file.
3. The disk should appear in the list with its size.
4. Click **Export** to download the stored image and compare size/hash with the original.
5. Select a disk as **Active** and click **Open active disk in I/O worker**.
   - The worker will attempt to create a `FileSystemSyncAccessHandle` and report the disk size.

If OPFS sync access handles are unavailable, the disk manager falls back to IndexedDB for persistence.
In that mode, the I/O worker cannot open a `FileSystemSyncAccessHandle` and the synchronous Rust
disk/controller path is unavailable (see `docs/19-indexeddb-storage-story.md` and
`docs/20-storage-trait-consolidation.md`).

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
