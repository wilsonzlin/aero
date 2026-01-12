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
  // Be defensive: other tooling might set `window.aero` to a non-object value.
  // Align with `web/src/api/status.ts` / net-trace backend installers.
  const win = window as unknown as { aero?: unknown };
  if (!win.aero || typeof win.aero !== "object") {
    win.aero = {};
  }
  const aero = win.aero as AeroGlobal;
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

  function runGuestCpuBenchGlobal(opts?: unknown): Promise<unknown>;
  function runGuestCpuBenchGlobal(opts: GuestCpuBenchOpts): Promise<GuestCpuBenchRun>;
  async function runGuestCpuBenchGlobal(opts?: unknown): Promise<unknown> {
    if (!opts || typeof opts !== "object") {
      throw new Error('Guest CPU benchmark: options object is required (expected "variant" and "mode").');
    }
    const result = await runGuestCpuBench(opts as GuestCpuBenchOpts);
    const typed = result as GuestCpuBenchRun;
    const exported: GuestCpuBenchPerfExport = {
      iters_per_run: typed.iters_per_run,
      warmup_runs: typed.warmup_runs,
      measured_runs: typed.measured_runs,
      results: [
        {
          variant: typed.variant,
          mode: typed.mode,
          mips_mean: typed.mips_mean,
          mips_stddev: typed.mips_stddev,
          mips_min: typed.mips_min,
          mips_max: typed.mips_max,
          expected_checksum: typed.expected_checksum,
          observed_checksum: typed.observed_checksum,
        },
      ],
    };
    setBenchmark("guest_cpu", exported);
    installAeroGlobals();
    return result;
  }
  aero.bench.runGuestCpuBench = runGuestCpuBenchGlobal;

  installAeroGlobals();
}
