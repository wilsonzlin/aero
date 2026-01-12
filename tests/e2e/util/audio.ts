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
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any)[globalVarName];
      if (!out?.ringBuffer?.samples || !out?.ringBuffer?.writeIndex) return false;
      const samples: Float32Array = out.ringBuffer.samples;
      const writeIndex: Uint32Array = out.ringBuffer.writeIndex;
      const cc = out.ringBuffer.channelCount | 0;
      const cap = out.ringBuffer.capacityFrames | 0;
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
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any)[globalVarName];
      if (!out?.ringBuffer?.samples || !out?.ringBuffer?.writeIndex) return null;
      const samples: Float32Array = out.ringBuffer.samples;
      const writeIndex: Uint32Array = out.ringBuffer.writeIndex;
      const cc = out.ringBuffer.channelCount | 0;
      const cap = out.ringBuffer.capacityFrames | 0;
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

