import { METRIC_BOOT_TIME_MS, ScenarioSkippedError } from './types.ts';
import type { Scenario } from './types.ts';

export const systemBootScenario: Scenario = {
  id: 'system_boot',
  name: 'System boot → desktop (macrobench)',
  kind: 'macro',
  requirements: {
    diskImage: 'required',
    webgpu: true,
    opfs: true,
    crossOriginIsolated: true,
  },
  async setup(ctx) {
    const source = ctx.config.diskImage;
    if (!source) {
      throw new ScenarioSkippedError(
        'missing disk image (provide --disk-image or set AERO_DISK_IMAGE_PATH)',
      );
    }

    if (!ctx.emulator.capabilities.systemBoot) {
      throw new ScenarioSkippedError('unsupported: emulator cannot boot OS images yet');
    }

    await ctx.emulator.attachDiskImage(source);
  },
  async run(ctx) {
    const start = performance.now();

    await ctx.emulator.start();

    if (ctx.emulator.capabilities.statusApi) {
      await ctx.milestones.waitForEvent('desktop_ready', { timeoutMs: 10 * 60_000 });
    } else {
      await ctx.milestones.waitForPhase('desktop', { timeoutMs: 10 * 60_000 });
    }

    ctx.metrics.setMs(METRIC_BOOT_TIME_MS, performance.now() - start);
    await ctx.milestones.captureScreenshot('desktop_ready');
  },
  async teardown(ctx) {
    await ctx.emulator.stop();
  },
};

