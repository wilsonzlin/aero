import { parentPort } from "node:worker_threads";

type WorkerCreatedMessage = {
  type: "test.worker.created";
  url: string;
};

/**
 * Worker-constructor spy shim for `worker_threads` tests.
 *
 * The IO worker's `RuntimeDiskClient` spawns `runtime_disk_worker.ts` via the Web Worker
 * `Worker` constructor. In `vmRuntime=machine` host-only mode, the IO worker must never
 * attempt to create that disk worker (OPFS sync access handles are exclusive and owned by
 * the machine CPU worker).
 *
 * This shim installs a minimal `globalThis.Worker` that reports constructor calls to the
 * parent test process.
 */
class SpyWorker {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  onmessageerror: ((event: unknown) => void) | null = null;

  constructor(url: string | URL) {
    const href = typeof url === "string" ? url : url instanceof URL ? url.href : String(url);
    parentPort?.postMessage({ type: "test.worker.created", url: href } satisfies WorkerCreatedMessage);
  }

  postMessage(_msg: unknown, _transfer?: unknown[]): void {
    // no-op
  }

  terminate(): void {
    // no-op
  }
}

(globalThis as unknown as { Worker?: unknown }).Worker = SpyWorker as unknown as typeof Worker;

