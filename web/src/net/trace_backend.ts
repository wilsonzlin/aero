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
  const win = window as unknown as { aero?: unknown };
  if (!win.aero || typeof win.aero !== "object") {
    win.aero = {};
  }
  const aero = win.aero as Record<string, unknown>;

  const backend: NetTraceBackend = {
    isEnabled: () => coordinator.isNetTraceEnabled(),
    enable: () => coordinator.setNetTraceEnabled(true),
    disable: () => coordinator.setNetTraceEnabled(false),
    downloadPcapng: () => coordinator.takeNetTracePcapng(),
    exportPcapng: () => coordinator.exportNetTracePcapng(),
    clear: () => coordinator.clearNetTrace(),
    getStats: async () => {
      // Avoid spamming UIs (notably the repo-root harness panel) with errors when
      // the VM/net worker isn't running yet. Return stub stats until the net
      // worker is ready.
      //
      // Guard optional calls so unit tests can install a minimal fake coordinator.
      const state = (coordinator as unknown as { getVmState?: () => string }).getVmState?.();
      if (state === "stopped" || state === "poweredOff" || state === "failed") {
        return { enabled: coordinator.isNetTraceEnabled(), records: 0, bytes: 0, droppedRecords: 0, droppedBytes: 0 };
      }

      const statuses = (coordinator as unknown as { getWorkerStatuses?: () => { net?: { state?: string } } }).getWorkerStatuses?.();
      const netState = statuses?.net?.state;
      if (netState !== undefined && netState !== "ready") {
        return { enabled: coordinator.isNetTraceEnabled(), records: 0, bytes: 0, droppedRecords: 0, droppedBytes: 0 };
      }

      return await coordinator.getNetTraceStats();
    },
    // Legacy alias kept for older UIs that still call `clearCapture()`.
    clearCapture: () => coordinator.clearNetTrace(),
  };

  aero.netTrace = backend;
}
