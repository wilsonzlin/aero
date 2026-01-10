import type { WebGpuBenchOptions, WebGpuBenchResult } from "./bench/webgpu_bench";

export {};

declare global {
  interface Window {
    aero?: {
      bench?: {
        runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
      };
      perf?: {
        export: () => unknown;
        getStats?: () => unknown;
        setEnabled?: (enabled: boolean) => void;
      };
    };
  }
}
