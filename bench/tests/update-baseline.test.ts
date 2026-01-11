import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';

import { buildUpdateBaselinePlan, parseUpdateBaselineArgs } from '../update-baseline';

test('bench/update-baseline parses args and selects the Node microbench runner', () => {
  const parsed = parseUpdateBaselineArgs(['--scenario', 'microbench', '--iterations', '15']);
  assert.equal(parsed.scenario, 'microbench');
  assert.equal(parsed.iterations, 15);

  const plan = buildUpdateBaselinePlan(parsed, { outFile: '/tmp/results.json' });
  assert.equal(plan.runner.kind, 'node-microbench');
  assert.equal(path.basename(plan.runner.args[0]), 'run.js');

  const outIndex = plan.runner.args.indexOf('--out');
  assert.notEqual(outIndex, -1);
  assert.equal(plan.runner.args[outIndex + 1], '/tmp/results.json');

  const scenarioIndex = plan.runner.args.indexOf('--scenario');
  assert.notEqual(scenarioIndex, -1);
  assert.equal(plan.runner.args[scenarioIndex + 1], 'microbench');

  const iterationsIndex = plan.runner.args.indexOf('--iterations');
  assert.notEqual(iterationsIndex, -1);
  assert.equal(plan.runner.args[iterationsIndex + 1], '15');
});

test('bench/update-baseline defaults to updating all scenarios', () => {
  const parsed = parseUpdateBaselineArgs([]);
  assert.equal(parsed.scenario, 'all');

  const plan = buildUpdateBaselinePlan(parsed, { outFile: '/tmp/results.json' });
  assert.equal(plan.runner.args.includes('--scenario'), false);
});

