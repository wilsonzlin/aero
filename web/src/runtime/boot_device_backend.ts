import type { WorkerCoordinator } from "./coordinator";

export type BootDeviceKind = "hdd" | "cdrom";

export type BootDiskSelectionSnapshot = {
  mounts: { hddId?: string; cdId?: string };
  bootDevice?: BootDeviceKind;
};

export type BootDeviceBackend = {
  /**
   * Returns the current boot disk selection policy (mount IDs + requested boot device), or null if
   * no selection is present.
   *
   * This intentionally does not include full disk metadata; it is meant as a stable, automation-
   * friendly snapshot of the selection state.
   */
  getBootDisks: () => BootDiskSelectionSnapshot | null;
  /**
   * Returns the active boot device reported by the machine CPU worker (what firmware actually
   * booted from for the current session), or null if unknown/unavailable.
   */
  getMachineCpuActiveBootDevice: () => BootDeviceKind | null;
};

/**
 * Installs a small helper API under `window.aero.debug` so automation harnesses can inspect the
 * machine runtime's boot-device state (selected policy vs active boot source).
 *
 * This must only run on the browser main thread (it depends on `window`).
 */
export function installBootDeviceBackendOnAeroGlobal(coordinator: WorkerCoordinator): void {
  if (typeof window === "undefined") {
    throw new Error("installBootDeviceBackendOnAeroGlobal must be called on the browser main thread (window is undefined).");
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

  const backend: BootDeviceBackend = {
    getBootDisks: () => {
      const msg = coordinator.getBootDisks();
      if (!msg) return null;
      const mounts = msg.mounts ?? {};
      const bootDevice = msg.bootDevice;
      return bootDevice ? { mounts, bootDevice } : { mounts };
    },
    getMachineCpuActiveBootDevice: () => coordinator.getMachineCpuActiveBootDevice(),
  };

  Object.assign(debug, backend);
}

