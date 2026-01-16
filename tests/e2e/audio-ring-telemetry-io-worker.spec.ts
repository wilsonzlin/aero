import { expect, test } from "@playwright/test";

test("IO worker publishes AudioWorklet ring telemetry into StatusIndex.Audio*", async ({ page }) => {
  test.setTimeout(30_000);
  await page.goto("/", { waitUntil: "load" });

  const support = await page.evaluate(() => {
    let wasm = false;
    let wasmThreads = false;
    try {
      wasm = typeof WebAssembly !== "undefined" && typeof WebAssembly.Memory === "function";
    } catch {
      wasm = false;
    }
    if (wasm) {
      try {
        // eslint-disable-next-line no-new
        new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
        wasmThreads = true;
      } catch {
        wasmThreads = false;
      }
    }
    return {
      crossOriginIsolated: globalThis.crossOriginIsolated === true,
      sharedArrayBuffer: typeof SharedArrayBuffer !== "undefined",
      atomics: typeof Atomics !== "undefined",
      worker: typeof Worker !== "undefined",
      wasm,
      wasmThreads,
    };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics, "Atomics is unavailable in this browser configuration.");
  test.skip(!support.worker, "Web Workers are unavailable in this environment.");
  test.skip(!support.wasm, "WebAssembly.Memory is unavailable in this environment.");
  test.skip(!support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");

  const result = await page.evaluate(async () => {
    const { allocateHarnessSharedMemorySegments } = await import("/web/src/runtime/harness_shared_memory.ts");
    const { StatusIndex, createIoIpcSab, createSharedMemoryViews } = await import("/web/src/runtime/shared_layout.ts");
    const { MessageType } = await import("/web/src/runtime/protocol.ts");
    const { emptySetBootDisksMessage } = await import("/web/src/runtime/boot_disks_protocol.ts");
    const { requiredBytes: audioRequiredBytes, wrapRingBuffer: wrapAudioRingBuffer } = await import("/web/src/audio/audio_worklet_ring.ts");
    const { openRingByKind } = await import("/web/src/ipc/ipc.ts");
    const { queueKind } = await import("/web/src/ipc/layout.ts");
    const { encodeCommand, decodeEvent } = await import("/web/src/ipc/protocol.ts");

    // This test only needs:
    // - a SharedArrayBuffer-backed status/control SAB, and
    // - enough guest RAM for the IO worker to instantiate (it does not execute the VM).
    //
    // Use the harness allocator so we don't reserve the full wasm32 runtime region (~128MiB).
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(64),
      sharedFramebufferOffsetBytes: 0,
      ioIpc: createIoIpcSab({ includeNet: false, includeHidIn: false }),
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);
    const status = views.status;
    const controlSab = segments.control;
    const guestMemory = segments.guestMemory;
    const ioIpcSab = segments.ioIpc;
    const sharedFramebuffer = segments.sharedFramebuffer;

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
    let workerError: string | null = null;

    ioWorker.addEventListener("message", (ev) => {
      const data = ev.data as unknown;
      if (!data || typeof data !== "object") return;
      const msg = data as { type?: unknown; message?: unknown };
      if (msg.type === MessageType.ERROR) {
        workerError = typeof msg.message === "string" ? msg.message : "IO worker reported an unknown error";
      }
    });

    ioWorker.addEventListener("error", (ev) => {
      workerError = ev.message || "IO worker error";
    });

    const waitForMessage = (predicate: (data: unknown) => boolean, timeoutMs = 10_000): Promise<unknown> => {
      return new Promise((resolve, reject) => {
        const timer = globalThis.setTimeout(() => {
          cleanup();
          reject(new Error(`Timed out waiting for IO worker message after ${timeoutMs}ms.`));
        }, timeoutMs);
        (timer as unknown as { unref?: () => void }).unref?.();

        const onMessage = (ev: MessageEvent<unknown>) => {
          const data = ev.data;
          if (data && typeof data === "object") {
            const msg = data as { type?: unknown; message?: unknown };
            if (msg.type === MessageType.ERROR) {
              cleanup();
              reject(new Error(typeof msg.message === "string" ? msg.message : "IO worker reported an unknown error"));
              return;
            }
          }
          if (!predicate(data)) return;
          cleanup();
          resolve(data);
        };
        const onError = (ev: ErrorEvent) => {
          cleanup();
          reject(new Error(ev.message || "IO worker error"));
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

    const waitForStatus = async (
      expected: { level: number; underrun: number; overrun: number },
      timeoutMs = 2_000,
    ): Promise<{ level: number; underrun: number; overrun: number }> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      let last = { level: 0, underrun: 0, overrun: 0 };
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        if (workerError) {
          throw new Error(`IO worker failed: ${workerError}`);
        }
        const level = Atomics.load(status, StatusIndex.AudioBufferLevelFrames) >>> 0;
        const underrun = Atomics.load(status, StatusIndex.AudioUnderrunCount) >>> 0;
        const overrun = Atomics.load(status, StatusIndex.AudioOverrunCount) >>> 0;
        last = { level, underrun, overrun };
        if (level === expected.level && underrun === expected.underrun && overrun === expected.overrun) {
          return { level, underrun, overrun };
        }
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(
        `Timed out waiting for audio telemetry status. ` +
          `Expected level=${expected.level} underrun=${expected.underrun} overrun=${expected.overrun}. ` +
          `Last observed level=${last.level} underrun=${last.underrun} overrun=${last.overrun}.`,
      );
    };

    try {
      // Avoid dropping early messages on WebKit by waiting until the imported worker module has run.
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("Timed out waiting for io.worker import marker")), 20_000);
        const handler = (ev: MessageEvent): void => {
          const data = ev.data as { type?: unknown; message?: unknown } | undefined;
          if (!data) return;
          if (data.type === "__aero_io_worker_imported") {
            clearTimeout(timer);
            ioWorker.removeEventListener("message", handler);
            resolve();
            return;
          }
          if (data.type === "__aero_io_worker_import_failed") {
            clearTimeout(timer);
            ioWorker.removeEventListener("message", handler);
            reject(
              new Error(`io.worker wrapper import failed: ${typeof data.message === "string" ? data.message : "unknown error"}`),
            );
          }
        };
        ioWorker.addEventListener("message", handler);
      });

      ioWorker.postMessage({
        kind: "init",
        role: "io",
        controlSab,
        guestMemory,
        ioIpcSab,
        sharedFramebuffer,
        sharedFramebufferOffsetBytes: 0,
      });

      // io.worker waits for `setBootDisks` before reporting READY.
      ioWorker.postMessage(emptySetBootDisksMessage());

      await waitForMessage((data) => {
        if (!data || typeof data !== "object") return false;
        const msg = data as { type?: unknown; role?: unknown };
        return msg.type === MessageType.READY && msg.role === "io";
      });

      // Smoke-check that the IO IPC server loop is alive by sending a NOP and waiting for an ACK.
      const ioCmd = openRingByKind(ioIpcSab, queueKind.CMD);
      const ioEvt = openRingByKind(ioIpcSab, queueKind.EVT);
      const nopSeq = 1;
      const nopBytes = encodeCommand({ kind: "nop", seq: nopSeq });
      const startAck = typeof performance?.now === "function" ? performance.now() : Date.now();
      let pushed = false;
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - startAck < 2_000) {
        if (workerError) throw new Error(`IO worker failed before ACK: ${workerError}`);
        if (ioCmd.tryPush(nopBytes)) {
          pushed = true;
          break;
        }
        await new Promise((resolve) => setTimeout(resolve, 1));
      }
      if (!pushed) {
        throw new Error("Timed out pushing NOP into IO IPC cmd ring.");
      }
      let acked = false;
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - startAck < 2_000) {
        if (workerError) throw new Error(`IO worker failed before ACK: ${workerError}`);
        const bytes = ioEvt.tryPop();
        if (!bytes) {
          await new Promise((resolve) => setTimeout(resolve, 1));
          continue;
        }
        try {
          const evt = decodeEvent(bytes);
          if (evt.kind === "ack" && evt.seq === nopSeq) {
            acked = true;
            break;
          }
        } catch {
          // ignore malformed events
        }
      }
      if (!acked) {
        throw new Error("Timed out waiting for IO IPC NOP ack.");
      }

      const capacityFrames = 128;
      const channelCount = 2;
      const ringBuffer = new SharedArrayBuffer(audioRequiredBytes(capacityFrames, channelCount));
      const views = wrapAudioRingBuffer(ringBuffer, capacityFrames, channelCount);
      Atomics.store(views.readIndex, 0, 0);
      Atomics.store(views.writeIndex, 0, 0);
      Atomics.store(views.underrunCount, 0, 0);
      Atomics.store(views.overrunCount, 0, 0);

      ioWorker.postMessage({
        type: "setAudioRingBuffer",
        ringBuffer,
        capacityFrames,
        channelCount,
        dstSampleRate: 48_000,
      });

      // Simulate the AudioWorklet consumer and guest producer moving indices in the ring header.
      Atomics.store(views.readIndex, 0, 0);
      Atomics.store(views.writeIndex, 0, 64);
      Atomics.store(views.underrunCount, 0, 123);
      Atomics.store(views.overrunCount, 0, 456);
      const sample1 = await waitForStatus({ level: 64, underrun: 123, overrun: 456 });

      Atomics.store(views.writeIndex, 0, 100);
      Atomics.store(views.underrunCount, 0, 124);
      Atomics.store(views.overrunCount, 0, 457);
      const sample2 = await waitForStatus({ level: 100, underrun: 124, overrun: 457 });

      // Detach the ring; IO worker should clear telemetry once.
      ioWorker.postMessage({
        type: "setAudioRingBuffer",
        ringBuffer: null,
        capacityFrames: 0,
        channelCount: 0,
        dstSampleRate: 0,
      });
      const cleared = await waitForStatus({ level: 0, underrun: 0, overrun: 0 });

      return { sample1, sample2, cleared };
    } finally {
      ioWorker.terminate();
      URL.revokeObjectURL(ioWorkerWrapperUrl);
    }
  });

  expect(result.sample1.level).toBe(64);
  expect(result.sample2.level).toBe(100);
  expect(result.cleared.level).toBe(0);
});
