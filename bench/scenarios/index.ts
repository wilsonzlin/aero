import type { Scenario } from './types.ts';
import { guestCpuScenario } from './guest_cpu.ts';
import { noopMicrobenchScenario } from './noop_microbench.ts';
import { storageIoScenario } from './storage_io.ts';
import { systemBootScenario } from './system_boot.ts';

export const scenarios: readonly Scenario[] = [
  noopMicrobenchScenario,
  storageIoScenario,
  guestCpuScenario,
  systemBootScenario,
];

export function getScenarioById(id: string): Scenario | undefined {
  return scenarios.find((scenario) => scenario.id === id);
}
