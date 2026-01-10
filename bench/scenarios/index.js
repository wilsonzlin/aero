'use strict';

const startup = require('./startup');
const microbench = require('./microbench');
const idleRaf = require('./idle_raf');

const SCENARIOS = [startup, microbench, idleRaf];

/**
 * @param {string} id
 */
function getScenario(id) {
  return SCENARIOS.find((s) => s.id === id) ?? null;
}

module.exports = { SCENARIOS, getScenario };
