# Aero (design docs + browser PoCs)

This repository currently contains Aero’s architecture/design documentation plus small browser-side proofs-of-concept used to validate feasibility constraints.

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

```
http://localhost:8080/
```

If allocation fails, try a smaller guest RAM size (browser/OS dependent).

