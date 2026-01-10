import type { AeroGlobalApi } from '../../shared/aero_api.ts';
import type { WebGpuBenchOptions, WebGpuBenchResult } from './bench/webgpu_bench';
import type { ByteSizedCacheTracker, GpuAllocationTracker, MemoryTelemetry } from './perf/memory';
import type { PerfApi } from './perf/types';

export {};

declare global {
  interface Window {
    aero?: AeroGlobalApi & {
      bench?: {
        runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
      };
      perf?: PerfApi & {
        memoryTelemetry?: MemoryTelemetry;
        gpuTracker?: GpuAllocationTracker;
        jitCacheTracker?: ByteSizedCacheTracker;
        shaderCacheTracker?: ByteSizedCacheTracker;
      };
    };
  }
}
