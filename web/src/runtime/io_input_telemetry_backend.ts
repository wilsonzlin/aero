import type { WorkerCoordinator } from "./coordinator";
import { readIoInputTelemetry, type IoInputTelemetrySnapshot } from "./io_input_telemetry";

export type IoInputTelemetryBackend = {
  /**
   * Returns the current input telemetry counters (or null if the runtime is not initialized).
   *
   * Note: despite the `Io*` naming, these counters are written by the active input injector:
   * - `vmRuntime=legacy`: I/O worker
   * - `vmRuntime=machine`: machine CPU worker
   */
  getIoInputTelemetry: () => IoInputTelemetrySnapshot | null;
};

/**
 * Installs a small helper API under `window.aero.debug` so developers (and
 * Playwright tests that run the full harness page) can read input telemetry without needing a
 * handle to the shared status SAB directly.
 *
 * This must only run on the browser main thread (it depends on `window`).
 */
export function installIoInputTelemetryBackendOnAeroGlobal(coordinator: WorkerCoordinator): void {
  if (typeof window === "undefined") {
    throw new Error(
      "installIoInputTelemetryBackendOnAeroGlobal must be called on the browser main thread (window is undefined).",
    );
  }

  // Be defensive: other tooling might set `window.aero` to a non-object value.
  // Align with `web/src/api/status.ts` which repairs the global in that case.
  const win = window as unknown as { aero?: unknown };
  if (!win.aero || typeof win.aero !== "object") {
    win.aero = {};
  }
  const aero = win.aero as { debug?: unknown };
  const debug = (() => {
    if (aero.debug && typeof aero.debug === "object") return aero.debug as Record<string, unknown>;
    const obj: Record<string, unknown> = {};
    aero.debug = obj;
    return obj;
  })();

  const backend: IoInputTelemetryBackend = {
    getIoInputTelemetry: () => {
      const status = coordinator.getStatusView();
      if (!status) return null;
      return readIoInputTelemetry(status);
    },
  };

  // Preserve any existing debug hooks.
  Object.assign(debug, backend);
}
