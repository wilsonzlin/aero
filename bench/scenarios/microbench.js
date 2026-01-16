import { createAeroPage, exportPerf, resetPerf, waitForAeroReady } from "../aero_page.js";
import { flattenNumericMetrics } from "../util/metrics.js";

const scenario = {
  id: 'microbench',
  name: 'microbench',

  /**
   * @param {import('../runner.js').ScenarioRunContext} ctx
   */
  async run(ctx) {
    const { context, page } = await createAeroPage(ctx.browser, { viewport: ctx.viewport });
    await page.goto(ctx.baseUrl, { waitUntil: 'load' });
    await waitForAeroReady(page, ctx.timeoutMs);

    const runs = [];
    const total = ctx.warmupIterations + ctx.iterations;

    for (let i = 0; i < total; i++) {
      const warmup = i < ctx.warmupIterations;
      await resetPerf(page);

      const t0 = process.hrtime.bigint();
      const result = await page.evaluate(async () => {
        // eslint-disable-next-line no-undef
        const fn = window.aero?.bench?.runMicrobenchSuite;
        if (typeof fn !== 'function') throw new Error('window.aero.bench.runMicrobenchSuite() is not available');
        return await fn();
      });
      const t1 = process.hrtime.bigint();
      const durationMs = Number(t1 - t0) / 1e6;

      const metrics = { microbenchDurationMs: durationMs };
      const numeric = flattenNumericMetrics(result?.tests && typeof result.tests === 'object' ? result.tests : result);
      for (const [k, v] of Object.entries(numeric)) {
        metrics[`microbench.${k}`] = v;
      }

      const perfExport = await exportPerf(page);
      runs.push({
        warmup,
        index: i + 1,
        metrics,
        details: { microbench: result },
        perfExport
      });
    }

    await context.close();

    return { id: this.id, name: this.name, runs };
  }
};

export default scenario;
