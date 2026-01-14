import { describe, expect, it } from "vitest";

import { installBootDeviceBackendOnAeroGlobal } from "./boot_device_backend";

describe("runtime/boot_device_backend", () => {
  it("installs boot device helpers under window.aero.debug", () => {
    const originalWindowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");

    try {
      // Minimal window shim.
      const win: any = { aero: { debug: { existing: true } } };
      Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });

      const coordinator: any = {
        getBootDisks: () => ({ type: "setBootDisks", mounts: { hddId: "hdd0", cdId: "cd0" }, hdd: null, cd: null, bootDevice: "hdd" }),
        getMachineCpuActiveBootDevice: () => "cdrom",
        getMachineCpuBootConfig: () => ({ bootDrive: 0x80, cdBootDrive: 0xe0, bootFromCdIfPresent: true }),
      };

      installBootDeviceBackendOnAeroGlobal(coordinator);

      expect(win.aero.debug.existing).toBe(true);
      expect(typeof win.aero.debug.getBootDisks).toBe("function");
      expect(typeof win.aero.debug.getMachineCpuActiveBootDevice).toBe("function");
      expect(typeof win.aero.debug.getMachineCpuBootConfig).toBe("function");

      expect(win.aero.debug.getBootDisks()).toEqual({ mounts: { hddId: "hdd0", cdId: "cd0" }, bootDevice: "hdd" });
      expect(win.aero.debug.getMachineCpuActiveBootDevice()).toBe("cdrom");
      expect(win.aero.debug.getMachineCpuBootConfig()).toEqual({ bootDrive: 0x80, cdBootDrive: 0xe0, bootFromCdIfPresent: true });
    } finally {
      if (originalWindowDescriptor) {
        Object.defineProperty(globalThis, "window", originalWindowDescriptor);
      } else {
        // Ensure we restore the pre-test global shape for other unit tests.
        Reflect.deleteProperty(globalThis, "window");
      }
    }
  });
});
