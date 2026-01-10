import { runWebGpuBench, type WebGpuBenchOptions, type WebGpuBenchResult } from "../bench/webgpu_bench";
import { runStorageBench } from "../bench/storage_bench";
import type { StorageBenchOpts, StorageBenchResult } from "../bench/storage_types";

type AeroGlobal = NonNullable<Window["aero"]> & {
  bench?: {
    runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
    runStorageBench?: (opts?: StorageBenchOpts) => Promise<StorageBenchResult>;
  };
};

let lastWebGpuBench: WebGpuBenchResult | undefined;
let lastStorageBench: StorageBenchResult | undefined;

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function wrapPerfExport(aero: AeroGlobal): void {
  const perf = aero.perf as unknown;
  if (!isRecord(perf)) return;

  const perfAny = perf as Record<string, unknown> & {
    export?: () => unknown;
    __aeroBenchWrapped?: boolean;
    __aeroBenchOriginalExport?: () => unknown;
  };

  if (perfAny.__aeroBenchWrapped) return;
  if (typeof perfAny.export !== "function") return;

  perfAny.__aeroBenchWrapped = true;
  perfAny.__aeroBenchOriginalExport = perfAny.export.bind(perf);

  perfAny.export = () => {
    const base = perfAny.__aeroBenchOriginalExport?.();

    const webgpu = lastWebGpuBench ?? null;
    const storage = lastStorageBench ?? null;

    if (isRecord(base)) {
      const existing = isRecord(base.benchmarks) ? (base.benchmarks as Record<string, unknown>) : {};
      return {
        ...base,
        benchmarks: {
          ...existing,
          webgpu,
          storage,
        },
      };
    }

    return {
      capture: base ?? null,
      benchmarks: { webgpu, storage },
    };
  };
}

/**
 * Installs the `window.aero.bench` API for browser automation + local perf runs.
 *
 * This is intentionally lightweight and safe to call multiple times.
 */
export function installAeroGlobal(): void {
  const aero = ((window as Window).aero ??= {}) as AeroGlobal;
  aero.bench ??= {};

  aero.bench.runWebGpuBench = async (opts?: WebGpuBenchOptions): Promise<WebGpuBenchResult> => {
    const result = await runWebGpuBench(opts);
    lastWebGpuBench = result;
    wrapPerfExport(aero);
    return result;
  };

  aero.bench.runStorageBench = async (opts?: StorageBenchOpts): Promise<StorageBenchResult> => {
    const result = await runStorageBench(opts);
    lastStorageBench = result;
    wrapPerfExport(aero);
    return result;
  };

  wrapPerfExport(aero);
}
