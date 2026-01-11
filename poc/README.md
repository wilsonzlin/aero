# Proofs of concept (`poc/`)

This directory contains **small, self-contained proofs-of-concept** used to validate browser/platform constraints.

These are **not production entrypoints**. The production browser host app lives in `web/`.

## Contents

- `browser-memory/` – SharedArrayBuffer + `WebAssembly.Memory` memory model PoC.
  - Run: `node poc/browser-memory/server.mjs` then open the printed URL.

