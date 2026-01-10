import type { Scenario } from './types.ts';

export const noopMicrobenchScenario: Scenario = {
  id: 'noop',
  name: 'No-op microbenchmark',
  kind: 'micro',
  async run(ctx) {
    ctx.metrics.setMs('noop_ms', 0);
  },
};

