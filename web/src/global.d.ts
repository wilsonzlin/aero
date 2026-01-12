import type { AeroGlobalApi } from '../../shared/aero_api.ts';
import type { MicrobenchSuiteOptions, MicrobenchSuiteResultV1 } from './bench/types';
import type { StorageBenchOpts, StorageBenchResult } from './bench/storage_types';
import type { WebGpuBenchOptions, WebGpuBenchResult } from './bench/webgpu_bench';
import type { NetTraceBackend } from './net/trace_ui.ts';
import type { ByteSizedCacheTracker, GpuAllocationTracker, MemoryTelemetry } from './perf/memory';
import type { PerfApi } from './perf/types';

export {};

declare global {
  const __AERO_BUILD_INFO__: Readonly<{
    version: string;
    gitSha: string;
    builtAt: string;
  }>;

  interface Window {
    aero?: AeroGlobalApi & {
      netTrace?: NetTraceBackend;
      bench?: {
        runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
        runMicrobenchSuite?: (opts?: MicrobenchSuiteOptions) => Promise<MicrobenchSuiteResultV1>;
        runStorageBench?: (opts?: StorageBenchOpts) => Promise<StorageBenchResult>;
      };
      perf?: PerfApi & {
        memoryTelemetry?: MemoryTelemetry;
        gpuTracker?: GpuAllocationTracker;
        jitCacheTracker?: ByteSizedCacheTracker;
        shaderCacheTracker?: ByteSizedCacheTracker;
        traceStart?: () => void;
        traceStop?: () => void;
        exportTrace?: (opts?: { asString?: boolean }) => Promise<unknown>;
        traceEnabled?: boolean;
      };
    };
  }
}
