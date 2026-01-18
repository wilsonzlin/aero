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
    const { allocateHarnessSharedMemorySegments } = await import("/web/src/runtime/harness_shared_memory.ts");
    const { createIoIpcSab, createSharedMemoryViews, StatusIndex } = await import("/web/src/runtime/shared_layout.ts");
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

    // Avoid dropping early messages on WebKit by waiting until the imported worker module has run.
    await new Promise<void>((resolve, reject) => {
      let timer = 0;
      const cleanup = () => {
        if (timer) clearTimeout(timer);
        ioWorker.removeEventListener("message", messageHandler);
        ioWorker.removeEventListener("error", errorHandler);
        ioWorker.removeEventListener("messageerror", messageErrorHandler);
      };

      const messageHandler = (ev: MessageEvent): void => {
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

      ioWorker.addEventListener("message", messageHandler);
      ioWorker.addEventListener("error", errorHandler);
      ioWorker.addEventListener("messageerror", messageErrorHandler);
      timer = setTimeout(() => {
        cleanup();
        reject(new Error("Timed out waiting for io.worker import marker"));
      }, 20_000);
    });

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
    URL.revokeObjectURL(ioWorkerWrapperUrl);

    return { before, after, batches };
  });

  expect(result.after.batchesDropped).toBeGreaterThan(result.before.batchesDropped);
  expect(result.after.batchesReceived).toBeGreaterThanOrEqual(result.before.batchesReceived + result.batches);
});
