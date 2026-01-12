import { describe, expect, it } from "vitest";

import { MmioBus } from "./mmio";
import { PciBus, type PciDevice } from "./pci";
import { PortIoBus } from "./portio";

function pciCfgAddr(opts: { bus: number; device: number; function: number; reg: number }): number {
  // PCI config mechanism #1 address register:
  //   bit 31: enable
  //   23:16 bus, 15:11 device, 10:8 function
  //   7:2 register number (DWORD aligned)
  return (
    0x8000_0000 |
    ((opts.bus & 0xff) << 16) |
    ((opts.device & 0x1f) << 11) |
    ((opts.function & 0x07) << 8) |
    (opts.reg & 0xfc)
  );
}

describe("io/bus/PciBus COMMAND/STATUS write semantics", () => {
  it("preserves STATUS bits (Capabilities List) on 32-bit writes to 0x04", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const device: PciDevice = {
      name: "pci_status_test",
      vendorId: 0x1af4,
      deviceId: 0x1000,
      classCode: 0xff_00_00,
      initPciConfig: (config) => {
        // PCI STATUS bit 4: "Capabilities List"
        config[0x06] |= 0x10;
      },
    };

    const addr = pciBus.registerDevice(device);
    portBus.write(0x0cf8, 4, pciCfgAddr({ ...addr, reg: 0x04 }));

    const before = portBus.read(0x0cfc, 4);
    expect((before >>> 16) & 0x10).toBe(0x10);

    // Windows commonly uses a 32-bit write to 0x04 (COMMAND+STATUS) even though it
    // intends to only update COMMAND. STATUS must not be clobbered by that write.
    portBus.write(0x0cfc, 4, 0x0000_0007); // IO + MEM + BUS MASTER

    const after = portBus.read(0x0cfc, 4);
    expect(after & 0xffff).toBe(0x0007);
    expect((after >>> 16) & 0x10).toBe(0x10);
  });

  it("preserves STATUS bits on partial writes routed via 0xCFC+{1,2,3}", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const device: PciDevice = {
      name: "pci_status_test_partial",
      vendorId: 0x1af4,
      deviceId: 0x1000,
      classCode: 0xff_00_00,
      initPciConfig: (config) => {
        config[0x06] |= 0x10;
      },
    };

    const addr = pciBus.registerDevice(device);
    portBus.write(0x0cf8, 4, pciCfgAddr({ ...addr, reg: 0x04 }));

    // Write low COMMAND byte via 0xCFC.
    portBus.write(0x0cfc, 1, 0x07);
    // Unaligned 16-bit write via 0xCFC+1 (touches COMMAND high byte + STATUS low byte).
    // STATUS must still be preserved.
    portBus.write(0x0cfd, 2, 0x0001);

    const after = portBus.read(0x0cfc, 4);
    expect(after & 0xffff).toBe(0x0107);
    expect((after >>> 16) & 0x10).toBe(0x10);
  });
});

