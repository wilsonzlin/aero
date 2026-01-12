import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciCapability, PciDevice } from "../bus/pci.ts";

/**
 * Two-function PCI device used by Playwright tests to validate:
 * - multi-function config access (fn0/fn1)
 * - 64-bit MMIO BAR encoding + sizing probe
 * - PCI capability list plumbing
 */

export class PciMultifunctionTestDeviceFn0 implements PciDevice {
  readonly name = "pci_multifn_fn0";
  readonly vendorId = 0x1af4;
  readonly deviceId = 0x1052;
  readonly classCode = 0xff_00_00; // "other"
  readonly revisionId = 0x01;
  readonly irqLine = 0x0b;
  readonly subsystemVendorId = 0x1af4;
  readonly subsystemDeviceId = 0x0010;

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio64", size: 0x4000 }, null, null, null, null, null];

  readonly capabilities: ReadonlyArray<PciCapability> = [
    {
      bytes: Uint8Array.from([
        0x09, // vendor-specific
        0x00, // next (set by bus)
        0x10, // length
        0x01,
        0xde,
        0xad,
        0xbe,
        0xef,
        0x00,
        0x11,
        0x22,
        0x33,
        0x44,
        0x55,
        0x66,
        0x77,
      ]),
    },
    {
      bytes: Uint8Array.from([
        0x09, // vendor-specific
        0x00, // next (set by bus)
        0x10, // length
        0x02,
        0xca,
        0xfe,
        0xba,
        0xbe,
        0x88,
        0x99,
        0xaa,
        0xbb,
        0xcc,
        0xdd,
        0xee,
        0xff,
      ]),
    },
  ];

  #reg0 = 0;

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (barIndex !== 0) return defaultReadValue(size);
    if (offset === 0n && size === 4) return this.#reg0 >>> 0;
    // Unlike generic unmapped MMIO (all-ones), virtio-pci modern expects undefined
    // offsets within the BAR to read as zeros; returning zero here keeps the test
    // device closer to that behavior.
    return 0;
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (barIndex !== 0) return;
    if (offset === 0n && size === 4) this.#reg0 = value >>> 0;
  }
}

export class PciMultifunctionTestDeviceFn1 implements PciDevice {
  readonly name = "pci_multifn_fn1";
  readonly vendorId = 0x1af4;
  readonly deviceId = 0x1052;
  readonly classCode = 0xff_00_00; // "other"
  readonly revisionId = 0x01;
  readonly irqLine = 0x0b;
  readonly subsystemVendorId = 0x1af4;
  readonly subsystemDeviceId = 0x0011;
}
