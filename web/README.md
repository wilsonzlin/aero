# Web / WASM build tooling

This directory contains the developer-facing WASM build scripts used by the web app.

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

The build produces **two** WASM variants:

- **Single-threaded** (no `SharedArrayBuffer` requirement): `web/src/wasm/pkg-single/`
- **Threaded** (shared memory + atomics): `web/src/wasm/pkg-threaded/`

Dev builds are written to:

- `web/src/wasm/pkg-single-dev/`
- `web/src/wasm/pkg-threaded-dev/` (supported, but typically slower due to `build-std`)

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

