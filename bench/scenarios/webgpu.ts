import type { Page } from '@playwright/test';

import type { WebGpuBenchOptions, WebGpuBenchResult } from '../../web/src/bench/webgpu_bench';

export type WebGpuScenarioRunOptions = {
  /**
   * Base URL where the Aero web app is served.
   *
   * By default we use the dev server from `playwright.config.ts` (5173) and
   * load the standalone `web/` entrypoint.
   */
  baseUrl?: string;

  benchOptions?: WebGpuBenchOptions;
};

export type WebGpuScenarioOutput = {
  scenario: 'webgpu';
  bench: WebGpuBenchResult;
  perfExport: unknown;
};

export const DEFAULT_WEBGPU_BENCH_OPTIONS: Required<WebGpuBenchOptions> = {
  frames: 60,
  warmupFrames: 5,
  width: 256,
  height: 256,
  drawCallsPerFrame: 200,
  pipelineSwitchesPerFrame: 50,
  compute: false,
  computeWorkgroups: 256,
};

export async function runWebGpuScenario(page: Page, opts: WebGpuScenarioRunOptions = {}): Promise<WebGpuScenarioOutput> {
  const baseUrl = opts.baseUrl ?? 'http://127.0.0.1:5173';
  const url = `${baseUrl.replace(/\\/$/, '')}/web/`;
  await page.goto(url, { waitUntil: 'load' });

  await page.waitForFunction(() => (window as any).aero?.bench?.runWebGpuBench);

  const benchOptions = {
    ...DEFAULT_WEBGPU_BENCH_OPTIONS,
    ...(opts.benchOptions ?? {}),
  };

  const bench: WebGpuBenchResult = await page.evaluate(async (optionsArg) => {
    return await (window as any).aero.bench.runWebGpuBench(optionsArg);
  }, benchOptions);

  const perfExport = await page.evaluate(() => (window as any).aero?.perf?.export?.());

  return {
    scenario: 'webgpu',
    bench,
    perfExport,
  };
}

