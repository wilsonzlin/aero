# Web / shared runtime + WASM build tooling

> Note: The canonical browser host app lives at the repo root (`npm run dev`, `just dev`).
> The Vite app entrypoint in this directory (`web/index.html`, `npm -w web run dev`) is
> legacy/experimental and is not used by CI/Playwright.

This directory contains shared runtime modules (`web/src/...`) and the developer-facing WASM build
scripts used by the browser host.

## Commands

From the repo root:

```bash
npm run wasm:build:dev
npm run wasm:build:release
npm run wasm:size
```

Or from `web/` directly:

```bash
npm run wasm:build:dev
npm run wasm:build:release
npm run wasm:size
```

## Outputs

The build produces **two** WASM variants (**single-threaded** and **threaded/shared-memory**).

Each variant emits multiple wasm-pack packages under `web/src/wasm/`:

- Core VM/runtime (`aero-wasm`):
  - **Single-threaded**: `web/src/wasm/pkg-single/`
  - **Threaded** (shared memory + atomics): `web/src/wasm/pkg-threaded/`
- GPU runtime (`aero-gpu-wasm`):
  - **Single-threaded**: `web/src/wasm/pkg-single-gpu/`
  - **Threaded**: `web/src/wasm/pkg-threaded-gpu/`
- Tier-1 compiler / JIT support (`aero-jit-wasm`):
  - **Single-threaded**: `web/src/wasm/pkg-jit-single/`
  - **Threaded**: `web/src/wasm/pkg-jit-threaded/`
  - Note: the runtime loader (`web/src/runtime/jit_wasm_loader.ts`) intentionally prefers the
    **single-threaded** package even in `crossOriginIsolated` / WASM-threads-capable environments,
    because a shared-memory build combined with a large `--max-memory` can cause eager allocation
    of a multi‑GiB `SharedArrayBuffer` during instantiation.

Dev builds are written to:

- `web/src/wasm/*-dev/` (e.g. `pkg-single-dev/`, `pkg-threaded-gpu-dev/`)

## Toolchains

- **Stable** (default): pinned in [`rust-toolchain.toml`](../rust-toolchain.toml).
- **Threaded/shared-memory WASM**: requires nightly `-Z build-std` and therefore uses the **pinned nightly**
  toolchain declared in [`scripts/toolchains.json`](../scripts/toolchains.json) (`rust.nightlyWasm`) plus
  `rust-src`. `just setup` installs the correct toolchains automatically (see
  [ADR 0009](../docs/adr/0009-rust-toolchain-policy.md)).

## Build modes

Build orchestration lives in `web/scripts/build_wasm.mjs`.

- **Dev** (`npm run wasm:build:dev`): fast incremental build (`wasm-pack --dev`) with debug info.
- **Release** (`npm run wasm:build:release`): tuned for runtime performance. In addition to
  `wasm-pack --release`, the build injects explicit Rust codegen flags:
  - `-C opt-level=3`
  - `-C lto=thin` (chosen as a strong perf/compile-time tradeoff for large WASM builds)
  - `-C codegen-units=1`
  - `-C embed-bitcode=yes` (required for LTO; Cargo defaults to `embed-bitcode=no`)

WASM target features are injected via `RUSTFLAGS` (scoped to the build command only):

- Always: `+simd128,+bulk-memory`
- Threaded: `+atomics,+mutable-globals` plus shared-memory link args.

## Optional `wasm-opt`

If `wasm-opt` (Binaryen) is available, release builds are post-processed with `-O4` plus the
feature enables required by the chosen memory model (threads vs single).

If it is not installed, the build still succeeds and prints a warning.

## Artifact size reporting

`npm run wasm:size` prints the raw + gzip sizes for:

- the `.wasm` binary
- the JS glue

for each available variant.
