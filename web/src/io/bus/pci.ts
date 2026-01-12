import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PortIoHandler } from "./portio.ts";
import type { PortIoBus } from "./portio.ts";
import type { MmioBus, MmioHandle } from "./mmio.ts";
import { PCI_MMIO_BASE } from "../../arch/guest_phys.ts";

export type PciBar =
  | {
      kind: "mmio32";
      size: number;
    }
  | {
      kind: "mmio64";
      size: number;
    }
  | {
      kind: "io";
      size: number;
    };

export interface PciCapability {
  /**
   * Raw PCI capability bytes including the standard 2-byte header:
   * - bytes[0] = capability ID
   * - bytes[1] = next pointer (will be overwritten by {@link PciBus} when chaining)
   */
  readonly bytes: Uint8Array;
}

export interface PciDevice {
  readonly name: string;
  readonly vendorId: number;
  readonly deviceId: number;
  /**
   * Optional requested PCI address (Bus/Device/Function).
   *
   * If provided, {@link PciBus.registerDevice} will use this BDF as the default registration
   * address (unless overridden by an explicit `addr` argument).
   */
  readonly bdf?: PciAddress;
  /**
   * Subsystem Vendor ID (SSVID) @ 0x2C.
   *
   * If omitted, defaults to {@link vendorId}.
   */
  readonly subsystemVendorId?: number;
  /**
   * PCI Subsystem Device ID (type 0 header, offset 0x2E..0x2F).
   *
   * Alias: {@link subsystemId} (legacy name).
   */
  readonly subsystemDeviceId?: number;
  /**
   * Legacy alias for {@link subsystemDeviceId}.
   */
  readonly subsystemId?: number;
  /**
   * Class code packed as 0xBBSSPP (base class, subclass, programming interface).
   * Example: AHCI is 0x010601.
   */
  readonly classCode: number;
  readonly revisionId?: number;
  /**
   * Interrupt line @ 0x3C.
   *
   * This field is writable by the guest (per PCI spec) and typically used to
   * report legacy PIC routing.
   */
  readonly irqLine?: number;
  /**
   * PCI interrupt pin number (0x3D): 0=none, 1=INTA#, 2=INTB#, 3=INTC#, 4=INTD#.
   *
   * Defaults to INTA# for endpoint devices.
   */
  readonly interruptPin?: 0 | 1 | 2 | 3 | 4;
  readonly bars?: ReadonlyArray<PciBar | null>;
  /**
   * PCI header type (standard offset 0x0E). Bit 7 is the multifunction bit.
   *
   * For multi-function devices, the bus will automatically set bit 7 on
   * function 0 when any additional functions are registered.
   */
  readonly headerType?: number;
  /**
   * Optional PCI capabilities to expose in configuration space. The bus will
   * build a valid, 4-byte aligned capability list, set the Status.CAP_LIST bit,
   * and populate the Capabilities Pointer at 0x34.
   */
  readonly capabilities?: ReadonlyArray<PciCapability>;
  /**
   * Optional hook for devices that need to populate additional PCI config space
   * fields after the bus has written the standard header/BARs/capabilities.
   */
  initConfigSpace?(config: Uint8Array, addr: PciAddress): void;
  /**
   * Optional hook called once during {@link PciBus.registerDevice}.
   *
   * Called after the bus has written:
   * - vendor/device IDs
   * - revision/class code
   * - header type
   * - IRQ line/pin
   * - BARs (including any 64-bit BARs)
   *
   * This enables devices (e.g. virtio-pci modern) to populate fields like:
   * - subsystem vendor/device IDs (0x2c..0x2f)
   * - Status.CAP_LIST + capability pointer (0x34) + capability structures
   *
   * The PCI bus will re-assert BAR and command register invariants after this
   * hook returns so devices cannot interfere with BAR decoding/mapping.
   */
  initPciConfig?(config: Uint8Array): void;
  /**
   * Optional hook for integrations that need to mirror PCI config-space state into an underlying
   * device model.
   *
   * Called after the bus has applied the guest write to the PCI command register (0x04, low 16
   * bits). The argument is the new 16-bit command value.
   */
  onPciCommandWrite?(command: number): void;

  /**
   * Optional hook invoked when the guest writes to PCI configuration space.
   *
   * The PCI bus owns the config space byte array (including enforcing RW/RO
   * fields and BAR decoding invariants). This callback allows device models to
   * mirror relevant config state (e.g. PCI command register Bus Master Enable)
   * into an out-of-band backend such as a WASM device model.
   */
  pciConfigWrite?(alignedOff: number, size: number, value: number): void;

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
  // Logical BAR index (0..5). For 64-bit BARs, this is the index of the low dword.
  index: number;
  base: bigint;
  sizingLow: boolean;
  sizingHigh: boolean;
  mmioHandle: MmioHandle | null;
  ioRange: { start: number; end: number } | null;
  ioHandler: PortIoHandler | null;
}

type PciBarSlot =
  | {
      bar: PciBarState;
      part: "low";
    }
  | {
      bar: PciBarState;
      part: "high";
    };

interface PciFunction {
  addr: PciAddress;
  config: Uint8Array;
  device: PciDevice;
  bars: Array<PciBarSlot | null>;
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
  if (desc.kind === "mmio64") {
    // Low dword mask. Type bits must indicate 64-bit memory BAR (bits 2:1 = 0b10).
    const fullMask = (~BigInt(desc.size - 1) & 0xffff_ffff_ffff_ffffn) & 0xffff_ffff_ffff_fff0n;
    const low = Number(fullMask & 0xffff_ffffn) >>> 0;
    return (low | 0x4) >>> 0;
  }
  // IO BAR.
  return ((~(desc.size - 1) & 0xffff_fffc) | 0x1) >>> 0;
}

function computeBarMaskHigh(desc: PciBar): number {
  if (desc.kind !== "mmio64") return 0;
  if (!isPow2(desc.size)) {
    throw new Error(`PCI BAR size must be power-of-two, got ${desc.size}`);
  }
  const fullMask = (~BigInt(desc.size - 1) & 0xffff_ffff_ffff_ffffn) & 0xffff_ffff_ffff_fff0n;
  return Number((fullMask >> 32n) & 0xffff_ffffn) >>> 0;
}

export class PciBus implements PortIoHandler {
  readonly #portBus: PortIoBus;
  readonly #mmioBus: MmioBus;
  #functions: Array<Array<PciFunction | null>> = Array.from({ length: 32 }, () =>
    Array.from({ length: 8 }, () => null),
  );
  #addrReg = 0;

  // Simple allocators for auto-assigned BARs (legacy 32-bit).
  #nextMmioBase = BigInt(PCI_MMIO_BASE);
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

  registerDevice(device: PciDevice, addr: Partial<PciAddress> = {}): PciAddress {
    const deviceBdf = device.bdf;

    const bus = addr.bus ?? deviceBdf?.bus ?? 0;
    if (!Number.isInteger(bus) || bus < 0) throw new RangeError(`PCI bus out of range: ${bus}`);
    if (bus !== 0) throw new Error(`only PCI bus 0 is supported, got bus ${bus}`);

    const fnNum = addr.function ?? deviceBdf?.function ?? 0;
    if (!Number.isInteger(fnNum)) throw new RangeError(`PCI function out of range: ${fnNum}`);
    if (fnNum < 0 || fnNum > 7) throw new RangeError(`PCI function out of range: ${fnNum}`);

    let devNum: number;
    if (addr.device !== undefined) {
      devNum = addr.device;
    } else if (deviceBdf?.device !== undefined) {
      devNum = deviceBdf.device;
    } else {
      devNum = this.#allocDeviceNumber();
    }
    if (!Number.isInteger(devNum)) throw new RangeError(`PCI device out of range: ${devNum}`);
    if (devNum < 0 || devNum > 31) throw new RangeError(`PCI device out of range: ${devNum}`);

    if (this.#functions[devNum]![fnNum] !== null) {
      throw new Error(`PCI address already in use: ${bus}:${devNum}.${fnNum}`);
    }

    const addrFull: PciAddress = { bus, device: devNum, function: fnNum };
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

    // Header type (type 0 by default).
    config[0x0e] = (device.headerType ?? 0x00) & 0xff;

    // Subsystem IDs (type 0 header).
    // Default to the device's own vendor/device IDs (improves guest driver matching).
    const subsystemVendorId = (device.subsystemVendorId ?? device.vendorId) & 0xffff;
    const subsystemDeviceId = (device.subsystemDeviceId ?? device.subsystemId ?? device.deviceId) & 0xffff;
    config[0x2c] = subsystemVendorId & 0xff;
    config[0x2d] = (subsystemVendorId >>> 8) & 0xff;
    config[0x2e] = subsystemDeviceId & 0xff;
    config[0x2f] = (subsystemDeviceId >>> 8) & 0xff;

    // Interrupt line/pin.
    config[0x3c] = (device.irqLine ?? 0x00) & 0xff;
    const intPin = device.interruptPin ?? 0x01;
    if (intPin < 0 || intPin > 4) throw new Error(`PCI interruptPin must be 0..4, got ${intPin}`);
    config[0x3d] = intPin & 0xff;

    const bars: Array<PciBarSlot | null> = Array.from({ length: 6 }, () => null);
    const barDescs = device.bars ?? [];
    for (let i = 0; i < 6; i++) {
      const desc = barDescs[i] ?? null;
      if (!desc) continue;

      if (desc.kind === "mmio64") {
        if (i >= 5) {
          throw new Error(`PCI device ${device.name}: 64-bit BAR cannot start at BAR5`);
        }
        if (barDescs[i + 1] != null) {
          throw new Error(`PCI device ${device.name}: 64-bit BAR at BAR${i} consumes BAR${i + 1}; it must be null`);
        }

        const base = this.#allocBarBase(desc);
        const state: PciBarState = {
          desc,
          index: i,
          base,
          sizingLow: false,
          sizingHigh: false,
          mmioHandle: null,
          ioRange: null,
          ioHandler: null,
        };

        bars[i] = { bar: state, part: "low" };
        bars[i + 1] = { bar: state, part: "high" };
        writeU32LE(config, 0x10 + i * 4, this.#encodeBarValueLow(state));
        writeU32LE(config, 0x10 + (i + 1) * 4, this.#encodeBarValueHigh(state));
        i++;
        continue;
      }

      const base = this.#allocBarBase(desc);
      const state: PciBarState = {
        desc,
        index: i,
        base,
        sizingLow: false,
        sizingHigh: false,
        mmioHandle: null,
        ioRange: null,
        ioHandler: null,
      };
      bars[i] = { bar: state, part: "low" };
      writeU32LE(config, 0x10 + i * 4, this.#encodeBarValueLow(state));
    }

    // Install PCI capabilities (if any).
    if (device.capabilities && device.capabilities.length > 0) {
      this.#installCapabilities(config, device.capabilities);
    }

    // Allow device to initialize additional config space (e.g. subsystem IDs,
    // capability list structures). Runs after any bus-installed capabilities so
    // devices may fully control the capabilities pointer/list if desired.
    device.initPciConfig?.(config);

    // Allow device to populate additional config bytes.
    device.initConfigSpace?.(config, addrFull);

    // Ensure devices cannot violate BAR decoding/mapping invariants via config
    // init hooks. (Status bits are intentionally left untouched.)
    //
    // Keep PCI command bits clear until the guest enables them.
    config[0x04] = 0x00;
    config[0x05] = 0x00;
    // Re-encode BAR dwords from the bus-controlled state so config space stays
    // consistent with runtime BAR mappings.
    for (const slot of bars) {
      if (!slot || slot.part !== "low") continue;
      const bar = slot.bar;
      writeU32LE(config, 0x10 + bar.index * 4, this.#encodeBarValueLow(bar));
      if (bar.desc.kind === "mmio64") {
        writeU32LE(config, 0x10 + (bar.index + 1) * 4, this.#encodeBarValueHigh(bar));
      }
    }
    // For type-0 headers, any unimplemented BAR registers must read as 0 and ignore guest writes.
    // Ensure config space reflects that invariant even if init hooks scribbled into those bytes.
    if ((config[0x0e]! & 0x7f) === 0) {
      for (let i = 0; i < 6; i++) {
        if (bars[i] === null) writeU32LE(config, 0x10 + i * 4, 0);
      }
    }

    const fn: PciFunction = { addr: addrFull, config, device, bars };
    this.#functions[devNum]![fnNum] = fn;

    // Multifunction: if any additional functions are registered, function 0 must
    // advertise it via the Header Type multifunction bit.
    if (fnNum !== 0) {
      const fn0 = this.#functions[devNum]![0];
      if (fn0) fn0.config[0x0e] = (fn0.config[0x0e]! | 0x80) & 0xff;
    } else {
      // fn0 registered; if other functions already exist, set the bit now.
      for (let f = 1; f < 8; f++) {
        if (this.#functions[devNum]![f]) {
          config[0x0e] = (config[0x0e]! | 0x80) & 0xff;
          break;
        }
      }
    }

    return addrFull;
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
    return this.#functions[dev]?.[fn] ?? null;
  }

  #readConfigDword(fn: PciFunction, alignedOff: number): number {
    // BAR sizing probe support (OS writes all-ones then reads mask).
    const headerType = fn.config[0x0e]! & 0x7f;
    if (headerType === 0 && alignedOff >= 0x10 && alignedOff <= 0x24) {
      const barIndex = (alignedOff - 0x10) >>> 2;
      const slot = fn.bars[barIndex] ?? null;
      if (slot) {
        const bar = slot.bar;
        if (slot.part === "low" && bar.sizingLow) return computeBarMask(bar.desc);
        if (slot.part === "high" && bar.sizingHigh) return computeBarMaskHigh(bar.desc);
      }
    }
    return readU32LE(fn.config, alignedOff);
  }

  #writeConfigDword(fn: PciFunction, alignedOff: number, value: number): void {
    // Command register changes affect BAR decoding enablement.
    if (alignedOff === 0x04) {
      // PCI header dword @ 0x04:
      // - Command register (0x04..0x05) is writable.
      // - Status register  (0x06..0x07) is RO / RW1C on real hardware and must not
      //   be blindly overwritten by 32-bit command writes. Guests commonly write
      //   the full dword with the upper 16 bits as zero, which would otherwise
      //   clobber Status bits such as "Capabilities List" (bit 4) used by modern
      //   virtio-pci.
      const cur = readU32LE(fn.config, alignedOff);
      const newValue = ((cur & 0xffff_0000) | (value & 0x0000_ffff)) >>> 0;
      writeU32LE(fn.config, alignedOff, newValue);
      const curCommand = cur & 0xffff;
      const newCommand = newValue & 0xffff;
      if (curCommand !== newCommand) {
        this.#refreshDeviceDecoding(fn);
        try {
          fn.device.onPciCommandWrite?.(newCommand >>> 0);
        } catch {
          // Ignore device hook failures; PCI config space writes should remain resilient to
          // device implementation bugs.
        }

        try {
          fn.device.pciConfigWrite?.(alignedOff, 4, newValue);
        } catch {
          // Ignore device hook failures; PCI config space writes should remain resilient to
          // device implementation bugs.
        }
      }
      return;
    }

    const headerType = fn.config[0x0e]! & 0x7f;
    if (headerType === 0 && alignedOff >= 0x10 && alignedOff <= 0x24) {
      const barIndex = (alignedOff - 0x10) >>> 2;
      const slot = fn.bars[barIndex] ?? null;
      if (!slot) {
        // Unimplemented BAR: writes have no effect (reads always return 0).
        return;
      }

      const bar = slot.bar;
      if (value === 0xffff_ffff) {
        if (slot.part === "low") bar.sizingLow = true;
        else bar.sizingHigh = true;
        // Store all-ones as written; reads will return mask while sizing is true.
        writeU32LE(fn.config, alignedOff, value);
        fn.device.pciConfigWrite?.(alignedOff, 4, value >>> 0);
        return;
      }

      if (slot.part === "low") bar.sizingLow = false;
      else bar.sizingHigh = false;

      // Update BAR base. For 64-bit BARs the base can be written via either dword.
      if (bar.desc.kind === "mmio64") {
        if (slot.part === "low") {
          const lo = BigInt((value & 0xffff_fff0) >>> 0);
          bar.base = (bar.base & 0xffff_ffff_0000_0000n) | lo;
        } else {
          const hi = BigInt(value >>> 0);
          const lo = bar.base & 0xffff_ffffn;
          bar.base = (hi << 32n) | lo;
        }
        // Always write both halves in canonical form (correct type bits in low dword).
        const lowOff = 0x10 + bar.index * 4;
        const highOff = 0x10 + (bar.index + 1) * 4;
        const lowVal = this.#encodeBarValueLow(bar);
        const highVal = this.#encodeBarValueHigh(bar);
        writeU32LE(fn.config, lowOff, lowVal);
        writeU32LE(fn.config, highOff, highVal);
        fn.device.pciConfigWrite?.(lowOff, 4, lowVal);
        fn.device.pciConfigWrite?.(highOff, 4, highVal);
      } else {
        // 32-bit MMIO or IO BAR.
        if (bar.desc.kind === "mmio32") bar.base = BigInt((value & 0xffff_fff0) >>> 0);
        else bar.base = BigInt((value & 0xffff_fffc) >>> 0);
        const newValue = this.#encodeBarValueLow(bar);
        writeU32LE(fn.config, alignedOff, newValue);
        fn.device.pciConfigWrite?.(alignedOff, 4, newValue);
      }

      // Remap BAR.
      this.#unmapBar(bar);
      this.#mapBarIfEnabled(fn, bar);
      return;
    }

    const mask = this.#writableMaskForDword(alignedOff);
    if (mask === 0) return;
    if (mask === 0xffff_ffff) {
      writeU32LE(fn.config, alignedOff, value);
      fn.device.pciConfigWrite?.(alignedOff, 4, value >>> 0);
      return;
    }
    const cur = readU32LE(fn.config, alignedOff);
    const newValue = ((cur & ~mask) | (value & mask)) >>> 0;
    writeU32LE(fn.config, alignedOff, newValue);
    fn.device.pciConfigWrite?.(alignedOff, 4, newValue);
  }

  #writableMaskForDword(alignedOff: number): number {
    // Keep a small mask table for registers we care about.
    // Any unlisted register defaults to RW (helps compatibility with guests).
    switch (alignedOff) {
      case 0x00:
        // Vendor/device IDs are RO.
        return 0x0000_0000;
      case 0x08:
        // Revision/class code are RO.
        return 0x0000_0000;
      case 0x0c:
        // Cache line size (0x0C), latency timer (0x0D), BIST (0x0F) are writable.
        // Header type (0x0E) is RO.
        return 0xff00_ffff;
      case 0x2c:
        // Subsystem IDs are RO.
        return 0x0000_0000;
      case 0x3c:
        // Interrupt line is RW; interrupt pin (and other bytes) are RO.
        return 0x0000_00ff;
      default:
        return 0xffff_ffff;
    }
  }

  #commandFlags(fn: PciFunction): { ioEnabled: boolean; memEnabled: boolean } {
    const command = (fn.config[0x04]! | (fn.config[0x05]! << 8)) >>> 0;
    return {
      ioEnabled: (command & 0x1) !== 0,
      memEnabled: (command & 0x2) !== 0,
    };
  }

  #refreshDeviceDecoding(fn: PciFunction): void {
    for (const slot of fn.bars) {
      if (!slot || slot.part !== "low") continue;
      const bar = slot.bar;
      this.#unmapBar(bar);
      this.#mapBarIfEnabled(fn, bar);
    }
  }

  #mapBarIfEnabled(fn: PciFunction, bar: PciBarState): void {
    // BARs decode only when PCI command bits enable them.
    if (bar.base === 0n) return;
    const { ioEnabled, memEnabled } = this.#commandFlags(fn);
    if (bar.desc.kind === "io") {
      if (!ioEnabled) return;
      this.#mapBar(fn.device, bar.index, bar);
      return;
    }
    if (bar.desc.kind === "mmio32" || bar.desc.kind === "mmio64") {
      if (!memEnabled) return;
      this.#mapBar(fn.device, bar.index, bar);
      return;
    }
  }

  #encodeBarValueLow(bar: PciBarState): number {
    if (bar.desc.kind === "mmio32") {
      return Number(bar.base & 0xffff_fff0n) >>> 0;
    }
    if (bar.desc.kind === "mmio64") {
      return (Number(bar.base & 0xffff_fff0n) | 0x4) >>> 0;
    }
    return (Number(bar.base & 0xffff_fffcn) | 0x1) >>> 0;
  }

  #encodeBarValueHigh(bar: PciBarState): number {
    if (bar.desc.kind !== "mmio64") return 0;
    return Number((bar.base >> 32n) & 0xffff_ffffn) >>> 0;
  }

  #allocBarBase(desc: PciBar): bigint {
    if (!isPow2(desc.size)) throw new Error(`BAR size must be power-of-two, got ${desc.size}`);

    if (desc.kind === "mmio32" || desc.kind === "mmio64") {
      const align = BigInt(Math.max(desc.size, 0x1000));
      const base = (this.#nextMmioBase + (align - 1n)) & ~(align - 1n);
      this.#nextMmioBase = base + BigInt(desc.size);
      return base;
    }

    const align = Math.max(desc.size, 4);
    const base = (this.#nextIoBase + (align - 1)) & ~(align - 1);
    this.#nextIoBase = (base + desc.size) & 0xffff;
    return BigInt(base & 0xffff);
  }

  #mapBar(device: PciDevice, barIndex: number, bar: PciBarState): void {
    if (bar.desc.kind === "mmio32" || bar.desc.kind === "mmio64") {
      bar.mmioHandle = this.#mmioBus.mapRange(bar.base, BigInt(bar.desc.size), {
        mmioRead: (offset, size) => device.mmioRead?.(barIndex, offset, size) ?? defaultReadValue(size),
        mmioWrite: (offset, size, value) => device.mmioWrite?.(barIndex, offset, size, value),
      });
      return;
    }

    const start = Number(bar.base & 0xffffn);
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

  #allocDeviceNumber(): number {
    for (let dev = 0; dev < 32; dev++) {
      const fns = this.#functions[dev]!;
      let any = false;
      for (let fn = 0; fn < 8; fn++) {
        if (fns[fn] !== null) {
          any = true;
          break;
        }
      }
      if (!any) return dev;
    }
    throw new Error("PCI bus full (max 32 devices on bus 0)");
  }

  #installCapabilities(config: Uint8Array, caps: ReadonlyArray<PciCapability>): void {
    // PCI spec: capability list lives in the device-specific region after the
    // standard 0x40-byte type-0 header.
    //
    // Start at 0x50 so Aero's virtio-pci devices expose a stable capability layout
    // (common/notif/isr/device at fixed offsets). Keeping 0x40..0x4f unused also
    // leaves room for future standard capabilities (e.g. MSI/MSI-X) without
    // needing to reshuffle vendor-specific offsets.
    let nextOff = 0x50;
    let firstPtr = 0;
    let prevPtr = 0;

    for (const cap of caps) {
      const bytes = cap.bytes;
      if (bytes.length < 2) throw new Error("PCI capability too short (need at least 2 bytes)");
      nextOff = (nextOff + 3) & ~3; // 4-byte aligned.
      if (nextOff > 0xff) throw new Error("PCI capability list overflow");
      if (nextOff + bytes.length > config.length) throw new Error("PCI capability list exceeds config space");

      if (firstPtr === 0) firstPtr = nextOff;
      if (prevPtr !== 0) config[prevPtr + 1] = nextOff & 0xff;

      config.set(bytes, nextOff);
      // Bus owns next-pointer chaining.
      config[nextOff + 1] = 0;
      // For vendor-specific capabilities (0x09), ensure cap_len matches the structure length.
      if (config[nextOff] === 0x09 && bytes.length >= 3) config[nextOff + 2] = bytes.length & 0xff;

      prevPtr = nextOff;
      nextOff += bytes.length;
    }

    if (firstPtr === 0) return;
    config[0x34] = firstPtr & 0xff;
    // Status bit 4: capabilities list.
    const status = (config[0x06]! | (config[0x07]! << 8)) >>> 0;
    const newStatus = (status | 0x0010) >>> 0;
    config[0x06] = newStatus & 0xff;
    config[0x07] = (newStatus >>> 8) & 0xff;
  }
}
