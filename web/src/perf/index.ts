import type { PerfApi } from './types';
import type { ByteSizedCacheTracker, GpuAllocationTracker } from './memory';

import { installFallbackPerf } from './fallback';
import { installHud } from './hud';

export type InstallPerfHudOptions = {
  guestRamBytes?: number;
  wasmMemory?: WebAssembly.Memory;
  wasmMemoryMaxPages?: number;
  gpuTracker?: GpuAllocationTracker;
  jitCacheTracker?: ByteSizedCacheTracker;
  shaderCacheTracker?: ByteSizedCacheTracker;
};

const isPerfApi = (value: unknown): value is PerfApi => {
  if (!value || typeof value !== 'object') return false;
  const maybe = value as { getHudSnapshot?: unknown; export?: unknown };
  return typeof maybe.getHudSnapshot === 'function' && typeof maybe.export === 'function';
};

export const installPerfHud = (options: InstallPerfHudOptions = {}) => {
  const aero = (window.aero ??= {});

  if (!isPerfApi(aero.perf)) {
    aero.perf = installFallbackPerf({
      guestRamBytes: options.guestRamBytes,
      wasmMemory: options.wasmMemory,
      wasmMemoryMaxPages: options.wasmMemoryMaxPages,
      gpuTracker: options.gpuTracker,
      jitCacheTracker: options.jitCacheTracker,
      shaderCacheTracker: options.shaderCacheTracker,
    });
  }

  const perf = aero.perf as PerfApi;
  return installHud(perf);
};

export type { PerfApi, PerfHudSnapshot } from './types';
