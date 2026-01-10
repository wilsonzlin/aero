(function () {
  'use strict';

  function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  /** @type {{ wasm?: { compileMs: number, instantiateMs: number }, lastMicrobench?: any }} */
  const state = {};

  let ready = false;
  const whenReady = (async () => {
    // Simulate some startup work: wasm compile + instantiate.
    const compileStart = performance.now();
    await sleep(25);
    const compileMs = performance.now() - compileStart;

    const instStart = performance.now();
    await sleep(15);
    const instantiateMs = performance.now() - instStart;

    state.wasm = { compileMs, instantiateMs };
    ready = true;
  })();

  const perf = {
    reset() {
      // Intentionally preserve `state.wasm` so startup metrics remain available.
      state.lastMicrobench = undefined;
    },
    export() {
      return {
        schemaVersion: 1,
        exportedAt: performance.now(),
        wasm: state.wasm ?? null,
        lastMicrobench: state.lastMicrobench ?? null
      };
    }
  };

  async function runOneTest(iterations, body) {
    const start = performance.now();
    let acc = 0;
    for (let i = 0; i < iterations; i++) acc = body(acc, i);
    const totalMs = performance.now() - start;
    // Prevent dead-code elimination.
    if (acc === Number.MIN_SAFE_INTEGER) throw new Error('unreachable');
    return { iterations, totalMs, opsPerSec: (iterations / totalMs) * 1000 };
  }

  const bench = {
    async runMicrobenchSuite() {
      const tests = {
        arith_add: await runOneTest(1_000_000, (acc, i) => acc + i),
        arith_mul: await runOneTest(1_000_000, (acc, i) => ((acc * 1664525) ^ i) | 0),
        array_sum: await (async () => {
          const arr = new Float64Array(1024);
          for (let i = 0; i < arr.length; i++) arr[i] = i;
          return await runOneTest(1_000_000, (acc, i) => acc + arr[i & 1023]);
        })()
      };

      const result = {
        suite: 'aero-bench-fixture-v1',
        tests
      };
      state.lastMicrobench = result;
      return result;
    }
  };

  // Minimal window.aero surface for the runner.
  // - `isReady()` is intentionally boolean (pollable), while `whenReady` is a Promise.
  window.aero = {
    isReady() {
      return ready;
    },
    whenReady,
    perf,
    bench
  };
})();
