import type { AeroGlobalApi } from '../../shared/aero_api.ts';
import type { GuestCpuBenchOpts, GuestCpuBenchRun } from './bench/guest_cpu_types';
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
    aero?: Omit<AeroGlobalApi, "bench"> & {
      netTrace?: NetTraceBackend;
      bench?: {
        runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
        runGuestCpuBench?: (opts: GuestCpuBenchOpts) => Promise<GuestCpuBenchRun>;
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

  /**
   * Global shims used by the wasm32 browser runtime (main thread and workers).
   *
   * Note: WebAssembly `i64` values are represented as JS `bigint` in the JS WebAssembly API.
   * Any shim that returns/accepts a wasm `i64` must use `bigint` (not `number`).
   */
  interface WindowOrWorkerGlobalScope {
    /**
     * Tier-1 JIT dispatch hook used by `crates/aero-wasm`'s tiered VM.
     *
     * Wasm signature: `__aero_jit_call(table_index: i32, cpu_ptr: i32, jit_ctx_ptr: i32) -> i64`.
     */
    __aero_jit_call?: (tableIndex: number, cpuPtr: number, jitCtxPtr: number) => bigint;

    /**
     * Port I/O shims for the minimal VM loop (`crates/aero-wasm/src/vm.rs`).
     */
    __aero_io_port_read?: (port: number, size: number) => number;
    __aero_io_port_write?: (port: number, size: number, value: number) => void;

    /**
     * MMIO shims used by the minimal VM loop and/or device models.
     *
     * Note: `addr` is wasm `u64`, represented as JS `bigint`.
     */
    __aero_mmio_read?: (addr: bigint, size: number) => number;
    __aero_mmio_write?: (addr: bigint, size: number, value: number) => void;

    /**
     * Optional Tier-1 JIT commit-status flag.
     *
     * Newer JIT hosts report commit status via the in-memory `commit_flag` slot. Older/alternate
     * hosts may instead set this global to `false` to indicate the last Tier-1 block did not
     * commit (e.g. it was stubbed or rolled back).
     */
    __aero_jit_last_committed?: boolean;
  }
}
