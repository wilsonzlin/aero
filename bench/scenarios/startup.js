import { createAeroPage, exportPerf, extractWasmTimes, waitForAeroReady } from "../aero_page.js";

const scenario = {
  id: 'startup',
  name: 'startup',

  /**
   * @param {import('../runner.js').ScenarioRunContext} ctx
   */
  async run(ctx) {
    const runs = [];

    const total = ctx.warmupIterations + ctx.iterations;
    for (let i = 0; i < total; i++) {
      const warmup = i < ctx.warmupIterations;
      const { context, page } = await createAeroPage(ctx.browser, { viewport: ctx.viewport });

      const t0 = process.hrtime.bigint();
      await page.goto(ctx.baseUrl, { waitUntil: 'load' });
      await waitForAeroReady(page, ctx.timeoutMs);
      const t1 = process.hrtime.bigint();

      const timeToReadyMs = Number(t1 - t0) / 1e6;
      const perfExport = await exportPerf(page);
      const wasm = extractWasmTimes(perfExport);

      runs.push({
        warmup,
        index: i + 1,
        metrics: {
          timeToReadyMs,
          ...wasm
        },
        details: {},
        perfExport
      });

      await context.close();
    }

    return { id: this.id, name: this.name, runs };
  }
};

export default scenario;
