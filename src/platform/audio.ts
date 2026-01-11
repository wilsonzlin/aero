// Repo-root Vite harness shim.
//
// The canonical browser host lives under `web/` (ADR 0001) and owns the
// SharedArrayBuffer + AudioWorklet audio output implementation.
//
// Keep the old import path (`src/platform/audio`) working for the harness UI
// while avoiding duplicated audio code that could drift from `web/`.
export * from "../../web/src/platform/audio";

