import { expect, test } from "@playwright/test";

test("IO worker survives malformed in:input-batch messages", async ({ page }) => {
  // Worker startup + WASM init can be slow under high parallel load (e.g. CI, Grind swarm). Keep
  // this test's budget generous to avoid false failures while still bounding hangs.
  test.setTimeout(60_000);
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
    const { allocateHarnessSharedMemorySegments } = await import("/web/src/runtime/harness_shared_memory.ts");
    const { createIoIpcSab, createSharedMemoryViews, StatusIndex } = await import("/web/src/runtime/shared_layout.ts");
    const { InputEventQueue } = await import("/web/src/input/event_queue.ts");
    const { MessageType } = await import("/web/src/runtime/protocol.ts");
    const { emptySetBootDisksMessage } = await import("/web/src/runtime/boot_disks_protocol.ts");

    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpc: createIoIpcSab({ includeNet: false, includeHidIn: false }),
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);
    const status = views.status;

    // WebKit can fail to load large module workers directly via `new Worker(httpUrl, { type: "module" })`
    // (it emits an `error` event without useful details). Wrap the module entrypoint in a tiny
    // blob-based module worker and import the real worker from there for cross-browser stability.
    const ioWorkerEntrypoint = new URL("/web/src/workers/io.worker.ts", location.href).toString();
    const ioWorkerWrapperUrl = URL.createObjectURL(
      new Blob(
        [
          `\n            (async () => {\n              const MAX_ERROR_CHARS = 512;\n              const fallbackFormatErr = (err) => {\n                const msg = err instanceof Error ? err.message : err;\n                return String(msg ?? \"Error\")\n                  .replace(/[\\x00-\\x1F\\x7F]/g, \" \")\n                  .replace(/\\s+/g, \" \")\n                  .trim()\n                  .slice(0, MAX_ERROR_CHARS);\n              };\n\n              let formatErr = fallbackFormatErr;\n              try {\n                const mod = await import(\"/web/src/text.ts\");\n                const formatOneLineUtf8 = mod?.formatOneLineUtf8;\n                if (typeof formatOneLineUtf8 === \"function\") {\n                  formatErr = (err) => {\n                    const msg = err instanceof Error ? err.message : err;\n                    return formatOneLineUtf8(String(msg ?? \"\"), 512) || \"Error\";\n                  };\n                }\n              } catch {\n                // ignore: keep fallbackFormatErr\n              }\n\n              try {\n                await import(${JSON.stringify(ioWorkerEntrypoint)});\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_imported\" }), 0);\n              } catch (err) {\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_import_failed\", message: formatErr(err) }), 0);\n              }\n            })();\n          `,
        ],
        { type: "text/javascript" },
      ),
    );
    const ioWorker = new Worker(ioWorkerWrapperUrl, { type: "module" });

    // Avoid dropping early messages on WebKit by waiting until the imported worker module has run.
    const ioWorkerImported = new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        cleanup();
        reject(new Error("Timed out waiting for io.worker import marker"));
      }, 20_000);
      (timer as unknown as { unref?: () => void }).unref?.();

      const handler = (ev: MessageEvent): void => {
        const data = ev.data as { type?: unknown; message?: unknown } | undefined;
        if (!data) return;
        if (data.type === "__aero_io_worker_imported") {
          cleanup();
          resolve();
          return;
        }
        if (data.type === "__aero_io_worker_import_failed") {
          cleanup();
          reject(new Error(`io.worker wrapper import failed: ${typeof data.message === "string" ? data.message : "unknown error"}`));
        }
      };

      const errorHandler = (err: Event) => {
        cleanup();
        const e = err as any;
        const message =
          typeof e?.message === "string"
            ? e.message
            : typeof e?.error?.message === "string"
              ? e.error.message
              : String(err);
        const filename = typeof e?.filename === "string" ? e.filename : "?";
        const lineno = typeof e?.lineno === "number" ? e.lineno : "?";
        const colno = typeof e?.colno === "number" ? e.colno : "?";
        reject(new Error(`io.worker wrapper error during import: ${message} (${filename}:${lineno}:${colno})`));
      };

      const messageErrorHandler = () => {
        cleanup();
        reject(new Error("io.worker wrapper messageerror during import"));
      };

      const cleanup = () => {
        clearTimeout(timer);
        ioWorker.removeEventListener("message", handler);
        ioWorker.removeEventListener("error", errorHandler);
        ioWorker.removeEventListener("messageerror", messageErrorHandler);
      };

      ioWorker.addEventListener("message", handler);
      ioWorker.addEventListener("error", errorHandler);
      ioWorker.addEventListener("messageerror", messageErrorHandler);
    });

    let workerError: unknown = null;
    const onWorkerMessage = (ev: MessageEvent) => {
      const data = ev.data as { type?: unknown; message?: unknown };
      if (data && data.type === MessageType.ERROR) {
        workerError = data;
      }
    };
    const onWorkerError = (ev: Event) => {
      const msg = (ev as ErrorEvent).message || "worker error";
      workerError = { type: "error", message: msg };
    };
    const onWorkerMessageError = () => {
      workerError = { type: "messageerror" };
    };
    ioWorker.addEventListener("message", onWorkerMessage);
    ioWorker.addEventListener("error", onWorkerError);
    ioWorker.addEventListener("messageerror", onWorkerMessageError);

    const waitForAtomic = async (idx: number, expected: number, timeoutMs: number): Promise<void> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        if (workerError) throw new Error(`IO worker error: ${JSON.stringify(workerError)}`);
        if (Atomics.load(status, idx) === expected) return;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(`Timed out waiting for status[${idx}] == ${expected} after ${timeoutMs}ms.`);
    };

    const waitForCounterGreaterThan = async (idx: number, prev: number, timeoutMs: number): Promise<number> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        if (workerError) throw new Error(`IO worker error: ${JSON.stringify(workerError)}`);
        const cur = Atomics.load(status, idx) >>> 0;
        if (cur > prev) return cur;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(`Timed out waiting for status[${idx}] to advance past ${prev} after ${timeoutMs}ms.`);
    };

    const sendValidInputBatch = async (): Promise<void> => {
      let lastError: Error | null = null;
      for (let attempt = 0; attempt < 3; attempt += 1) {
        const before = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
        const beforeEvents = Atomics.load(status, StatusIndex.IoInputEventCounter) >>> 0;
        const q = new InputEventQueue(8);
        const nowUs = Math.round(performance.now() * 1000) >>> 0;
        // Send a press+release pair so the IO worker doesn't retain "held key" state across cases.
        q.pushKeyHidUsage(nowUs, 0x04, true);
        q.pushKeyHidUsage(nowUs, 0x04, false);

        q.flush(
          {
            postMessage: (msg, transfer) => {
              ioWorker.postMessage(msg, transfer);
            },
          },
        );

        try {
          await waitForCounterGreaterThan(StatusIndex.IoInputBatchCounter, before, 750);
          // Ensure the batch was actually processed (not just recycled) by observing the event counter.
          // Two HID usage events were sent above. Only require >=1 so this check remains robust even
          // if future input pipelines coalesce events differently; the main purpose is to avoid
          // treating an unrelated batch counter increment (e.g. a clamped malformed batch) as proof
          // that this follow-up batch was processed.
          await waitForCounterGreaterThan(StatusIndex.IoInputEventCounter, beforeEvents, 750);
          return;
        } catch (err) {
          lastError = err instanceof Error ? err : new Error(String(err));
        }
      }
      throw lastError ?? new Error("Failed to send a valid input batch");
    };

    const runCase = async (
      sendMalformed: () => void,
    ): Promise<{ batchBefore: number; batchAfter: number; dropsBefore: number; dropsAfter: number }> => {
      const batchBefore = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      const dropsBefore = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;
      sendMalformed();
      await sendValidInputBatch();
      const batchAfter = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      const dropsAfter = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;
      return { batchBefore, batchAfter, dropsBefore, dropsAfter };
    };

    try {
      await ioWorkerImported;

      // io.worker waits for an initial boot disk selection message before reporting READY.
      ioWorker.postMessage(emptySetBootDisksMessage());
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

      await waitForAtomic(StatusIndex.IoReady, 1, 20_000);

      // Preflight: ensure the worker has fully started and can process a valid batch before we
      // inject malformed payloads (avoids flakiness if IoReady is observed slightly before the
      // IO IPC server flips `started=true`).
      await sendValidInputBatch();

      const countersBefore = {
        received: Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0,
        processed: Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0,
        dropped: Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0,
      };

      const caseA = await runCase(() => {
        // byteLength not divisible by 4.
        // Use >= header size so a regression that checks "too small" first but still constructs an
        // Int32Array could still crash (RangeError) without an explicit alignment guard.
        const buffer = new ArrayBuffer(10);
        ioWorker.postMessage({ type: "in:input-batch", buffer }, [buffer]);
      });

      const caseB = await runCase(() => {
        // Header-only buffer with a huge count.
        const buffer = new ArrayBuffer(8);
        const words = new Int32Array(buffer);
        words[0] = 0x7fffffff;
        words[1] = 0;
        ioWorker.postMessage({ type: "in:input-batch", buffer }, [buffer]);
      });

      const countersAfter = {
        received: Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0,
        processed: Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0,
        dropped: Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0,
      };

      return { ok: true, workerError, countersBefore, countersAfter, caseA, caseB };
    } finally {
      ioWorker.removeEventListener("message", onWorkerMessage);
      ioWorker.removeEventListener("error", onWorkerError);
      ioWorker.removeEventListener("messageerror", onWorkerMessageError);
      ioWorker.terminate();
      URL.revokeObjectURL(ioWorkerWrapperUrl);
    }
  });

  expect(result.ok).toBe(true);
  expect(result.workerError).toBeNull();
  expect(result.caseA.batchAfter).toBeGreaterThan(result.caseA.batchBefore);
  expect(result.caseA.dropsAfter).toBeGreaterThan(result.caseA.dropsBefore);
  expect(result.caseB.batchAfter).toBeGreaterThan(result.caseB.batchBefore);
  expect(result.caseB.dropsAfter).toBeGreaterThan(result.caseB.dropsBefore);

  const receivedDelta = (result.countersAfter.received - result.countersBefore.received) >>> 0;
  const processedDelta = (result.countersAfter.processed - result.countersBefore.processed) >>> 0;
  const droppedDelta = (result.countersAfter.dropped - result.countersBefore.dropped) >>> 0;
  expect(receivedDelta).toBeGreaterThanOrEqual(4);
  expect(processedDelta).toBeGreaterThanOrEqual(2);
  expect(droppedDelta).toBeGreaterThanOrEqual(2);
  // `IoInputBatchDropCounter` increments for malformed batches *and* for batches where the claimed
  // event count is clamped. A single received batch can therefore contribute to both "processed"
  // and "dropped", so we avoid assuming `processed + dropped <= received`.
  expect(receivedDelta).toBeGreaterThanOrEqual(processedDelta);
  expect(receivedDelta).toBeGreaterThanOrEqual(droppedDelta);
});
