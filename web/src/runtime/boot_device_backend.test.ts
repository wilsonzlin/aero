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

  it("ignores inherited mounts/bootDevice fields", () => {
    const originalWindowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");

    try {
      const win: any = { aero: { debug: {} } };
      Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });

      const mounts = Object.create({ hddId: "evil", cdId: "evil2" });
      mounts.cdId = "cd0";

      const msg = Object.create({ bootDevice: "hdd" });
      msg.mounts = mounts;
      msg.type = "setBootDisks";
      msg.hdd = null;
      msg.cd = null;

      const coordinator: any = {
        getBootDisks: () => msg,
        getMachineCpuActiveBootDevice: () => null,
        getMachineCpuBootConfig: () => null,
      };

      installBootDeviceBackendOnAeroGlobal(coordinator);

      expect(win.aero.debug.getBootDisks()).toEqual({ mounts: { cdId: "cd0" } });
    } finally {
      if (originalWindowDescriptor) {
        Object.defineProperty(globalThis, "window", originalWindowDescriptor);
      } else {
        Reflect.deleteProperty(globalThis, "window");
      }
    }
  });

  it("sanitizes invalid machine CPU boot config payloads", () => {
    const originalWindowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");

    try {
      const win: any = { aero: { debug: {} } };
      Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });

      const coordinator: any = {
        getBootDisks: () => null,
        getMachineCpuActiveBootDevice: () => null,
        // Invalid: bootDrive is out of range and bootFromCdIfPresent is not boolean.
        getMachineCpuBootConfig: () => ({ bootDrive: 0x1ff, cdBootDrive: 0xe0, bootFromCdIfPresent: 1 }),
      };

      installBootDeviceBackendOnAeroGlobal(coordinator);

      expect(win.aero.debug.getMachineCpuBootConfig()).toBe(null);
    } finally {
      if (originalWindowDescriptor) {
        Object.defineProperty(globalThis, "window", originalWindowDescriptor);
      } else {
        Reflect.deleteProperty(globalThis, "window");
      }
    }
  });

  it("ignores inherited machine CPU boot config fields", () => {
    const originalWindowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");

    try {
      const win: any = { aero: { debug: {} } };
      Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });

      const inherited = Object.create({ bootDrive: 0x80, cdBootDrive: 0xe0, bootFromCdIfPresent: true });
      const coordinator: any = {
        getBootDisks: () => null,
        getMachineCpuActiveBootDevice: () => null,
        getMachineCpuBootConfig: () => inherited,
      };

      installBootDeviceBackendOnAeroGlobal(coordinator);

      expect(win.aero.debug.getMachineCpuBootConfig()).toBe(null);
    } finally {
      if (originalWindowDescriptor) {
        Object.defineProperty(globalThis, "window", originalWindowDescriptor);
      } else {
        Reflect.deleteProperty(globalThis, "window");
      }
    }
  });

  it("repairs window.aero/debug when they are non-objects", () => {
    const originalWindowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");

    try {
      const win: any = { aero: "not-an-object" };
      Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });

      const coordinator: any = {
        getBootDisks: () => null,
        getMachineCpuActiveBootDevice: () => null,
        getMachineCpuBootConfig: () => null,
      };

      installBootDeviceBackendOnAeroGlobal(coordinator);

      expect(win.aero && typeof win.aero).toBe("object");
      expect(win.aero.debug && typeof win.aero.debug).toBe("object");
      expect(typeof win.aero.debug.getBootDisks).toBe("function");

      // Also repair the debug object itself if it is replaced by some other tooling.
      win.aero.debug = "not-an-object";
      installBootDeviceBackendOnAeroGlobal(coordinator);
      expect(win.aero.debug && typeof win.aero.debug).toBe("object");
      expect(typeof win.aero.debug.getBootDisks).toBe("function");
    } finally {
      if (originalWindowDescriptor) {
        Object.defineProperty(globalThis, "window", originalWindowDescriptor);
      } else {
        Reflect.deleteProperty(globalThis, "window");
      }
    }
  });
});
