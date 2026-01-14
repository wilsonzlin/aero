import type { Page } from "@playwright/test";

export type WaitForAudioOutputNonSilentOptions = {
  /**
   * Threshold for `maxAbs` to treat the ring as "non-silent".
   *
   * Most tone producers in the harness use gain=0.1, so `0.01` is a safe
   * low threshold that still catches "all zeros" regressions.
   */
  threshold?: number;
  /**
   * How many recent frames (per channel) to inspect when computing `maxAbs`.
   */
  framesToInspect?: number;
  /**
   * Playwright timeout (ms).
   */
  timeoutMs?: number;
};

export async function waitForAudioOutputNonSilent(
  page: Page,
  globalVarName: string,
  opts: WaitForAudioOutputNonSilentOptions = {},
): Promise<void> {
  const threshold = Number.isFinite(opts.threshold) ? (opts.threshold as number) : 0.01;
  const framesToInspect = Number.isFinite(opts.framesToInspect) ? (opts.framesToInspect as number) : 1024;
  const timeoutMs = Number.isFinite(opts.timeoutMs) ? (opts.timeoutMs as number) : 10_000;

  await page.waitForFunction(
    ({ globalVarName, threshold, framesToInspect }) => {
      const g = globalThis as unknown as Record<string, unknown>;
      const out = g[globalVarName];
      if (!out || typeof out !== "object") return false;
      const ringBuffer = (out as Record<string, unknown>).ringBuffer;
      if (!ringBuffer || typeof ringBuffer !== "object") return false;
      const ring = ringBuffer as Record<string, unknown>;
      const samples = ring.samples;
      const writeIndex = ring.writeIndex;
      if (!(samples instanceof Float32Array)) return false;
      if (!(writeIndex instanceof Uint32Array)) return false;
      const cc = typeof ring.channelCount === "number" ? ring.channelCount | 0 : 0;
      const cap = typeof ring.capacityFrames === "number" ? ring.capacityFrames | 0 : 0;
      if (cc <= 0 || cap <= 0) return false;

      const write = Atomics.load(writeIndex, 0) >>> 0;
      const frames = Math.min(framesToInspect as number, cap);
      if (frames <= 0) return false;

      const startFrame = (write - frames) >>> 0;
      let maxAbs = 0;
      for (let i = 0; i < frames; i++) {
        const frame = (startFrame + i) % cap;
        const base = frame * cc;
        for (let c = 0; c < cc; c++) {
          const s = samples[base + c] ?? 0;
          const a = Math.abs(s);
          if (a > maxAbs) maxAbs = a;
        }
      }
      return maxAbs > (threshold as number);
    },
    { globalVarName, threshold, framesToInspect },
    { timeout: timeoutMs },
  );
}

export async function getAudioOutputMaxAbsSample(
  page: Page,
  globalVarName: string,
  framesToInspect = 1024,
): Promise<number | null> {
  const frames = Number.isFinite(framesToInspect) ? framesToInspect : 1024;
  return await page.evaluate(
    ({ globalVarName, framesToInspect }) => {
      const g = globalThis as unknown as Record<string, unknown>;
      const out = g[globalVarName];
      if (!out || typeof out !== "object") return null;
      const ringBuffer = (out as Record<string, unknown>).ringBuffer;
      if (!ringBuffer || typeof ringBuffer !== "object") return null;
      const ring = ringBuffer as Record<string, unknown>;
      const samples = ring.samples;
      const writeIndex = ring.writeIndex;
      if (!(samples instanceof Float32Array)) return null;
      if (!(writeIndex instanceof Uint32Array)) return null;
      const cc = typeof ring.channelCount === "number" ? ring.channelCount | 0 : 0;
      const cap = typeof ring.capacityFrames === "number" ? ring.capacityFrames | 0 : 0;
      if (cc <= 0 || cap <= 0) return null;

      const write = Atomics.load(writeIndex, 0) >>> 0;
      const frames = Math.min(framesToInspect as number, cap);
      if (frames <= 0) return null;

      const startFrame = (write - frames) >>> 0;
      let maxAbs = 0;
      for (let i = 0; i < frames; i++) {
        const frame = (startFrame + i) % cap;
        const base = frame * cc;
        for (let c = 0; c < cc; c++) {
          const s = samples[base + c] ?? 0;
          const a = Math.abs(s);
          if (a > maxAbs) maxAbs = a;
        }
      }
      return maxAbs;
    },
    { globalVarName, framesToInspect: frames },
  );
}
