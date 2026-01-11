import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";

export type WebUsbUhciBridgeLike = {
  io_read(offset: number, size: number): number;
  io_write(offset: number, size: number, value: number): void;
};

/**
 * Intel PIIX3 UHCI controller (PCI function) that forwards register accesses into WASM.
 *
 * This is intentionally "thin": BAR4 is an I/O range with the UHCI register block
 * (0x20 bytes). The actual controller logic (TD/QH traversal, guest RAM reads/writes,
 * passthrough device) lives in Rust (`WebUsbUhciBridge`).
 */
export class UhciWebUsbPciDevice implements PciDevice {
  readonly name = "uhci_webusb";
  readonly vendorId = 0x8086;
  readonly deviceId = 0x7020;
  readonly classCode = 0x0c_03_00; // USB controller (UHCI)
  readonly irqLine = 0x0b;

  readonly bars: ReadonlyArray<PciBar | null> = [null, null, null, null, { kind: "io", size: 0x20 }, null];

  constructor(private readonly bridge: WebUsbUhciBridgeLike) {}

  ioRead(barIndex: number, offset: number, size: number): number {
    if (barIndex !== 4) return defaultReadValue(size);
    try {
      return this.bridge.io_read(offset >>> 0, size >>> 0) >>> 0;
    } catch {
      return defaultReadValue(size);
    }
  }

  ioWrite(barIndex: number, offset: number, size: number, value: number): void {
    if (barIndex !== 4) return;
    try {
      this.bridge.io_write(offset >>> 0, size >>> 0, value >>> 0);
    } catch {
      // ignore
    }
  }
}
