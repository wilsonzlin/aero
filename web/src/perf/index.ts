import type { PerfApi } from "./types";
import type { ByteSizedCacheTracker, GpuAllocationTracker } from "./memory";

import { installFallbackPerf } from "./fallback";
import { installHud } from "./hud";
import { PerfSession } from "./session";

export type InstallPerfHudOptions = {
  guestRamBytes?: number;
  wasmMemory?: WebAssembly.Memory;
  wasmMemoryMaxPages?: number;
  gpuTracker?: GpuAllocationTracker;
  jitCacheTracker?: ByteSizedCacheTracker;
  shaderCacheTracker?: ByteSizedCacheTracker;
};

const isPerfApi = (value: unknown): value is PerfApi => {
  if (!value || typeof value !== "object") return false;
  const maybe = value as { getHudSnapshot?: unknown; export?: unknown; getChannel?: unknown };
  return (
    typeof maybe.getHudSnapshot === "function" && typeof maybe.export === "function" && typeof maybe.getChannel === "function"
  );
};

export const installPerfHud = (options: InstallPerfHudOptions = {}) => {
  // Be defensive: other tooling might set `window.aero` to a non-object value.
  // Align with `web/src/api/status.ts` / net-trace backend installers.
  const win = window as unknown as { aero?: unknown };
  if (!win.aero || typeof win.aero !== "object") {
    win.aero = {};
  }
  const aero = win.aero as NonNullable<Window["aero"]>;

  if (!isPerfApi(aero.perf)) {
    if (typeof SharedArrayBuffer === "undefined") {
      aero.perf = installFallbackPerf({
        guestRamBytes: options.guestRamBytes,
        wasmMemory: options.wasmMemory,
        wasmMemoryMaxPages: options.wasmMemoryMaxPages,
        gpuTracker: options.gpuTracker,
        jitCacheTracker: options.jitCacheTracker,
        shaderCacheTracker: options.shaderCacheTracker,
      });
    } else {
      aero.perf = new PerfSession({
        guestRamBytes: options.guestRamBytes,
        wasmMemory: options.wasmMemory,
        wasmMemoryMaxPages: options.wasmMemoryMaxPages,
        gpuTracker: options.gpuTracker,
        jitCacheTracker: options.jitCacheTracker,
        shaderCacheTracker: options.shaderCacheTracker,
      });
    }
  }

  const perf = aero.perf as PerfApi;
  return installHud(perf);
};

export type { PerfApi, PerfHudSnapshot } from "./types";
export type { PerfExport } from "./export";
export type { ResponsivenessExport, ResponsivenessHudSnapshot } from "./responsiveness";
export { ResponsivenessTracker } from "./responsiveness";

export { PerfAggregator } from "./aggregator.js";
export { createPerfChannel, nowEpochMs } from "./shared.js";
export { WorkerKind } from "./record.js";
export { PerfWriter } from "./writer.js";
