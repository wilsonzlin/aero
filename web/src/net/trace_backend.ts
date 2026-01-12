import type { WorkerCoordinator } from "../runtime/coordinator";
import type { NetTraceBackend } from "./trace_ui";

/**
 * Install a `NetTraceBackend` implementation on `window.aero.netTrace` so the
 * repo-root harness UI (`src/main.ts`) can drive network tracing.
 *
 * This must only run on the browser main thread (it depends on `window`).
 */
export function installNetTraceBackendOnAeroGlobal(coordinator: WorkerCoordinator): void {
  if (typeof window === "undefined") {
    throw new Error("installNetTraceBackendOnAeroGlobal must be called on the browser main thread (window is undefined).");
  }

  // Be defensive: other tooling might set `window.aero` to a non-object value.
  // Align with `web/src/api/status.ts` which repairs the global in that case.
  const existing = (window as unknown as { aero?: unknown }).aero;
  const aero: Record<string, unknown> =
    existing && typeof existing === "object" ? (existing as Record<string, unknown>) : ((window as unknown as any).aero = {});

  const backend: NetTraceBackend = {
    isEnabled: () => coordinator.isNetTraceEnabled(),
    enable: () => coordinator.setNetTraceEnabled(true),
    disable: () => coordinator.setNetTraceEnabled(false),
    downloadPcapng: () => coordinator.takeNetTracePcapng(),
    clear: () => coordinator.clearNetTrace(),
    getStats: () => coordinator.getNetTraceStats(),
    // Legacy alias kept for older UIs that still call `clearCapture()`.
    clearCapture: () => coordinator.clearNetTrace(),
  };

  aero.netTrace = backend;
}
