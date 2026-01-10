import { SpaceSavingTopK } from './space_saving_topk.js';

/**
 * @param {unknown} pc
 * @returns {string}
 */
export function formatPc(pc) {
  if (typeof pc === 'bigint') return `0x${pc.toString(16)}`;
  if (typeof pc === 'number') return `0x${pc.toString(16)}`;
  return String(pc);
}

/**
 * Tracks approximate per-PC "hotness" using Space-Saving.
 *
 * This is designed to be called at *basic-block granularity* (interpreter/JIT
 * block entry), not every instruction. Each call attributes `instructionsInBlock`
 * instructions to the block starting at `pc`.
 */
export class HotspotTracker {
  /**
   * @param {{
   *   capacity?: number,
   *   onHotspotEnter?: (event: { pc: unknown, replacedPc: unknown | undefined }) => void
   * }} [options]
   */
  constructor(options = {}) {
    const { capacity = 256, onHotspotEnter } = options;
    this._instructionsTopK = new SpaceSavingTopK(capacity);
    /** @type {Map<unknown, number>} */
    this._hitsByPc = new Map();
    this._totalInstructions = 0;
    this._onHotspotEnter = onHotspotEnter ?? null;
  }

  /** @returns {number} */
  get totalInstructions() {
    return this._totalInstructions;
  }

  /**
   * @param {unknown} pc
   * @param {number} instructionsInBlock
   */
  recordBlock(pc, instructionsInBlock) {
    if (instructionsInBlock <= 0) return;

    this._totalInstructions += instructionsInBlock;

    const { event, replacedKey } = this._instructionsTopK.observe(pc, instructionsInBlock);

    if (event === 'replace') {
      if (replacedKey !== undefined) this._hitsByPc.delete(replacedKey);
      this._hitsByPc.set(pc, 1);
      this._onHotspotEnter?.({ pc, replacedPc: replacedKey });
      return;
    }

    if (event === 'insert') {
      this._hitsByPc.set(pc, 1);
      return;
    }

    // increment
    this._hitsByPc.set(pc, (this._hitsByPc.get(pc) ?? 0) + 1);
  }

  reset() {
    this._instructionsTopK.clear();
    this._hitsByPc.clear();
    this._totalInstructions = 0;
  }

  /**
   * @param {{limit?: number}} [options]
   * @returns {Array<{pc: string, hits: number, instructions: number, percent_of_total: number}>}
   */
  snapshot(options = {}) {
    const { limit = 50 } = options;
    const total = this._totalInstructions;
    return this._instructionsTopK.snapshot({ limit }).map((entry) => {
      const hits = this._hitsByPc.get(entry.key) ?? 0;
      const percent = total > 0 ? (entry.count / total) * 100 : 0;
      return {
        pc: formatPc(entry.key),
        hits,
        instructions: entry.count,
        percent_of_total: percent,
      };
    });
  }
}

