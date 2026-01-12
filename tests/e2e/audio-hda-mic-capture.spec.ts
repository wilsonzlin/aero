import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("HDA capture stream DMA-writes microphone PCM into guest RAM (synthetic mic)", async ({ page }) => {
  test.setTimeout(60_000);
  test.skip(test.info().project.name !== "chromium", "HDA mic capture test only runs on Chromium.");

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return Boolean((globalThis as any).__aeroWorkerCoordinator);
  });

  await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator as any;

    const workerConfig = {
      guestMemoryMiB: 64,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };

    coord.start(workerConfig);
    // io.worker waits for the first `setBootDisks` message before reporting READY.
    coord.getIoWorker()?.postMessage({ type: "setBootDisks", mounts: {}, hdd: null, cd: null });

    // Force mic ring ownership to the IO worker so the HDA capture engine is the consumer.
    coord.setMicrophoneRingBufferOwner("io");

    // Create a deterministic mono f32 mic ring buffer and prefill it with samples.
    const capacitySamples = 16_384;
    const headerU32Len = 4;
    const headerBytes = headerU32Len * Uint32Array.BYTES_PER_ELEMENT;
    const sab = new SharedArrayBuffer(headerBytes + capacitySamples * Float32Array.BYTES_PER_ELEMENT);
    const header = new Uint32Array(sab, 0, headerU32Len);
    const samples = new Float32Array(sab, headerBytes, capacitySamples);

    // Deterministic square wave (-0.75, +0.75, ...).
    for (let i = 0; i < capacitySamples; i++) {
      samples[i] = (i & 1) === 0 ? 0.75 : -0.75;
    }

    // Header layout matches `web/src/audio/mic_ring.js`:
    // 0=write_pos, 1=read_pos, 2=dropped, 3=capacity.
    Atomics.store(header, 0, capacitySamples >>> 0);
    Atomics.store(header, 1, 0);
    Atomics.store(header, 2, 0);
    Atomics.store(header, 3, capacitySamples >>> 0);

    coord.setMicrophoneRingBuffer(sab, 48_000);
  });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator as any;
    return coord?.getWorkerStatuses?.().io?.state === "ready";
  });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator as any;
    return Boolean(coord?.getWorkerWasmStatus?.("io"));
  });

  const pcm = await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator as any;
    const io = coord.getIoWorker?.();
    if (!io) throw new Error("Missing IO worker");

    const requestId = 1;
    return await new Promise<ArrayBuffer>((resolve, reject) => {
      const timeoutMs = 10_000;
      const timer = setTimeout(() => {
        io.removeEventListener("message", onMessage as any);
        reject(new Error(`Timed out waiting for hda.micCaptureTest.result (${timeoutMs}ms)`));
      }, timeoutMs);

      const onMessage = (ev: MessageEvent<any>) => {
        const msg = ev.data;
        if (!msg || typeof msg !== "object") return;
        if (msg.type !== "hda.micCaptureTest.result") return;
        if (msg.requestId !== requestId) return;
        clearTimeout(timer);
        io.removeEventListener("message", onMessage as any);
        if (msg.ok) {
          resolve(msg.pcm as ArrayBuffer);
        } else {
          reject(new Error(typeof msg.error === "string" ? msg.error : "HDA mic capture test failed"));
        }
      };

      io.addEventListener("message", onMessage as any);
      io.postMessage({ type: "hda.micCaptureTest", requestId });
    });
  });

  const bytes = new Uint8Array(pcm);
  let nonZero = 0;
  for (const b of bytes) if (b !== 0) nonZero += 1;

  expect(nonZero).toBeGreaterThan(0);
});

