export type HotspotEntry = {
  pc: string;
  hits: number;
  instructions: number;
  percent_of_total: number;
};

export function formatPc(pc: unknown): string;

export class HotspotTracker {
  constructor(options?: { capacity?: number; onHotspotEnter?: (event: { pc: unknown; replacedPc: unknown | undefined }) => void });
  readonly totalInstructions: number;
  recordBlock(pc: unknown, instructionsInBlock: number): void;
  reset(): void;
  snapshot(options?: { limit?: number }): HotspotEntry[];
}

