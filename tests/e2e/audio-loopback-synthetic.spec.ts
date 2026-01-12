import { expect, test } from "@playwright/test";

import { DROPPED_SAMPLES_INDEX, HEADER_U32_LEN, READ_POS_INDEX, WRITE_POS_INDEX } from "../../web/src/audio/mic_ring.js";
import { getAudioOutputMaxAbsSample, waitForAudioOutputNonSilent } from "./util/audio";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet loopback runs with synthetic microphone source (no underruns)", async ({ page }) => {
  test.skip(test.info().project.name !== "chromium", "AudioWorklet loopback test only runs on Chromium.");

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-loopback-synthetic");

  await page.waitForFunction(() => {
    // Exposed by the repo-root Vite harness UI (`src/main.ts`).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputLoopback;
    return out?.enabled === true && out?.context?.state === "running";
  });

  // Ensure the output ring is being filled with non-silent samples (and that we're not just
  // playing the CPU-worker fallback sine tone with gain=0.1).
  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputLoopback", { threshold: 0.12 });

  await page.waitForTimeout(1000);

  const result = await page.evaluate(({ DROPPED_SAMPLES_INDEX, HEADER_U32_LEN, READ_POS_INDEX, WRITE_POS_INDEX }) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputLoopback;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const mic = (globalThis as any).__aeroSyntheticMic as { ringBuffer?: SharedArrayBuffer } | undefined;
    const header = mic?.ringBuffer ? new Uint32Array(mic.ringBuffer, 0, HEADER_U32_LEN) : null;
    const micWritePos = header ? Atomics.load(header, WRITE_POS_INDEX) >>> 0 : null;
    const micReadPos = header ? Atomics.load(header, READ_POS_INDEX) >>> 0 : null;
    const micDropped = header ? Atomics.load(header, DROPPED_SAMPLES_INDEX) >>> 0 : null;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      bufferLevelFrames: typeof out?.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : null,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      backend: (globalThis as any).__aeroAudioLoopbackBackend ?? null,
      micWritePos,
      micReadPos,
      micDropped,
    };
  }, { DROPPED_SAMPLES_INDEX, HEADER_U32_LEN, READ_POS_INDEX, WRITE_POS_INDEX });

  const maxAbs = await getAudioOutputMaxAbsSample(page, "__aeroAudioOutputLoopback");

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  // Startup can be racy across CI environments; allow a tiny tolerance.
  // Underruns are counted as missing frames (a single render quantum is 128 frames).
  expect(result.underruns).toBeLessThanOrEqual(128);
  expect(result.bufferLevelFrames).toBeGreaterThan(0);
  // This demo is intended to validate worker plumbing end-to-end.
  expect(result.backend).toBe("worker");
  // Verify that something is actually consuming the mic ring; otherwise the CPU
  // worker could be playing its fallback tone and still keep audio running.
  expect(result.micWritePos).toBeGreaterThan(0);
  expect(result.micReadPos).toBeGreaterThan(0);
  // Confirm the output is actually being driven by the synthetic mic (gain=0.2),
  // not the CPU-worker fallback sine tone (gain=0.1).
  expect(maxAbs).not.toBeNull();
  expect(maxAbs as number).toBeGreaterThan(0.12);
});
