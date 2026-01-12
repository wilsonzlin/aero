import { runMicrobenchSuite } from "./bench/microbench";
import { getBenchmarksSnapshot } from "./bench/store";
import type { PerfApi } from "./perf/types";

type RunMicrobenchSuite = typeof runMicrobenchSuite;

function runMicrobenchSuiteGlobal(opts?: unknown): Promise<unknown>;
function runMicrobenchSuiteGlobal(opts?: Parameters<RunMicrobenchSuite>[0]): ReturnType<RunMicrobenchSuite>;
async function runMicrobenchSuiteGlobal(opts?: unknown): Promise<unknown> {
  return await runMicrobenchSuite(opts as Parameters<RunMicrobenchSuite>[0]);
}

export function installAeroGlobals(): void {
  window.aero = window.aero ?? {};
  const aero = window.aero as NonNullable<Window["aero"]>;
  aero.bench = { ...(aero.bench ?? {}), runMicrobenchSuite: runMicrobenchSuiteGlobal };

  if (aero.perf) {
    ensureBenchmarksAttachedToPerfExport(aero.perf);
  }
}

const BENCHMARKS_WRAPPED = Symbol.for("aero.perf.export.benchmarks_wrapped");

type PerfWithBenchmarks = PerfApi & {
  [BENCHMARKS_WRAPPED]?: boolean;
};

function ensureBenchmarksAttachedToPerfExport(perf: PerfApi): void {
  const wrapped = perf as PerfWithBenchmarks;
  if (wrapped[BENCHMARKS_WRAPPED]) {
    return;
  }
  wrapped[BENCHMARKS_WRAPPED] = true;

  const baseExport = perf.export.bind(perf);
  perf.export = () => {
    const exported = baseExport();
    const fromStore = getBenchmarksSnapshot();
    const existingBenchmarks =
      exported && typeof exported === "object" && !Array.isArray(exported)
        ? (exported as Record<string, unknown>).benchmarks
        : undefined;
    const mergedBenchmarks =
      existingBenchmarks && typeof existingBenchmarks === "object" && !Array.isArray(existingBenchmarks)
        ? {
            ...(existingBenchmarks as Record<string, unknown>),
            ...fromStore,
          }
        : { ...fromStore };

    if (exported && typeof exported === "object" && !Array.isArray(exported)) {
      return {
        ...(exported as Record<string, unknown>),
        benchmarks: mergedBenchmarks,
      };
    }

    return {
      exported,
      benchmarks: mergedBenchmarks,
    };
  };
}
