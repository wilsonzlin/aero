import type { WorkerCoordinator } from "../runtime/coordinator";
import { PCAPNG_LINKTYPE_ETHERNET, writePcapng } from "./pcapng";
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

  // Provide a valid-but-empty capture for best-effort exports when the net
  // worker hasn't started yet (or is restarting).
  const emptyCapture = (): Uint8Array<ArrayBuffer> => {
    return writePcapng({
      interfaces: [{ linkType: PCAPNG_LINKTYPE_ETHERNET, snapLen: 0xffff, name: "guest-eth0", tsResolPower10: 9 }],
      packets: [],
    });
  };

  const isNetWorkerReady = (): boolean => {
    const state = (coordinator as unknown as { getVmState?: () => string }).getVmState?.();
    if (state === "stopped" || state === "poweredOff" || state === "failed") return false;
    const statuses = (coordinator as unknown as { getWorkerStatuses?: () => { net?: { state?: string } } }).getWorkerStatuses?.();
    const netState = statuses?.net?.state;
    if (netState !== undefined && netState !== "ready") return false;
    return true;
  };

  const backend: NetTraceBackend = {
    isEnabled: () => coordinator.isNetTraceEnabled(),
    enable: () => coordinator.setNetTraceEnabled(true),
    disable: () => coordinator.setNetTraceEnabled(false),
    downloadPcapng: async () => {
      if (!isNetWorkerReady()) return emptyCapture();
      try {
        return await coordinator.takeNetTracePcapng();
      } catch {
        return emptyCapture();
      }
    },
    exportPcapng: async () => {
      if (!isNetWorkerReady()) return emptyCapture();
      try {
        return await coordinator.exportNetTracePcapng();
      } catch {
        return emptyCapture();
      }
    },
    clear: () => coordinator.clearNetTrace(),
    getStats: async () => {
      // Avoid spamming UIs (notably the repo-root harness panel) with errors when
      // the VM/net worker isn't running yet. Return stub stats until the net
      // worker is ready.
      if (!isNetWorkerReady()) {
        return { enabled: coordinator.isNetTraceEnabled(), records: 0, bytes: 0, droppedRecords: 0, droppedBytes: 0 };
      }
      try {
        return await coordinator.getNetTraceStats();
      } catch {
        return { enabled: coordinator.isNetTraceEnabled(), records: 0, bytes: 0, droppedRecords: 0, droppedBytes: 0 };
      }
    },
    // Legacy alias kept for older UIs that still call `clearCapture()`.
    clearCapture: () => coordinator.clearNetTrace(),
  };

  aero.netTrace = backend;
}
