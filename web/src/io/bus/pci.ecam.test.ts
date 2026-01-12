import { describe, expect, it } from "vitest";

import { PCIE_ECAM_BASE } from "../../arch/guest_phys";
import { MmioBus } from "./mmio";
import { PciBus, type PciDevice } from "./pci";
import { PortIoBus } from "./portio";

function ecamAddr(bus: number, device: number, fn: number, reg: number): bigint {
  return (
    PCIE_ECAM_BASE +
    (BigInt(bus & 0xff) << 20n) +
    (BigInt(device & 0x1f) << 15n) +
    (BigInt(fn & 0x07) << 12n) +
    BigInt(reg & 0xfff)
  );
}

describe("io/bus/pci ECAM (MMCONFIG)", () => {
  it("routes ECAM MMIO reads/writes to PCI config space", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerEcamToMmioBus();

    const dev: PciDevice = { name: "ecam_dev", vendorId: 0x1234, deviceId: 0x5678, classCode: 0 };
    pciBus.registerDevice(dev, { device: 0, function: 0 });

    // Vendor ID (low 16) + Device ID (high 16).
    expect(mmioBus.read(ecamAddr(0, 0, 0, 0x00), 4)).toBe(0x5678_1234);

    // Command register.
    mmioBus.write(ecamAddr(0, 0, 0, 0x04), 2, 0x0007);
    expect(mmioBus.read(ecamAddr(0, 0, 0, 0x04), 2)).toBe(0x0007);

    // Unregistered functions/devices should read as all-ones.
    expect(mmioBus.read(ecamAddr(0, 0, 1, 0x00), 4)).toBe(0xffff_ffff);
    expect(mmioBus.read(ecamAddr(0, 1, 0, 0x00), 4)).toBe(0xffff_ffff);
  });
});

