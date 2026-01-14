import { expect, test } from "@playwright/test";

test("IO worker increments input drop counter when snapshot-paused input queue is full", async ({ page }) => {
  test.setTimeout(30_000);
  await page.goto("/", { waitUntil: "load" });

  const support = await page.evaluate(() => {
    const crossOriginIsolated = globalThis.crossOriginIsolated === true;
    const sharedArrayBuffer = typeof SharedArrayBuffer !== "undefined";
    const atomics = typeof Atomics !== "undefined";
    const worker = typeof Worker !== "undefined";
    const wasm = typeof WebAssembly !== "undefined" && typeof WebAssembly.Memory === "function";
    let wasmThreads = false;
    if (wasm) {
      try {
        // eslint-disable-next-line no-new
        new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
        wasmThreads = true;
      } catch {
        wasmThreads = false;
      }
    }
    return { crossOriginIsolated, sharedArrayBuffer, atomics, worker, wasm, wasmThreads };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics, "Atomics is unavailable in this browser configuration.");
  test.skip(!support.worker, "Web Workers are unavailable in this environment.");
  test.skip(!support.wasm, "WebAssembly.Memory is unavailable in this environment.");
  test.skip(!support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");

  const result = await page.evaluate(async () => {
    const { allocateSharedMemorySegments, createSharedMemoryViews, StatusIndex } = await import("/web/src/runtime/shared_layout.ts");

    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const views = createSharedMemoryViews(segments);
    const status = views.status;

    const ioWorker = new Worker(new URL("/web/src/workers/io.worker.ts", location.href), { type: "module" });

    const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
    const waitFor = async (predicate: () => boolean, timeoutMs: number, name: string) => {
      const deadline = (typeof performance?.now === "function" ? performance.now() : Date.now()) + timeoutMs;
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) < deadline) {
        if (predicate()) return;
        await sleep(5);
      }
      throw new Error(`Timed out waiting for ${name}`);
    };

    const waitForWorkerMessage = (kind: string, requestId: number, timeoutMs = 5_000): Promise<void> => {
      return new Promise((resolve, reject) => {
        const timer = globalThis.setTimeout(() => {
          cleanup();
          reject(new Error(`Timed out waiting for io.worker ${kind}(${requestId}) after ${timeoutMs}ms.`));
        }, timeoutMs);
        (timer as unknown as { unref?: () => void }).unref?.();

        const onMessage = (ev: MessageEvent<unknown>) => {
          const data = ev.data as { kind?: unknown; requestId?: unknown; ok?: unknown; error?: unknown };
          if (data?.kind !== kind || (data.requestId as number) !== requestId) return;
          cleanup();
          if (data.ok !== true) {
            reject(new Error(typeof (data.error as any)?.message === "string" ? (data.error as any).message : `${kind} failed`));
            return;
          }
          resolve();
        };
        const onError = (ev: ErrorEvent) => {
          cleanup();
          reject(new Error(ev.message || "io.worker error"));
        };
        const cleanup = () => {
          globalThis.clearTimeout(timer);
          ioWorker.removeEventListener("message", onMessage);
          ioWorker.removeEventListener("error", onError);
        };
        ioWorker.addEventListener("message", onMessage);
        ioWorker.addEventListener("error", onError);
      });
    };

    // io.worker waits for an initial boot disk selection message before reporting READY.
    ioWorker.postMessage({ type: "setBootDisks", mounts: {}, hdd: null, cd: null });
    ioWorker.postMessage({
      kind: "init",
      role: "io",
      controlSab: segments.control,
      guestMemory: segments.guestMemory,
      ioIpcSab: segments.ioIpc,
      sharedFramebuffer: segments.sharedFramebuffer,
      sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      scanoutState: segments.scanoutState,
      scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
    });

    await waitFor(() => Atomics.load(status, StatusIndex.IoReady) === 1, 20_000, "StatusIndex.IoReady");

    // Pause so input batches are queued/dropped instead of being processed.
    const pauseId = 1;
    ioWorker.postMessage({ kind: "vm.snapshot.pause", requestId: pauseId });
    await waitForWorkerMessage("vm.snapshot.paused", pauseId);

    const readTelemetry = (globalThis as any).aero?.debug?.readIoInputTelemetry as ((status: Int32Array) => any) | undefined;
    if (typeof readTelemetry !== "function") {
      throw new Error("window.aero.debug.readIoInputTelemetry is unavailable (installAeroGlobal not installed?)");
    }

    const before = readTelemetry(status);

    // Overflow the bounded snapshot-paused queue (4 MiB). Use 1 MiB batches so the overflow is deterministic:
    // - first 4 batches fit
    // - remaining batches are dropped
    const batchSize = 1024 * 1024;
    const batches = 6;
    for (let i = 0; i < batches; i += 1) {
      const buffer = new ArrayBuffer(batchSize);
      ioWorker.postMessage({ type: "in:input-batch", buffer }, [buffer]);
    }

    // Wait for at least one drop to be observed.
    await waitFor(() => (Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0) > (before.batchesDropped >>> 0), 2_000, "IoInputBatchDropCounter to increment");

    const after = readTelemetry(status);

    ioWorker.terminate();

    return { before, after, batches };
  });

  expect(result.after.batchesDropped).toBeGreaterThan(result.before.batchesDropped);
  expect(result.after.batchesReceived).toBeGreaterThanOrEqual(result.before.batchesReceived + result.batches);
});
