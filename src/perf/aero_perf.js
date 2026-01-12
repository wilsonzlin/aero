import { HotspotTracker } from './hotspot_tracker.js';

/**
 * Minimal performance capture surface intended to back `window.aero.perf`.
 *
 * Real implementation would aggregate additional telemetry (PF-001..004),
 * but PF-005 only requires hotspots.
 */
export class AeroPerf {
  /**
   * @param {{hotspotCapacity?: number, hotspotExportLimit?: number}} [options]
   */
  constructor(options = {}) {
    const { hotspotCapacity = 256, hotspotExportLimit = 50 } = options;
    this._hotspotExportLimit = hotspotExportLimit;
    this._hotspots = new HotspotTracker({ capacity: hotspotCapacity });
  }

  /**
   * Called by the CPU worker (or WASM core) at *block entry* granularity.
   *
   * @param {unknown} pc
   * @param {number} instructionsInBlock
   */
  recordBasicBlock(pc, instructionsInBlock) {
    this._hotspots.recordBlock(pc, instructionsInBlock);
  }

  reset() {
    this._hotspots.reset();
  }

  /**
   * Export a snapshot suitable for JSON serialization.
   * @returns {{hotspots: Array<{pc: string, hits: number, instructions: number, percent_of_total: number}>}}
   */
  export() {
    return {
      hotspots: this._hotspots.snapshot({ limit: this._hotspotExportLimit }),
    };
  }
}

/**
 * Installs an `aero.perf` surface on the provided global object (e.g. `window`).
 *
 * @param {any} globalThisLike
 * @param {{hotspotCapacity?: number, hotspotExportLimit?: number}} [options]
 */
export function installAeroPerf(globalThisLike, options = {}) {
  const g = globalThisLike;
  if (!g) throw new Error('installAeroPerf requires a global object (e.g. window)');
  // Be defensive: callers may have set `aero` to a non-object value.
  if (!g.aero || typeof g.aero !== 'object') g.aero = {};
  const perf = new AeroPerf(options);
  g.aero.perf = perf;
  return perf;
}
