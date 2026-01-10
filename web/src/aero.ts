import { runMicrobenchSuite } from "./bench/microbench";
import { getBenchmarksSnapshot } from "./bench/store";
import type { PerfApi } from "./perf/types";

export function installAeroGlobals(): void {
  window.aero = window.aero ?? {};
  const aero = window.aero as NonNullable<Window["aero"]>;
  aero.bench = { ...(aero.bench ?? {}), runMicrobenchSuite };

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
    const benchmarks = getBenchmarksSnapshot();
    const hasBenchmarks = Object.keys(benchmarks).length > 0;

    if (exported && typeof exported === "object" && !Array.isArray(exported)) {
      return {
        ...(exported as Record<string, unknown>),
        benchmarks: hasBenchmarks ? benchmarks : undefined,
      };
    }

    return {
      exported,
      benchmarks: hasBenchmarks ? benchmarks : undefined,
    };
  };
}
