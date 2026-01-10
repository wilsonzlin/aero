import { installAeroGlobals } from "../aero";
import { runStorageBench } from "../bench/storage_bench";
import { setBenchmark } from "../bench/store";
import type { StorageBenchOpts, StorageBenchResult } from "../bench/storage_types";
import { runWebGpuBench, type WebGpuBenchOptions, type WebGpuBenchResult } from "../bench/webgpu_bench";

type AeroGlobal = NonNullable<Window["aero"]> & {
  bench?: {
    runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
    runStorageBench?: (opts?: StorageBenchOpts) => Promise<StorageBenchResult>;
  };
};

/**
 * Installs the `window.aero.bench` API for browser automation + local perf runs.
 *
 * Benchmarks are persisted into the shared in-memory benchmark store so other
 * tooling (e.g. `window.aero.perf.export()`) can surface them via the standard
 * `benchmarks.*` payload.
 *
 * Safe to call multiple times.
 */
export function installAeroGlobal(): void {
  const aero = ((window as Window).aero ??= {}) as AeroGlobal;
  aero.bench ??= {};

  aero.bench.runWebGpuBench = async (opts?: WebGpuBenchOptions): Promise<WebGpuBenchResult> => {
    const result = await runWebGpuBench(opts);
    setBenchmark("webgpu", result);
    // Ensure the perf export wrapper is installed so results are included.
    installAeroGlobals();
    return result;
  };

  aero.bench.runStorageBench = async (opts?: StorageBenchOpts): Promise<StorageBenchResult> => {
    const result = await runStorageBench(opts);
    setBenchmark("storage", result);
    installAeroGlobals();
    return result;
  };

  installAeroGlobals();
}

