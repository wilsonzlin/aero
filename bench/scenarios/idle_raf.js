import { createAeroPage, exportPerf, resetPerf, waitForAeroReady } from "../aero_page.js";

function percentile(sorted, p) {
  if (!sorted.length) return NaN;
  const clamped = Math.max(0, Math.min(1, p));
  const idx = Math.floor(clamped * (sorted.length - 1));
  return sorted[idx];
}

const scenario = {
  id: 'idle_raf',
  name: 'idle_raf',

  /**
   * @param {import('../runner.js').ScenarioRunContext} ctx
   */
  async run(ctx) {
    const { context, page } = await createAeroPage(ctx.browser, { viewport: ctx.viewport });
    await page.goto(ctx.baseUrl, { waitUntil: 'load' });
    await waitForAeroReady(page, ctx.timeoutMs);

    const runs = [];
    const total = ctx.warmupIterations + ctx.iterations;
    const targetMs = Math.max(0, ctx.idleSeconds) * 1000;

    for (let i = 0; i < total; i++) {
      const warmup = i < ctx.warmupIterations;
      await resetPerf(page);

      const rafResult = await page.evaluate(async ({ targetMs: ms }) => {
        return await new Promise((resolve) => {
          /** @type {number[]} */
          const frameTimesMs = [];
          /** @type {number | undefined} */
          let start;
          /** @type {number | undefined} */
          let last;

          function step(ts) {
            if (start === undefined) {
              start = ts;
              last = ts;
              // schedule the first "real" sample.
              requestAnimationFrame(step);
              return;
            }

            frameTimesMs.push(ts - /** @type {number} */ (last));
            last = ts;

            if (ts - /** @type {number} */ (start) < ms) requestAnimationFrame(step);
            else {
              resolve({
                durationMs: ts - /** @type {number} */ (start),
                frames: frameTimesMs.length,
                frameTimesMs
              });
            }
          }

          requestAnimationFrame(step);
        });
      }, { targetMs });

      const sorted = [...rafResult.frameTimesMs].sort((a, b) => a - b);
      const durationSec = rafResult.durationMs / 1000;
      const fps = durationSec > 0 ? rafResult.frames / durationSec : 0;

      const meanFrameTimeMs = sorted.length ? sorted.reduce((a, b) => a + b, 0) / sorted.length : NaN;

      const metrics = {
        idleDurationMs: rafResult.durationMs,
        idleFrames: rafResult.frames,
        idleFps: fps,
        frameTimeMeanMs: meanFrameTimeMs,
        frameTimeP50Ms: percentile(sorted, 0.5),
        frameTimeP90Ms: percentile(sorted, 0.9),
        frameTimeP99Ms: percentile(sorted, 0.99),
        frameTimeMaxMs: sorted.length ? sorted[sorted.length - 1] : NaN
      };

      const perfExport = await exportPerf(page);
      runs.push({
        warmup,
        index: i + 1,
        metrics,
        details: { raf: rafResult },
        perfExport
      });
    }

    await context.close();

    return { id: this.id, name: this.name, runs };
  }
};

export default scenario;
