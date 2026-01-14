import { describe, expect, it } from "vitest";

import { isVmRequested, shouldRunLegacyDemoMode } from "./vm_mode";

describe("runtime/vm_mode", () => {
  it("vmRuntime=machine never enters legacy demo mode (even with no disks)", () => {
    expect(
      shouldRunLegacyDemoMode({
        config: { vmRuntime: "machine" },
        bootDisks: { mounts: {}, hdd: null, cd: null },
      }),
    ).toBe(false);

    expect(
      isVmRequested({
        config: { vmRuntime: "machine" },
        bootDisks: { mounts: {}, hdd: null, cd: null },
      }),
    ).toBe(true);
  });

  it("vmRuntime=legacy + no boot disks enters legacy demo mode", () => {
    expect(
      shouldRunLegacyDemoMode({
        config: { vmRuntime: "legacy", activeDiskImage: "ignored.img" } as any,
        bootDisks: { mounts: {}, hdd: null, cd: null },
      }),
    ).toBe(true);

    expect(
      isVmRequested({
        config: { vmRuntime: "legacy" },
        bootDisks: { mounts: {}, hdd: null, cd: null },
      }),
    ).toBe(false);
  });

  it("vmRuntime=legacy + mounted disks requests a VM and disables demo mode", () => {
    expect(
      shouldRunLegacyDemoMode({
        config: { vmRuntime: "legacy" },
        bootDisks: { mounts: { hddId: "disk1" }, hdd: null, cd: null },
      }),
    ).toBe(false);

    expect(
      isVmRequested({
        config: { vmRuntime: "legacy" },
        bootDisks: { mounts: { hddId: "disk1" }, hdd: null, cd: null },
      }),
    ).toBe(true);
  });

  it("ignores mount IDs inherited from Object.prototype", () => {
    const hddExisting = Object.getOwnPropertyDescriptor(Object.prototype, "hddId");
    const cdExisting = Object.getOwnPropertyDescriptor(Object.prototype, "cdId");
    if ((hddExisting && hddExisting.configurable === false) || (cdExisting && cdExisting.configurable === false)) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "hddId", { value: "evil", configurable: true });
      Object.defineProperty(Object.prototype, "cdId", { value: "evil2", configurable: true });

      // With no real disks mounted, legacy runtime should still run demo mode.
      expect(
        shouldRunLegacyDemoMode({
          config: { vmRuntime: "legacy" },
          bootDisks: { mounts: {}, hdd: null, cd: null },
        }),
      ).toBe(true);
      expect(
        isVmRequested({
          config: { vmRuntime: "legacy" },
          bootDisks: { mounts: {}, hdd: null, cd: null },
        }),
      ).toBe(false);
    } finally {
      if (hddExisting) Object.defineProperty(Object.prototype, "hddId", hddExisting);
      else delete (Object.prototype as any).hddId;
      if (cdExisting) Object.defineProperty(Object.prototype, "cdId", cdExisting);
      else delete (Object.prototype as any).cdId;
    }
  });
});
