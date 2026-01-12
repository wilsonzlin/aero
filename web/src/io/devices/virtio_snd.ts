import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciCapability, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioSndPciBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  poll(): void;
  /**
   * Optional hook for mirroring PCI command register writes into the underlying device model.
   *
   * When present, this can be used by WASM bridges to enforce DMA gating based on Bus Master Enable.
   */
  set_pci_command?(command: number): void;
  driver_ok(): boolean;
  irq_asserted(): boolean;
  set_audio_ring_buffer(ringSab: SharedArrayBuffer | null | undefined, capacityFrames: number, channelCount: number): void;
  set_host_sample_rate_hz(rate: number): void;
  set_mic_ring_buffer(ringSab?: SharedArrayBuffer | null): void;
  set_capture_sample_rate_hz(rate: number): void;
  free(): void;
};

const VIRTIO_VENDOR_ID = 0x1af4;
const VIRTIO_SND_DEVICE_ID = 0x1059;
const VIRTIO_SND_SUBSYSTEM_DEVICE_ID = 0x0019;
const VIRTIO_SND_CLASS_CODE = 0x04_01_00;
const VIRTIO_CONTRACT_REVISION_ID = 0x01;

// BAR0 size in the Aero Windows 7 virtio contract v1 (see docs/windows7-virtio-driver-contract.md).
const VIRTIO_MMIO_BAR0_SIZE = 0x4000;

// Fixed virtio-pci capability layout within BAR0 (contract v1).
const VIRTIO_MMIO_COMMON_OFFSET = 0x0000;
const VIRTIO_MMIO_COMMON_LEN = 0x0100;
const VIRTIO_MMIO_NOTIFY_OFFSET = 0x1000;
const VIRTIO_MMIO_NOTIFY_LEN = 0x0100;
const VIRTIO_MMIO_ISR_OFFSET = 0x2000;
const VIRTIO_MMIO_ISR_LEN = 0x0020;
const VIRTIO_MMIO_DEVICE_OFFSET = 0x3000;
const VIRTIO_MMIO_DEVICE_LEN = 0x0100;
const VIRTIO_MMIO_NOTIFY_OFF_MULTIPLIER = 4;

// Pick a stable IRQ line not currently used by the built-in devices.
const VIRTIO_SND_IRQ_LINE = 0x09;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

function isInRange(off: number, size: number, base: number, len: number): boolean {
  return off >= base && off + size <= base + len;
}

function writeU32LE(buf: Uint8Array, off: number, value: number): void {
  buf[off] = value & 0xff;
  buf[off + 1] = (value >>> 8) & 0xff;
  buf[off + 2] = (value >>> 16) & 0xff;
  buf[off + 3] = (value >>> 24) & 0xff;
}

function virtioVendorCap(opts: {
  cfgType: number;
  bar: number;
  offset: number;
  length: number;
  notifyOffMultiplier?: number;
}): PciCapability {
  const capLen = opts.notifyOffMultiplier !== undefined ? 20 : 16;
  const bytes = new Uint8Array(capLen);
  // Standard PCI capability header.
  bytes[0] = 0x09; // Vendor-specific
  bytes[1] = 0x00; // next pointer (patched by PCI bus)
  bytes[2] = capLen & 0xff;

  // virtio_pci_cap fields.
  bytes[3] = opts.cfgType & 0xff;
  bytes[4] = opts.bar & 0xff;
  bytes[5] = 0x00; // id (unused)
  bytes[6] = 0x00;
  bytes[7] = 0x00;
  writeU32LE(bytes, 8, opts.offset >>> 0);
  writeU32LE(bytes, 12, opts.length >>> 0);
  if (opts.notifyOffMultiplier !== undefined) {
    writeU32LE(bytes, 16, opts.notifyOffMultiplier >>> 0);
  }
  return { bytes };
}

/**
 * Virtio-snd PCI function (virtio-pci modern transport) backed by the WASM `VirtioSndPciBridge`.
 *
 * Exposes:
 * - BAR0: 64-bit MMIO BAR, size 0x4000 (Aero Windows 7 virtio contract v1 modern layout).
 */
export class VirtioSndPciDevice implements PciDevice, TickableDevice {
  readonly name = "virtio_snd";
  readonly vendorId = VIRTIO_VENDOR_ID;
  readonly deviceId = VIRTIO_SND_DEVICE_ID;
  readonly subsystemVendorId = VIRTIO_VENDOR_ID;
  readonly subsystemId = VIRTIO_SND_SUBSYSTEM_DEVICE_ID;
  readonly classCode = VIRTIO_SND_CLASS_CODE;
  readonly revisionId = VIRTIO_CONTRACT_REVISION_ID;
  readonly irqLine = VIRTIO_SND_IRQ_LINE;
  readonly interruptPin = 1 as const;
  // Keep the canonical PCI address consistent with the docs + Rust PCI profile
  // (`docs/pci-device-compatibility.md`, `crates/devices/src/pci/profile.rs`).
  readonly bdf = { bus: 0, device: 11, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio64", size: VIRTIO_MMIO_BAR0_SIZE }, null, null, null, null, null];
  readonly capabilities: ReadonlyArray<PciCapability> = [
    // Virtio modern vendor-specific capabilities (contract v1 fixed BAR0 layout).
    // The PCI bus will install these starting at 0x40 with 4-byte aligned pointers.
    virtioVendorCap({ cfgType: 1, bar: 0, offset: VIRTIO_MMIO_COMMON_OFFSET, length: VIRTIO_MMIO_COMMON_LEN }), // COMMON_CFG
    virtioVendorCap({
      cfgType: 2,
      bar: 0,
      offset: VIRTIO_MMIO_NOTIFY_OFFSET,
      length: VIRTIO_MMIO_NOTIFY_LEN,
      notifyOffMultiplier: VIRTIO_MMIO_NOTIFY_OFF_MULTIPLIER,
    }), // NOTIFY_CFG
    virtioVendorCap({ cfgType: 3, bar: 0, offset: VIRTIO_MMIO_ISR_OFFSET, length: VIRTIO_MMIO_ISR_LEN }), // ISR_CFG
    virtioVendorCap({ cfgType: 4, bar: 0, offset: VIRTIO_MMIO_DEVICE_OFFSET, length: VIRTIO_MMIO_DEVICE_LEN }), // DEVICE_CFG
  ];

  readonly #bridge: VirtioSndPciBridgeLike;
  readonly #irqSink: IrqSink;

  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;
  #driverOkLogged = false;

  constructor(opts: { bridge: VirtioSndPciBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_MMIO_BAR0_SIZE) return 0;

    // Undefined offsets within BAR0 must read as 0 (contract v1).
    const defined =
      isInRange(off, size, VIRTIO_MMIO_COMMON_OFFSET, VIRTIO_MMIO_COMMON_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_NOTIFY_OFFSET, VIRTIO_MMIO_NOTIFY_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_ISR_OFFSET, VIRTIO_MMIO_ISR_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_DEVICE_OFFSET, VIRTIO_MMIO_DEVICE_LEN);
    if (!defined) return 0;

    let value = 0;
    try {
      value = this.#bridge.mmio_read(off >>> 0, size) >>> 0;
    } catch {
      value = 0;
    }

    // Reads from the ISR config region are read-to-ack and may deassert the IRQ.
    if (isInRange(off, size, VIRTIO_MMIO_ISR_OFFSET, VIRTIO_MMIO_ISR_LEN)) {
      this.#syncIrq();
    }

    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_MMIO_BAR0_SIZE) return;

    // Undefined offsets within BAR0 must ignore writes (contract v1).
    const defined =
      isInRange(off, size, VIRTIO_MMIO_COMMON_OFFSET, VIRTIO_MMIO_COMMON_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_NOTIFY_OFFSET, VIRTIO_MMIO_NOTIFY_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_ISR_OFFSET, VIRTIO_MMIO_ISR_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_DEVICE_OFFSET, VIRTIO_MMIO_DEVICE_LEN);
    if (!defined) return;

    try {
      this.#bridge.mmio_write(off >>> 0, size, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest IO
    }
    this.#syncIrq();
  }

  onPciCommandWrite(command: number): void {
    if (this.#destroyed) return;
    const cmd = command & 0xffff;
    this.#pciCommand = cmd;

    // Mirror into the WASM bridge so it can enforce PCI Bus Master Enable gating for DMA.
    const setCmd = this.#bridge.set_pci_command;
    if (typeof setCmd === "function") {
      try {
        setCmd.call(this.#bridge, cmd >>> 0);
      } catch {
        // ignore device errors during PCI config writes
      }
    }

    // Interrupt Disable bit can immediately drop INTx level.
    this.#syncIrq();
  }

  tick(_nowMs: number): void {
    if (this.#destroyed) return;
    // PCI Bus Master Enable (command bit 2) gates whether the device is allowed to DMA into guest
    // memory (virtqueue descriptor reads / used-ring writes / audio buffer fills).
    //
    // Mirror/gating note:
    // - Newer WASM builds can also enforce this via `set_pci_command`, but keep a wrapper-side gate
    //   so older builds remain correct and we avoid invoking poll unnecessarily.
    const busMasterEnabled = (this.#pciCommand & (1 << 2)) !== 0;
    if (busMasterEnabled) {
      try {
        this.#bridge.poll();
      } catch {
        // ignore device errors during tick
      }
    }
    this.#syncIrq();
  }

  driverOk(): boolean {
    let ok = false;
    try {
      ok = Boolean(this.#bridge.driver_ok());
    } catch {
      ok = false;
    }
    if (ok && !this.#driverOkLogged) {
      this.#driverOkLogged = true;
      console.info("[virtio-snd] driver_ok");
    }
    return ok;
  }

  setAudioRingBuffer(opts: {
    ringBuffer: SharedArrayBuffer | null;
    capacityFrames: number;
    channelCount: number;
    dstSampleRateHz: number;
  }): void {
    if (this.#destroyed) return;

    const ring = opts.ringBuffer;
    const capacityFrames = opts.capacityFrames >>> 0;
    const channelCount = opts.channelCount >>> 0;
    const dstSampleRateHz = opts.dstSampleRateHz >>> 0;

    if (dstSampleRateHz > 0) {
      try {
        this.#bridge.set_host_sample_rate_hz(dstSampleRateHz);
      } catch {
        // ignore invalid/missing rate plumbing
      }
    }

    try {
      // Rust expects an `Option<SharedArrayBuffer>`; pass `undefined` when detaching.
      this.#bridge.set_audio_ring_buffer(ring ?? undefined, capacityFrames, channelCount);
    } catch {
      // ignore invalid ring attachment
    }
  }

  setMicRingBuffer(ringBuffer: SharedArrayBuffer | null): void {
    if (this.#destroyed) return;
    try {
      this.#bridge.set_mic_ring_buffer(ringBuffer ?? undefined);
    } catch {
      // ignore
    }
  }

  setCaptureSampleRateHz(sampleRateHz: number): void {
    if (this.#destroyed) return;
    const sr = sampleRateHz >>> 0;
    if (!sr) return;
    try {
      this.#bridge.set_capture_sample_rate_hz(sr);
    } catch {
      // ignore
    }
  }

  destroy(): void {
    if (this.#destroyed) return;
    this.#destroyed = true;
    if (this.#irqLevel) {
      this.#irqSink.lowerIrq(this.irqLine);
      this.#irqLevel = false;
    }
    try {
      this.#bridge.free();
    } catch {
      // ignore
    }
  }

  #syncIrq(): void {
    let asserted = false;
    try {
      asserted = Boolean(this.#bridge.irq_asserted());
    } catch {
      asserted = false;
    }

    // Respect PCI command register Interrupt Disable bit (bit 10). When set, the device must not
    // assert INTx.
    if ((this.#pciCommand & (1 << 10)) !== 0) {
      asserted = false;
    }
    if (asserted === this.#irqLevel) return;
    this.#irqLevel = asserted;
    if (asserted) this.#irqSink.raiseIrq(this.irqLine);
    else this.#irqSink.lowerIrq(this.irqLine);
  }
}
