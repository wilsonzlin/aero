import type { PerfExport } from '../perf/aero_perf.js';

export function createHotspotsPanel(options: {
  perf: { export(): PerfExport };
  topN?: number;
  refreshMs?: number;
}): HTMLElement;

