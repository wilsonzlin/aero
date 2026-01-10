export interface HotspotEntry {
  pc: string;
  hits: number;
  instructions: number;
  percent_of_total: number;
}

export interface PerfExport {
  hotspots: HotspotEntry[];
}

export class AeroPerf {
  constructor(options?: { hotspotCapacity?: number; hotspotExportLimit?: number });
  recordBasicBlock(pc: unknown, instructionsInBlock: number): void;
  reset(): void;
  export(): PerfExport;
}

export function installAeroPerf(
  globalThisLike: any,
  options?: { hotspotCapacity?: number; hotspotExportLimit?: number },
): AeroPerf;

