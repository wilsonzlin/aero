import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";

function readLe(buf: Uint8Array, off: number, size: number): number {
  if (off < 0 || off + size > buf.length) return defaultReadValue(size);
  if (size === 1) return buf[off]!;
  if (size === 2) return (buf[off]! | (buf[off + 1]! << 8)) >>> 0;
  return (
    (buf[off]! | (buf[off + 1]! << 8) | (buf[off + 2]! << 16) | (buf[off + 3]! << 24)) >>> 0
  );
}

function writeLe(buf: Uint8Array, off: number, size: number, value: number): void {
  if (off < 0 || off + size > buf.length) return;
  const v = value >>> 0;
  buf[off] = v & 0xff;
  if (size >= 2) buf[off + 1] = (v >>> 8) & 0xff;
  if (size >= 4) {
    buf[off + 2] = (v >>> 16) & 0xff;
    buf[off + 3] = (v >>> 24) & 0xff;
  }
}

/**
 * A tiny PCI device used by the framework tests.
 *
 * - One MMIO BAR (BAR0) of size 0x100.
 * - MMIO accesses read/write a small register file.
 */
export class PciTestDevice implements PciDevice {
  readonly name = "pci_test";
  readonly vendorId = 0x1234;
  readonly deviceId = 0x5678;
  readonly subsystemVendorId = 0xabcd;
  readonly subsystemId = 0xef01;
  readonly classCode = 0xff_00_00; // "other"
  readonly irqLine = 0x0b;
  readonly interruptPin = 0x02; // INTB#

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio32", size: 0x100 }, null, null, null, null, null];

  readonly #regs = new Uint8Array(0x100);

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (barIndex !== 0) return defaultReadValue(size);
    return readLe(this.#regs, Number(offset), size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (barIndex !== 0) return;
    writeLe(this.#regs, Number(offset), size, value);
  }
}
