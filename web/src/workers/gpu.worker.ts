/// <reference lib="webworker" />

// Vite worker entrypoint. The actual implementation lives in `gpu-worker.ts` so it can be
// imported by other modules without relying on the `.worker.ts` filename convention.
import "./gpu-worker";
