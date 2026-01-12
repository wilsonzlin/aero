import { expect, test } from "@playwright/test";

import {
  CAPACITY_SAMPLES_INDEX as MIC_CAPACITY_SAMPLES_INDEX,
  DROPPED_SAMPLES_INDEX as MIC_DROPPED_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
  READ_POS_INDEX as MIC_READ_POS_INDEX,
  WRITE_POS_INDEX as MIC_WRITE_POS_INDEX,
} from "../../web/src/audio/mic_ring.js";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("HDA capture stream DMA-writes microphone PCM into guest RAM (synthetic mic)", async ({ page }) => {
  // Worker + WASM bring-up can be slow in CI/headless Chromium, especially without a cached
  // compilation artifact. Keep this comfortably above any internal timeouts.
  test.setTimeout(90_000);
  test.skip(test.info().project.name !== "chromium", "HDA mic capture test only runs on Chromium.");
  page.setDefaultTimeout(90_000);

  // The harness programs the HDA capture stream at 48kHz but we intentionally publish a
  // different mic sample rate to exercise the capture resampler + sample-rate plumbing.
  const micSampleRateHz = 44_100;
  const captureStreamRateHz = 48_000;

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return Boolean((globalThis as any).__aeroWorkerCoordinator);
  });

  await page.evaluate(
    ({
      MIC_CAPACITY_SAMPLES_INDEX,
      MIC_DROPPED_SAMPLES_INDEX,
      MIC_HEADER_BYTES,
      MIC_HEADER_U32_LEN,
      MIC_READ_POS_INDEX,
      MIC_WRITE_POS_INDEX,
      micSampleRateHz,
    }) => {
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
    const sab = new SharedArrayBuffer(MIC_HEADER_BYTES + capacitySamples * Float32Array.BYTES_PER_ELEMENT);
    const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);
    const samples = new Float32Array(sab, MIC_HEADER_BYTES, capacitySamples);

    // Deterministic square wave (-0.75, +0.75, ...).
    for (let i = 0; i < capacitySamples; i++) {
      samples[i] = (i & 1) === 0 ? 0.75 : -0.75;
    }

    // Header layout matches `web/src/audio/mic_ring.js`.
    Atomics.store(header, MIC_WRITE_POS_INDEX, capacitySamples >>> 0);
    Atomics.store(header, MIC_READ_POS_INDEX, 0);
    Atomics.store(header, MIC_DROPPED_SAMPLES_INDEX, 0);
    Atomics.store(header, MIC_CAPACITY_SAMPLES_INDEX, capacitySamples >>> 0);

    // Expose for the assertion step so we can verify the IO worker consumed samples.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroTestMicRingSab = sab;

    coord.setMicrophoneRingBuffer(sab, micSampleRateHz);
    },
    {
      MIC_CAPACITY_SAMPLES_INDEX,
      MIC_DROPPED_SAMPLES_INDEX,
      MIC_HEADER_BYTES,
      MIC_HEADER_U32_LEN,
      MIC_READ_POS_INDEX,
      MIC_WRITE_POS_INDEX,
      micSampleRateHz,
    },
  );

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

  const first = await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator as any;
    const io = coord.getIoWorker?.();
    if (!io) throw new Error("Missing IO worker");

    const requestId = 1;
    return await new Promise<{ pcm: ArrayBuffer; lpibBefore: number; lpibAfter: number }>((resolve, reject) => {
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
          resolve({
            pcm: msg.pcm as ArrayBuffer,
            lpibBefore: (msg.lpibBefore ?? 0) >>> 0,
            lpibAfter: (msg.lpibAfter ?? 0) >>> 0,
          });
        } else {
          reject(new Error(typeof msg.error === "string" ? msg.error : "HDA mic capture test failed"));
        }
      };

      io.addEventListener("message", onMessage as any);
      io.postMessage({ type: "hda.micCaptureTest", requestId });
    });
  });

  // 1024 frames @ 16-bit mono = 2048 bytes.
  const expectedLpibDelta = 1024 * 2;
  expect(((first.lpibAfter - first.lpibBefore) >>> 0) >>> 0).toBe(expectedLpibDelta);

  const bytes = new Uint8Array(first.pcm);
  let nonZero = 0;
  for (const b of bytes) if (b !== 0) nonZero += 1;

  expect(nonZero).toBeGreaterThan(0);

  // Decode as signed 16-bit PCM (the harness programs 16-bit mono).
  const view = new DataView(first.pcm);
  let posSamples = 0;
  let negSamples = 0;
  for (let off = 0; off + 1 < view.byteLength; off += 2) {
    const s = view.getInt16(off, true);
    if (s > 0) posSamples += 1;
    else if (s < 0) negSamples += 1;
  }
  expect(posSamples).toBeGreaterThan(0);
  expect(negSamples).toBeGreaterThan(0);

  // Confirm that the mic ring consumer advanced (IO worker actually read from the ring).
  const micAfter = await page.evaluate(
    ({ MIC_HEADER_U32_LEN, MIC_READ_POS_INDEX, MIC_WRITE_POS_INDEX, MIC_DROPPED_SAMPLES_INDEX }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const sab = (globalThis as any).__aeroTestMicRingSab as SharedArrayBuffer | undefined;
      if (!sab) return null;
      const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);
      return {
        read: Atomics.load(header, MIC_READ_POS_INDEX) >>> 0,
        write: Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0,
        dropped: Atomics.load(header, MIC_DROPPED_SAMPLES_INDEX) >>> 0,
      };
    },
    { MIC_HEADER_U32_LEN, MIC_READ_POS_INDEX, MIC_WRITE_POS_INDEX, MIC_DROPPED_SAMPLES_INDEX },
  );

  const expectedReadAfterFirstCapture = (() => {
    const dstFrames = 1024;
    const step = micSampleRateHz / captureStreamRateHz;
    const lastPos = (dstFrames - 1) * step;
    const idx = Math.floor(lastPos);
    const frac = lastPos - idx;
    // Match `LinearResampler.required_source_frames` in `crates/aero-audio/src/pcm.rs`.
    return Math.abs(frac) <= 1e-12 ? idx + 1 : idx + 2;
  })();
  expect(micAfter?.read ?? 0).toBe(expectedReadAfterFirstCapture);

  // Empty the mic ring (no available samples) and ensure capture still completes and produces silence.
  const micBeforeSilence = micAfter;
  await page.evaluate(
    ({ MIC_HEADER_U32_LEN, MIC_READ_POS_INDEX, MIC_WRITE_POS_INDEX }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const sab = (globalThis as any).__aeroTestMicRingSab as SharedArrayBuffer | undefined;
      if (!sab) return;
      const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);
      const read = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
      Atomics.store(header, MIC_WRITE_POS_INDEX, read);
    },
    { MIC_HEADER_U32_LEN, MIC_READ_POS_INDEX, MIC_WRITE_POS_INDEX },
  );

  const silence = await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator as any;
    const io = coord.getIoWorker?.();
    if (!io) throw new Error("Missing IO worker");

    const requestId = 2;
    return await new Promise<{ pcm: ArrayBuffer; lpibBefore: number; lpibAfter: number }>((resolve, reject) => {
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
          resolve({
            pcm: msg.pcm as ArrayBuffer,
            lpibBefore: (msg.lpibBefore ?? 0) >>> 0,
            lpibAfter: (msg.lpibAfter ?? 0) >>> 0,
          });
        } else {
          reject(new Error(typeof msg.error === "string" ? msg.error : "HDA mic capture test failed"));
        }
      };

      io.addEventListener("message", onMessage as any);
      io.postMessage({ type: "hda.micCaptureTest", requestId });
    });
  });

  expect(((silence.lpibAfter - silence.lpibBefore) >>> 0) >>> 0).toBe(expectedLpibDelta);
  const silenceBytes = new Uint8Array(silence.pcm);
  let silenceNonZero = 0;
  for (const b of silenceBytes) if (b !== 0) silenceNonZero += 1;
  expect(silenceNonZero).toBe(0);

  const micAfterSilence = await page.evaluate(
    ({ MIC_HEADER_U32_LEN, MIC_READ_POS_INDEX, MIC_WRITE_POS_INDEX, MIC_DROPPED_SAMPLES_INDEX }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const sab = (globalThis as any).__aeroTestMicRingSab as SharedArrayBuffer | undefined;
      if (!sab) return null;
      const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);
      return {
        read: Atomics.load(header, MIC_READ_POS_INDEX) >>> 0,
        write: Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0,
        dropped: Atomics.load(header, MIC_DROPPED_SAMPLES_INDEX) >>> 0,
      };
    },
    { MIC_HEADER_U32_LEN, MIC_READ_POS_INDEX, MIC_WRITE_POS_INDEX, MIC_DROPPED_SAMPLES_INDEX },
  );
  expect(micAfterSilence).toBeTruthy();
  // No samples available -> consumer should not advance.
  expect(micAfterSilence?.read).toBe(micBeforeSilence?.read);
});
