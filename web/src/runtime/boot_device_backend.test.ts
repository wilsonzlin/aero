import { describe, expect, it } from "vitest";

import { installBootDeviceBackendOnAeroGlobal } from "./boot_device_backend";

describe("runtime/boot_device_backend", () => {
  it("installs boot device helpers under window.aero.debug", () => {
    const hadWindow = Object.prototype.hasOwnProperty.call(globalThis, "window");
    const prevWindow = (globalThis as unknown as { window?: unknown }).window;

    try {
      // Minimal window shim.
      const win: any = { aero: { debug: { existing: true } } };
      (globalThis as any).window = win;

      const coordinator: any = {
        getBootDisks: () => ({ type: "setBootDisks", mounts: { hddId: "hdd0", cdId: "cd0" }, hdd: null, cd: null, bootDevice: "hdd" }),
        getMachineCpuActiveBootDevice: () => "cdrom",
      };

      installBootDeviceBackendOnAeroGlobal(coordinator);

      expect(win.aero.debug.existing).toBe(true);
      expect(typeof win.aero.debug.getBootDisks).toBe("function");
      expect(typeof win.aero.debug.getMachineCpuActiveBootDevice).toBe("function");

      expect(win.aero.debug.getBootDisks()).toEqual({ mounts: { hddId: "hdd0", cdId: "cd0" }, bootDevice: "hdd" });
      expect(win.aero.debug.getMachineCpuActiveBootDevice()).toBe("cdrom");
    } finally {
      if (hadWindow) {
        (globalThis as any).window = prevWindow;
      } else {
        // Ensure we restore the pre-test global shape for other unit tests.
        delete (globalThis as any).window;
      }
    }
  });
});
