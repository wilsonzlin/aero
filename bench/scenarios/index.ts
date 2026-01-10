import type { Scenario } from './types.ts';
import { noopMicrobenchScenario } from './noop_microbench.ts';
import { systemBootScenario } from './system_boot.ts';

export const scenarios: readonly Scenario[] = [noopMicrobenchScenario, systemBootScenario];

export function getScenarioById(id: string): Scenario | undefined {
  return scenarios.find((scenario) => scenario.id === id);
}

