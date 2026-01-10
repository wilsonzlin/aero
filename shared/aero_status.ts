export const AERO_PHASES = ['booting', 'installing', 'desktop', 'idle'] as const;

export type AeroPhase = (typeof AERO_PHASES)[number];

export interface AeroStatusSnapshot {
  phase: AeroPhase;
  phaseSinceMs: number;
}

export function isAeroPhase(value: unknown): value is AeroPhase {
  return typeof value === 'string' && (AERO_PHASES as readonly string[]).includes(value);
}

