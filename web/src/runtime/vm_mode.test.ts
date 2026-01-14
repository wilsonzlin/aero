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
});

