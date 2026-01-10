import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PortIoHandler } from "./portio.ts";
import type { PortIoBus } from "./portio.ts";
import type { MmioBus, MmioHandle } from "./mmio.ts";

export type PciBar =
  | {
      kind: "mmio32";
      size: number;
    }
  | {
      kind: "io";
      size: number;
    };

export interface PciDevice {
  readonly name: string;
  readonly vendorId: number;
  readonly deviceId: number;
  /**
   * Class code packed as 0xBBSSPP (base class, subclass, programming interface).
   * Example: AHCI is 0x010601.
   */
  readonly classCode: number;
  readonly revisionId?: number;
  readonly irqLine?: number;
  readonly bars?: ReadonlyArray<PciBar | null>;

  mmioRead?(barIndex: number, offset: bigint, size: number): number;
  mmioWrite?(barIndex: number, offset: bigint, size: number, value: number): void;
  ioRead?(barIndex: number, offset: number, size: number): number;
  ioWrite?(barIndex: number, offset: number, size: number, value: number): void;
}

export interface PciAddress {
  bus: number;
  device: number;
  function: number;
}

interface PciBarState {
  desc: PciBar;
  base: number;
  sizing: boolean;
  mmioHandle: MmioHandle | null;
  ioRange: { start: number; end: number } | null;
  ioHandler: PortIoHandler | null;
}

interface PciFunction {
  addr: PciAddress;
  config: Uint8Array;
  device: PciDevice;
  bars: Array<PciBarState | null>;
}

function isPow2(n: number): boolean {
  return n > 0 && (n & (n - 1)) === 0;
}

function writeU32LE(buf: Uint8Array, off: number, value: number): void {
  buf[off] = value & 0xff;
  buf[off + 1] = (value >>> 8) & 0xff;
  buf[off + 2] = (value >>> 16) & 0xff;
  buf[off + 3] = (value >>> 24) & 0xff;
}

function readU32LE(buf: Uint8Array, off: number): number {
  return (
    (buf[off]! | (buf[off + 1]! << 8) | (buf[off + 2]! << 16) | (buf[off + 3]! << 24)) >>> 0
  );
}

function computeBarMask(desc: PciBar): number {
  if (!isPow2(desc.size)) {
    throw new Error(`PCI BAR size must be power-of-two, got ${desc.size}`);
  }
  if (desc.kind === "mmio32") {
    return (~(desc.size - 1) & 0xffff_fff0) >>> 0;
  }
  // IO BAR.
  return ((~(desc.size - 1) & 0xffff_fffc) | 0x1) >>> 0;
}

function sanitizeBarBase(desc: PciBar, value: number): number {
  if (desc.kind === "mmio32") return value & 0xffff_fff0;
  return value & 0xffff_fffc;
}

export class PciBus implements PortIoHandler {
  readonly #portBus: PortIoBus;
  readonly #mmioBus: MmioBus;
  #functions: PciFunction[] = [];
  #addrReg = 0;

  // Simple allocators for auto-assigned BARs (legacy 32-bit).
  #nextMmioBase = 0xe000_0000;
  #nextIoBase = 0xc000;

  constructor(portBus: PortIoBus, mmioBus: MmioBus) {
    this.#portBus = portBus;
    this.#mmioBus = mmioBus;
  }

  registerToPortBus(): void {
    // PCI config mechanism #1 uses 0xCF8 (address) and 0xCFC..0xCFF (data).
    // Avoid stealing 0xCF9, which is commonly used by a chipset reset-control port.
    this.#portBus.registerRange(0x0cf8, 0x0cf8, this);
    this.#portBus.registerRange(0x0cfc, 0x0cff, this);
  }

  registerDevice(device: PciDevice): PciAddress {
    if (this.#functions.length >= 32) throw new Error("PCI bus full (max 32 devices on bus 0)");

    const addr: PciAddress = { bus: 0, device: this.#functions.length, function: 0 };
    const config = new Uint8Array(256);

    // IDs.
    config[0x00] = device.vendorId & 0xff;
    config[0x01] = (device.vendorId >>> 8) & 0xff;
    config[0x02] = device.deviceId & 0xff;
    config[0x03] = (device.deviceId >>> 8) & 0xff;

    // Revision / class code.
    const revisionId = device.revisionId ?? 0x00;
    const classCode = device.classCode >>> 0;
    config[0x08] = revisionId & 0xff;
    config[0x09] = classCode & 0xff; // prog IF
    config[0x0a] = (classCode >>> 8) & 0xff; // subclass
    config[0x0b] = (classCode >>> 16) & 0xff; // base class

    // Header type: 0 (endpoint).
    config[0x0e] = 0x00;

    // Interrupt line/pin.
    config[0x3c] = (device.irqLine ?? 0x00) & 0xff;
    config[0x3d] = 0x01; // INTA#

    const bars: Array<PciBarState | null> = [];
    const barDescs = device.bars ?? [];
    for (let i = 0; i < 6; i++) {
      const desc = barDescs[i] ?? null;
      if (!desc) {
        bars.push(null);
        continue;
      }

      const base = this.#allocBarBase(desc);
      const state: PciBarState = {
        desc,
        base,
        sizing: false,
        mmioHandle: null,
        ioRange: null,
        ioHandler: null,
      };

      writeU32LE(config, 0x10 + i * 4, this.#encodeBarValue(state));
      bars.push(state);
    }

    const fn: PciFunction = { addr, config, device, bars };
    this.#functions.push(fn);
    return addr;
  }

  portRead(port: number, size: number): number {
    const p = port & 0xffff;
    if (p === 0x0cf8) {
      return this.#readFromReg(this.#addrReg, p, size, 0x0cf8);
    }
    if (p >= 0x0cfc && p <= 0x0cff) {
      if ((this.#addrReg & 0x8000_0000) === 0) return defaultReadValue(size);
      const fn = this.#getSelectedFunction();
      if (!fn) return defaultReadValue(size);

      const regOff = (this.#addrReg & 0xfc) + (p - 0x0cfc);
      const aligned = regOff & ~3;
      const dword = this.#readConfigDword(fn, aligned);
      return this.#readFromReg(dword, p, size, 0x0cfc + (aligned & 3));
    }
    return defaultReadValue(size);
  }

  portWrite(port: number, size: number, value: number): void {
    const p = port & 0xffff;
    const v = value >>> 0;
    if (p === 0x0cf8) {
      // Only support 32-bit writes for now (typical for PCI config).
      if (size !== 4) return;
      this.#addrReg = v >>> 0;
      return;
    }
    if (p >= 0x0cfc && p <= 0x0cff) {
      if ((this.#addrReg & 0x8000_0000) === 0) return;
      const fn = this.#getSelectedFunction();
      if (!fn) return;

      const regOff = (this.#addrReg & 0xfc) + (p - 0x0cfc);
      const aligned = regOff & ~3;

      // Preserve untouched bytes when writing < 4 bytes.
      let newDword: number;
      if (size === 4 && (regOff & 3) === 0) {
        newDword = v;
      } else {
        const cur = this.#readConfigDword(fn, aligned);
        const shift = (regOff & 3) * 8;
        const mask = size === 1 ? 0xff : size === 2 ? 0xffff : 0xffff_ffff;
        newDword = ((cur & ~(mask << shift)) | ((v & mask) << shift)) >>> 0;
      }

      this.#writeConfigDword(fn, aligned, newDword);
      return;
    }
  }

  #readFromReg(reg: number, port: number, size: number, basePort: number): number {
    const shift = ((port - basePort) & 3) * 8;
    if (size === 1) return (reg >>> shift) & 0xff;
    if (size === 2) return (reg >>> shift) & 0xffff;
    return reg >>> 0;
  }

  #getSelectedFunction(): PciFunction | null {
    const bus = (this.#addrReg >>> 16) & 0xff;
    if (bus !== 0) return null;
    const dev = (this.#addrReg >>> 11) & 0x1f;
    const fn = (this.#addrReg >>> 8) & 0x07;
    if (fn !== 0) return null;
    return this.#functions[dev] ?? null;
  }

  #readConfigDword(fn: PciFunction, alignedOff: number): number {
    // BAR sizing probe support (OS writes all-ones then reads mask).
    if (alignedOff >= 0x10 && alignedOff <= 0x24) {
      const barIndex = (alignedOff - 0x10) >>> 2;
      const bar = fn.bars[barIndex] ?? null;
      if (bar && bar.sizing) {
        return computeBarMask(bar.desc);
      }
    }
    return readU32LE(fn.config, alignedOff);
  }

  #writeConfigDword(fn: PciFunction, alignedOff: number, value: number): void {
    // Command register changes affect BAR decoding enablement.
    if (alignedOff === 0x04) {
      writeU32LE(fn.config, alignedOff, value);
      this.#refreshDeviceDecoding(fn);
      return;
    }

    if (alignedOff >= 0x10 && alignedOff <= 0x24) {
      const barIndex = (alignedOff - 0x10) >>> 2;
      const bar = fn.bars[barIndex] ?? null;
      if (!bar) {
        writeU32LE(fn.config, alignedOff, value);
        return;
      }

      if (value === 0xffff_ffff) {
        bar.sizing = true;
        // Store all-ones as written; reads will return mask while sizing is true.
        writeU32LE(fn.config, alignedOff, value);
        return;
      }

      bar.sizing = false;
      const newBase = sanitizeBarBase(bar.desc, value);
      bar.base = newBase;
      writeU32LE(fn.config, alignedOff, this.#encodeBarValue(bar));

      // Remap BAR.
      this.#unmapBar(bar);
      this.#mapBarIfEnabled(fn, barIndex, bar);
      return;
    }

    writeU32LE(fn.config, alignedOff, value);
  }

  #commandFlags(fn: PciFunction): { ioEnabled: boolean; memEnabled: boolean } {
    const command = (fn.config[0x04]! | (fn.config[0x05]! << 8)) >>> 0;
    return {
      ioEnabled: (command & 0x1) !== 0,
      memEnabled: (command & 0x2) !== 0,
    };
  }

  #refreshDeviceDecoding(fn: PciFunction): void {
    for (let barIndex = 0; barIndex < fn.bars.length; barIndex++) {
      const bar = fn.bars[barIndex];
      if (!bar) continue;
      this.#unmapBar(bar);
      this.#mapBarIfEnabled(fn, barIndex, bar);
    }
  }

  #mapBarIfEnabled(fn: PciFunction, barIndex: number, bar: PciBarState): void {
    // BARs decode only when PCI command bits enable them.
    if (bar.base === 0) return;
    const { ioEnabled, memEnabled } = this.#commandFlags(fn);
    if (bar.desc.kind === "io") {
      if (!ioEnabled) return;
      this.#mapBar(fn.device, barIndex, bar);
      return;
    }
    if (bar.desc.kind === "mmio32") {
      if (!memEnabled) return;
      this.#mapBar(fn.device, barIndex, bar);
      return;
    }
  }

  #encodeBarValue(bar: PciBarState): number {
    if (bar.desc.kind === "mmio32") {
      return (bar.base & 0xffff_fff0) >>> 0;
    }
    return ((bar.base & 0xffff_fffc) | 0x1) >>> 0;
  }

  #allocBarBase(desc: PciBar): number {
    if (!isPow2(desc.size)) throw new Error(`BAR size must be power-of-two, got ${desc.size}`);

    if (desc.kind === "mmio32") {
      const align = Math.max(desc.size, 0x1000);
      const base = (this.#nextMmioBase + (align - 1)) & ~(align - 1);
      this.#nextMmioBase = (base + desc.size) >>> 0;
      return base >>> 0;
    }

    const align = Math.max(desc.size, 4);
    const base = (this.#nextIoBase + (align - 1)) & ~(align - 1);
    this.#nextIoBase = (base + desc.size) & 0xffff;
    return base & 0xffff;
  }

  #mapBar(device: PciDevice, barIndex: number, bar: PciBarState): void {
    if (bar.desc.kind === "mmio32") {
      bar.mmioHandle = this.#mmioBus.mapRange(BigInt(bar.base >>> 0), BigInt(bar.desc.size), {
        mmioRead: (offset, size) => device.mmioRead?.(barIndex, offset, size) ?? defaultReadValue(size),
        mmioWrite: (offset, size, value) => device.mmioWrite?.(barIndex, offset, size, value),
      });
      return;
    }

    const start = bar.base & 0xffff;
    const end = (start + bar.desc.size - 1) & 0xffff;
    const handler: PortIoHandler = {
      portRead: (port, size) => device.ioRead?.(barIndex, (port - start) & 0xffff, size) ?? defaultReadValue(size),
      portWrite: (port, size, value) => device.ioWrite?.(barIndex, (port - start) & 0xffff, size, value),
    };
    this.#portBus.registerRange(start, end, handler);
    bar.ioRange = { start, end };
    bar.ioHandler = handler;
  }

  #unmapBar(bar: PciBarState): void {
    if (bar.mmioHandle !== null) {
      this.#mmioBus.unmap(bar.mmioHandle);
      bar.mmioHandle = null;
    }
    if (bar.ioRange && bar.ioHandler) {
      this.#portBus.unregisterRange(bar.ioRange.start, bar.ioRange.end, bar.ioHandler);
      bar.ioRange = null;
      bar.ioHandler = null;
    }
  }
}
