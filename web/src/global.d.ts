import type { AeroGlobalApi } from '../../shared/aero_api.ts';
import type { MicrobenchSuiteOptions, MicrobenchSuiteResultV1 } from './bench/types';
import type { WebGpuBenchOptions, WebGpuBenchResult } from './bench/webgpu_bench';
import type { ByteSizedCacheTracker, GpuAllocationTracker, MemoryTelemetry } from './perf/memory';
import type { PerfApi } from './perf/types';

export {};

declare global {
  interface Window {
    aero?: AeroGlobalApi & {
      bench?: {
        runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
        runMicrobenchSuite?: (opts?: MicrobenchSuiteOptions) => Promise<MicrobenchSuiteResultV1>;
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
