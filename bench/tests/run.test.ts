import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { noopMicrobenchScenario } from '../scenarios/noop_microbench.ts';
import { systemBootScenario } from '../scenarios/system_boot.ts';
import { runScenario } from '../runner/run.ts';

async function withTempDir<T>(fn: (dir: string) => Promise<T>): Promise<T> {
  const dir = await mkdtemp(join(tmpdir(), 'aero-bench-'));
  try {
    return await fn(dir);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

test('noop scenario runs and writes a report', async () => {
  await withTempDir(async (outDir) => {
    const report = await runScenario(noopMicrobenchScenario, {
      scenarioId: 'noop',
      outDir,
      trace: false,
    });

    assert.equal(report.status, 'ok');
    assert.deepEqual(report.metrics, [{ id: 'noop_ms', unit: 'ms', value: 0 }]);

    const payload = JSON.parse(await readFile(join(outDir, 'report.json'), 'utf8')) as typeof report;
    assert.equal(payload.status, 'ok');
  });
});

test('system boot scenario skips when disk image is missing', async () => {
  await withTempDir(async (outDir) => {
    const report = await runScenario(systemBootScenario, {
      scenarioId: 'system_boot',
      outDir,
      trace: false,
    });

    assert.equal(report.status, 'skipped');
    assert.match(report.skipReason ?? '', /missing disk image/);
  });
});

test('system boot scenario skips when emulator does not support OS boot', async () => {
  await withTempDir(async (outDir) => {
    const report = await runScenario(systemBootScenario, {
      scenarioId: 'system_boot',
      outDir,
      trace: false,
      diskImage: { kind: 'path', path: '/tmp/win7.img' },
    });

    assert.equal(report.status, 'skipped');
    assert.match(report.skipReason ?? '', /unsupported/);
  });
});

