import startup from "./startup.js";
import microbench from "./microbench.js";
import idleRaf from "./idle_raf.js";

export const SCENARIOS = [startup, microbench, idleRaf];

/**
 * @param {string} id
 */
export function getScenario(id) {
  return SCENARIOS.find((s) => s.id === id) ?? null;
}
