import { installAeroGlobals } from "../aero";
import { runGuestCpuBench } from "../bench/guest_cpu_bench";
import type { GuestCpuBenchOpts, GuestCpuBenchPerfExport, GuestCpuBenchRun } from "../bench/guest_cpu_types";
import { runStorageBench } from "../bench/storage_bench";
import { setBenchmark } from "../bench/store";
import type { StorageBenchOpts, StorageBenchResult } from "../bench/storage_types";
import { runWebGpuBench, type WebGpuBenchOptions, type WebGpuBenchResult } from "../bench/webgpu_bench";

type AeroGlobal = NonNullable<Window["aero"]> & {
  bench?: {
    runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
    runStorageBench?: (opts?: StorageBenchOpts) => Promise<StorageBenchResult>;
    runGuestCpuBench?: (opts: GuestCpuBenchOpts) => Promise<GuestCpuBenchRun>;
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

  function runStorageBenchGlobal(opts?: unknown): Promise<unknown>;
  function runStorageBenchGlobal(opts?: StorageBenchOpts): Promise<StorageBenchResult>;
  async function runStorageBenchGlobal(opts?: unknown): Promise<unknown> {
    const result = await runStorageBench(opts as StorageBenchOpts | undefined);
    setBenchmark("storage", result);
    installAeroGlobals();
    return result;
  }
  aero.bench.runStorageBench = runStorageBenchGlobal;

  aero.bench.runGuestCpuBench = async (opts: GuestCpuBenchOpts): Promise<GuestCpuBenchRun> => {
    const result = await runGuestCpuBench(opts);
    const exported: GuestCpuBenchPerfExport = {
      iters_per_run: result.iters_per_run,
      warmup_runs: result.warmup_runs,
      measured_runs: result.measured_runs,
      results: [
        {
          variant: result.variant,
          mode: result.mode,
          mips_mean: result.mips_mean,
          mips_stddev: result.mips_stddev,
          mips_min: result.mips_min,
          mips_max: result.mips_max,
          expected_checksum: result.expected_checksum,
          observed_checksum: result.observed_checksum,
        },
      ],
    };
    setBenchmark("guest_cpu", exported);
    installAeroGlobals();
    return result;
  };

  installAeroGlobals();
}
