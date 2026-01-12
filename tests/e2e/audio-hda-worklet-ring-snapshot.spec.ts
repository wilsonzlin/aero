import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("WASM HDA snapshot restores AudioWorklet ring indices and clears samples", async ({ page }) => {
  test.setTimeout(120_000);
  test.skip(test.info().project.name !== "chromium", "SharedArrayBuffer + WASM snapshot test only runs on Chromium.");

  await page.goto(`${PREVIEW_ORIGIN}/web/?mem=256`, { waitUntil: "load" });

  // Wait for the main-thread WASM API to be available.
  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const api = (globalThis as any).__aeroWasmApi;
    return !!api;
  }, undefined, { timeout: 60_000 });

  const res = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const api = (globalThis as any).__aeroWasmApi as any;
    if (typeof SharedArrayBuffer === "undefined" || typeof Atomics === "undefined") {
      return { ok: false, reason: "SharedArrayBuffer/Atomics unavailable" };
    }
    if (!api || typeof api.HdaControllerBridge !== "function") {
      return { ok: false, reason: "Missing HdaControllerBridge export" };
    }

    const Hda = api.HdaControllerBridge as any;
    const hda = new Hda(1, 1);

    if (typeof hda.attach_audio_ring !== "function" || typeof hda.detach_audio_ring !== "function") {
      return { ok: false, reason: "HdaControllerBridge ring attach exports unavailable" };
    }
    if (typeof hda.save_state !== "function" || typeof hda.load_state !== "function") {
      return { ok: false, reason: "HdaControllerBridge snapshot exports unavailable" };
    }

    // AudioWorklet ring layout (must match `web/src/platform/audio_worklet_ring_layout.js`).
    const READ_FRAME_INDEX = 0;
    const WRITE_FRAME_INDEX = 1;
    const HEADER_U32_LEN = 4;
    const HEADER_BYTES = 16;

    const capacityFrames = 8;
    const channelCount = 2;
    const sab = new SharedArrayBuffer(HEADER_BYTES + capacityFrames * channelCount * 4);

    const header = new Uint32Array(sab, 0, HEADER_U32_LEN);
    const samples = new Float32Array(sab, HEADER_BYTES, capacityFrames * channelCount);

    // Attach the ring.
    hda.attach_audio_ring(sab, capacityFrames, channelCount);

    // Seed indices + sample payload.
    Atomics.store(header, READ_FRAME_INDEX, 2);
    Atomics.store(header, WRITE_FRAME_INDEX, 6);
    samples.fill(123.0);

    const snap = hda.save_state() as Uint8Array;

    // Corrupt both indices and samples.
    Atomics.store(header, READ_FRAME_INDEX, 123);
    Atomics.store(header, WRITE_FRAME_INDEX, 456);
    samples.fill(456.0);

    // Restore.
    hda.load_state(snap);

    const read1 = Atomics.load(header, READ_FRAME_INDEX) >>> 0;
    const write1 = Atomics.load(header, WRITE_FRAME_INDEX) >>> 0;
    let cleared1 = true;
    for (let i = 0; i < samples.length; i += 1) {
      if (samples[i] !== 0) {
        cleared1 = false;
        break;
      }
    }

    // Now exercise deferred ring restore: detach, restore state (should defer), reattach.
    hda.detach_audio_ring();
    Atomics.store(header, READ_FRAME_INDEX, 999);
    Atomics.store(header, WRITE_FRAME_INDEX, 1000);
    samples.fill(1.0);

    hda.load_state(snap);
    const deferredRead = Atomics.load(header, READ_FRAME_INDEX) >>> 0;

    hda.attach_audio_ring(sab, capacityFrames, channelCount);

    const read2 = Atomics.load(header, READ_FRAME_INDEX) >>> 0;
    const write2 = Atomics.load(header, WRITE_FRAME_INDEX) >>> 0;
    let cleared2 = true;
    for (let i = 0; i < samples.length; i += 1) {
      if (samples[i] !== 0) {
        cleared2 = false;
        break;
      }
    }

    hda.free();

    return {
      ok: true,
      read1,
      write1,
      cleared1,
      deferredRead,
      read2,
      write2,
      cleared2,
    };
  });

  if (!res.ok) {
    throw new Error(`Precondition failed: ${res.reason}`);
  }

  expect(res.read1).toBe(2);
  expect(res.write1).toBe(6);
  expect(res.cleared1).toBe(true);

  // While detached, `load_state` must not be able to restore indices yet.
  expect(res.deferredRead).toBe(999);

  // On reattach, deferred ring state should be applied.
  expect(res.read2).toBe(2);
  expect(res.write2).toBe(6);
  expect(res.cleared2).toBe(true);
});
