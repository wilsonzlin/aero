import { mkdir } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';

import { FileArtifactWriter } from './artifacts.ts';
import { DefaultMilestoneClient } from './milestones.ts';
import { checkScenarioRequirements } from './requirements.ts';
import { NullEmulatorDriver } from './null_emulator.ts';
import { formatOneLineError } from '../../src/text.js';
import {
  MetricsRecorder,
  ScenarioSkippedError,
  type EmulatorDriver,
  type HostCapabilities,
  type RunnerConfig,
  type Scenario,
  type ScenarioReport,
} from '../scenarios/types.ts';

export interface RunOptions {
  emulator?: EmulatorDriver;
  host?: HostCapabilities;
  log?: (message: string) => void;
}

const DEFAULT_HOST: HostCapabilities = {
  webgpu: true,
  opfs: true,
  crossOriginIsolated: true,
};

export async function runScenario(scenario: Scenario, config: RunnerConfig, options?: RunOptions): Promise<ScenarioReport> {
  const startedAtMs = Date.now();
  const runId = randomUUID();

  await mkdir(config.outDir, { recursive: true });

  const log = options?.log ?? ((message: string) => console.log(message));
  const host = options?.host ?? DEFAULT_HOST;
  const emulator = options?.emulator ?? new NullEmulatorDriver();
  const artifacts = new FileArtifactWriter(config.outDir);
  const milestones = new DefaultMilestoneClient(emulator, artifacts);
  const metrics = new MetricsRecorder();

  const ctx = {
    runId,
    config,
    host,
    emulator,
    artifacts,
    metrics,
    milestones,
    log,
  };

  let status: ScenarioReport['status'] = 'ok';
  let skipReason: string | undefined;
  let error: ScenarioReport['error'] | undefined;

  const preflightReason = checkScenarioRequirements(scenario, config, host);
  if (preflightReason) {
    status = 'skipped';
    skipReason = preflightReason;
  } else {
    let traceStarted = false;
    let setupCompleted = false;

    try {
      await scenario.setup?.(ctx);
      setupCompleted = true;

      if (config.trace && emulator.capabilities.trace && emulator.startTrace) {
        await emulator.startTrace();
        traceStarted = true;
      }

      await scenario.run(ctx);
      await scenario.collect?.(ctx);

      if (emulator.capabilities.perfExport && emulator.exportPerf) {
        await artifacts.writeJson('perf_export.json', await emulator.exportPerf(), 'perf_export');
      }
    } catch (caught) {
      if (caught instanceof ScenarioSkippedError) {
        status = 'skipped';
        skipReason = caught.reason;
      } else if (caught instanceof Error) {
        status = 'error';
        error = { message: caught.message, stack: caught.stack };
      } else {
        status = 'error';
        error = { message: formatOneLineError(caught, 512) };
      }
    } finally {
      if (traceStarted && emulator.stopTrace) {
        try {
          await artifacts.writeBinary('trace.bin', await emulator.stopTrace(), 'trace');
        } catch (caught) {
          const msg = formatOneLineError(caught, 512);
          log(`Failed to save trace: ${msg}`);
        }
      }

      try {
        if (setupCompleted) {
          await scenario.teardown?.(ctx);
        }
      } catch (caught) {
        const msg = formatOneLineError(caught, 512);
        log(`Scenario teardown failed: ${msg}`);
      }
    }
  }

  const finishedAtMs = Date.now();

  const report: ScenarioReport = {
    runId,
    scenarioId: scenario.id,
    scenarioName: scenario.name,
    kind: scenario.kind,
    status,
    startedAtMs,
    finishedAtMs,
    metrics: metrics.snapshot(),
    artifacts: artifacts.manifest(),
    skipReason,
    error,
  };

  await artifacts.writeJson('report.json', report, 'report');
  return report;
}
