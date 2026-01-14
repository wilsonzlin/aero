import type { WorkerCoordinator } from "./coordinator";

export type BootDeviceKind = "hdd" | "cdrom";

export type BootDiskSelectionSnapshot = {
  mounts: { hddId?: string; cdId?: string };
  bootDevice?: BootDeviceKind;
};

export type MachineCpuBootConfigSnapshot = {
  bootDrive: number;
  cdBootDrive: number;
  bootFromCdIfPresent: boolean;
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
  /**
   * Returns the machine CPU worker's BIOS boot configuration (boot drive number + CD-first state),
   * or null if unknown/unavailable.
   */
  getMachineCpuBootConfig: () => MachineCpuBootConfigSnapshot | null;
};

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

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
      const msg = coordinator.getBootDisks() as unknown;
      if (!msg || typeof msg !== "object") return null;
      const rec = msg as Partial<{ mounts: unknown; bootDevice: unknown }>;
      const mountsRaw = hasOwn(rec as object, "mounts") ? rec.mounts : undefined;
      const mountsRec = mountsRaw && typeof mountsRaw === "object" ? (mountsRaw as Record<string, unknown>) : {};
      const sanitize = (value: unknown): string | undefined => {
        if (typeof value !== "string") return undefined;
        const trimmed = value.trim();
        return trimmed ? trimmed : undefined;
      };
      // Treat mount IDs as untrusted; ignore inherited values (prototype pollution).
      const hddId = hasOwn(mountsRec, "hddId") ? sanitize(mountsRec.hddId) : undefined;
      const cdId = hasOwn(mountsRec, "cdId") ? sanitize(mountsRec.cdId) : undefined;
      const mounts = { ...(hddId ? { hddId } : {}), ...(cdId ? { cdId } : {}) };

      const bootDeviceRaw = hasOwn(rec as object, "bootDevice") ? rec.bootDevice : undefined;
      const bootDevice = bootDeviceRaw === "hdd" || bootDeviceRaw === "cdrom" ? bootDeviceRaw : undefined;
      return bootDevice ? { mounts, bootDevice } : { mounts };
    },
    getMachineCpuActiveBootDevice: () => {
      const raw = coordinator.getMachineCpuActiveBootDevice() as unknown;
      return raw === "hdd" || raw === "cdrom" ? raw : null;
    },
    getMachineCpuBootConfig: () => {
      const raw = coordinator.getMachineCpuBootConfig() as unknown;
      if (!raw || typeof raw !== "object") return null;
      const rec = raw as Partial<{ bootDrive: unknown; cdBootDrive: unknown; bootFromCdIfPresent: unknown }>;
      const sanitizeDrive = (value: unknown): number | null => {
        if (typeof value !== "number" || !Number.isFinite(value) || !Number.isSafeInteger(value)) return null;
        if (value < 0 || value > 0xff) return null;
        return value;
      };
      const bootDrive = sanitizeDrive(rec.bootDrive);
      const cdBootDrive = sanitizeDrive(rec.cdBootDrive);
      const bootFromCdIfPresent = rec.bootFromCdIfPresent;
      if (bootDrive === null || cdBootDrive === null) return null;
      if (typeof bootFromCdIfPresent !== "boolean") return null;
      return { bootDrive, cdBootDrive, bootFromCdIfPresent };
    },
  };

  Object.assign(debug, backend);
}
