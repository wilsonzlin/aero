import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciCapability, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioSndPciBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  /**
   * Legacy virtio-pci (0.9) I/O port register block accessors (BAR2).
   *
   * Newer WASM builds expose these as `legacy_io_read`/`legacy_io_write`. Older builds used
   * `io_read`/`io_write` and those names are retained for back-compat.
   */
  legacy_io_read?(offset: number, size: number): number;
  legacy_io_write?(offset: number, size: number, value: number): void;
  io_read?(offset: number, size: number): number;
  io_write?(offset: number, size: number, value: number): void;
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
  // Optional snapshot hooks.
  save_state?: () => Uint8Array;
  snapshot_state?: () => Uint8Array;
  load_state?: (bytes: Uint8Array) => void;
  restore_state?: (bytes: Uint8Array) => void;
  free(): void;
};

export type VirtioSndPciMode = "modern" | "transitional" | "legacy";

const VIRTIO_VENDOR_ID = 0x1af4;
// Modern virtio-pci device ID space is 0x1040 + <virtio device type>.
const VIRTIO_SND_MODERN_DEVICE_ID = 0x1059;
// Transitional virtio-pci device IDs are 0x1000 + (type - 1). virtio-snd type is 25 (0x19).
const VIRTIO_SND_TRANSITIONAL_DEVICE_ID = 0x1018;
const VIRTIO_SND_SUBSYSTEM_DEVICE_ID = 0x0019;
const VIRTIO_SND_CLASS_CODE = 0x04_01_00;
const VIRTIO_CONTRACT_REVISION_ID = 0x01;

// BAR0 size in the Aero Windows 7 virtio contract v1 (see docs/windows7-virtio-driver-contract.md).
const VIRTIO_MMIO_BAR0_SIZE = 0x4000;
// Keep in sync with `crates/aero-virtio/src/pci.rs` (`bar2_size` when legacy I/O is enabled).
const VIRTIO_LEGACY_IO_BAR2_SIZE = 0x100;

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
 * Virtio-snd PCI function (virtio-pci modern/transitional/legacy transport) backed by the WASM `VirtioSndPciBridge`.
 *
 * Exposes:
 * - BAR0: 64-bit MMIO BAR, size 0x4000 (Aero Windows 7 virtio contract v1 modern layout).
 * - BAR2 (transitional/legacy modes): legacy I/O port register block, size 0x100.
 */
export class VirtioSndPciDevice implements PciDevice, TickableDevice {
  readonly name = "virtio_snd";
  readonly vendorId = VIRTIO_VENDOR_ID;
  readonly deviceId: number;
  readonly subsystemVendorId = VIRTIO_VENDOR_ID;
  readonly subsystemId = VIRTIO_SND_SUBSYSTEM_DEVICE_ID;
  readonly classCode = VIRTIO_SND_CLASS_CODE;
  readonly revisionId = VIRTIO_CONTRACT_REVISION_ID;
  readonly irqLine = VIRTIO_SND_IRQ_LINE;
  readonly interruptPin = 1 as const;
  // Keep the canonical PCI address consistent with the docs + Rust PCI profile
  // (`docs/pci-device-compatibility.md`, `crates/devices/src/pci/profile.rs`).
  readonly bdf = { bus: 0, device: 11, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null>;
  readonly capabilities: ReadonlyArray<PciCapability>;

  readonly #bridge: VirtioSndPciBridgeLike;
  readonly #mmioReadFn: (offset: number, size: number) => number;
  readonly #mmioWriteFn: (offset: number, size: number, value: number) => void;
  readonly #pollFn: () => void;
  readonly #driverOkFn: () => boolean;
  readonly #irqAssertedFn: () => boolean;
  readonly #setAudioRingBufferFn: (ringSab: SharedArrayBuffer | null | undefined, capacityFrames: number, channelCount: number) => void;
  readonly #setHostSampleRateHzFn: (rate: number) => void;
  readonly #setMicRingBufferFn: (ringSab?: SharedArrayBuffer | null) => void;
  readonly #setCaptureSampleRateHzFn: (rate: number) => void;
  readonly #freeFn: () => void;
  readonly #setPciCommandFn: ((command: number) => void) | null;
  readonly #irqSink: IrqSink;
  readonly #mode: VirtioSndPciMode;

  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;
  #driverOkLogged = false;
  #micSampleRateHz = 0;

  constructor(opts: { bridge: VirtioSndPciBridgeLike; irqSink: IrqSink; mode?: VirtioSndPciMode }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
    this.#mode = opts.mode ?? "modern";

    // Backwards compatibility: accept both snake_case and camelCase exports and call extracted
    // methods via `.call(bridge, ...)` to avoid wasm-bindgen `this` binding pitfalls.
    const bridgeAny = opts.bridge as unknown as Record<string, unknown>;
    const mmioRead = bridgeAny.mmio_read ?? bridgeAny.mmioRead;
    const mmioWrite = bridgeAny.mmio_write ?? bridgeAny.mmioWrite;
    const poll = bridgeAny.poll;
    const driverOk = bridgeAny.driver_ok ?? bridgeAny.driverOk;
    const irqAsserted = bridgeAny.irq_asserted ?? bridgeAny.irqAsserted;
    const setAudioRing = bridgeAny.set_audio_ring_buffer ?? bridgeAny.setAudioRingBuffer;
    const setHostRate = bridgeAny.set_host_sample_rate_hz ?? bridgeAny.setHostSampleRateHz;
    const setMicRing = bridgeAny.set_mic_ring_buffer ?? bridgeAny.setMicRingBuffer;
    const setCaptureRate = bridgeAny.set_capture_sample_rate_hz ?? bridgeAny.setCaptureSampleRateHz;
    const free = bridgeAny.free;

    if (typeof mmioRead !== "function" || typeof mmioWrite !== "function") {
      throw new Error("virtio-snd bridge missing mmio_read/mmioRead or mmio_write/mmioWrite exports.");
    }
    if (typeof poll !== "function") {
      throw new Error("virtio-snd bridge missing poll() export.");
    }
    if (typeof driverOk !== "function") {
      throw new Error("virtio-snd bridge missing driver_ok/driverOk export.");
    }
    if (typeof irqAsserted !== "function") {
      throw new Error("virtio-snd bridge missing irq_asserted/irqAsserted export.");
    }
    if (typeof setAudioRing !== "function") {
      throw new Error("virtio-snd bridge missing set_audio_ring_buffer/setAudioRingBuffer export.");
    }
    if (typeof setHostRate !== "function") {
      throw new Error("virtio-snd bridge missing set_host_sample_rate_hz/setHostSampleRateHz export.");
    }
    if (typeof setMicRing !== "function") {
      throw new Error("virtio-snd bridge missing set_mic_ring_buffer/setMicRingBuffer export.");
    }
    if (typeof setCaptureRate !== "function") {
      throw new Error("virtio-snd bridge missing set_capture_sample_rate_hz/setCaptureSampleRateHz export.");
    }
    if (typeof free !== "function") {
      throw new Error("virtio-snd bridge missing free() export.");
    }

    this.#mmioReadFn = mmioRead as (offset: number, size: number) => number;
    this.#mmioWriteFn = mmioWrite as (offset: number, size: number, value: number) => void;
    this.#pollFn = poll as () => void;
    this.#driverOkFn = driverOk as () => boolean;
    this.#irqAssertedFn = irqAsserted as () => boolean;
    this.#setAudioRingBufferFn =
      setAudioRing as (ringSab: SharedArrayBuffer | null | undefined, capacityFrames: number, channelCount: number) => void;
    this.#setHostSampleRateHzFn = setHostRate as (rate: number) => void;
    this.#setMicRingBufferFn = setMicRing as (ringSab?: SharedArrayBuffer | null) => void;
    this.#setCaptureSampleRateHzFn = setCaptureRate as (rate: number) => void;
    this.#freeFn = free as () => void;

    const setCmd = bridgeAny.set_pci_command ?? bridgeAny.setPciCommand;
    this.#setPciCommandFn = typeof setCmd === "function" ? (setCmd as (command: number) => void) : null;

    const caps: ReadonlyArray<PciCapability> = [
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

    // Legacy-only mode intentionally disables modern virtio-pci capabilities so guests take the
    // virtio 0.9 I/O-port transport path.
    this.capabilities = this.#mode === "legacy" ? [] : caps;

    this.deviceId = this.#mode === "modern" ? VIRTIO_SND_MODERN_DEVICE_ID : VIRTIO_SND_TRANSITIONAL_DEVICE_ID;
    this.bars =
      this.#mode === "modern"
        ? [{ kind: "mmio64", size: VIRTIO_MMIO_BAR0_SIZE }, null, null, null, null, null]
        : [{ kind: "mmio64", size: VIRTIO_MMIO_BAR0_SIZE }, null, { kind: "io", size: VIRTIO_LEGACY_IO_BAR2_SIZE }, null, null, null];
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
      value = this.#mmioReadFn.call(this.#bridge, off >>> 0, size) >>> 0;
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
      this.#mmioWriteFn.call(this.#bridge, off >>> 0, size, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest IO
    }
    this.#syncIrq();
  }

  ioRead(barIndex: number, offset: number, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 2) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);
    if (this.#mode === "modern") return defaultReadValue(size);

    const off = offset >>> 0;
    if (off + size > VIRTIO_LEGACY_IO_BAR2_SIZE) return defaultReadValue(size);

    const bridge = this.#bridge as unknown as Record<string, unknown>;
    const fn =
      (typeof bridge.legacy_io_read === "function"
        ? (bridge.legacy_io_read as (offset: number, size: number) => number)
        : typeof bridge.legacyIoRead === "function"
          ? (bridge.legacyIoRead as (offset: number, size: number) => number)
          : typeof bridge.io_read === "function"
            ? (bridge.io_read as (offset: number, size: number) => number)
            : typeof bridge.ioRead === "function"
              ? (bridge.ioRead as (offset: number, size: number) => number)
              : undefined) ?? undefined;
    if (typeof fn !== "function") return defaultReadValue(size);

    let value: number;
    try {
      value = fn.call(this.#bridge, off, size) >>> 0;
    } catch {
      value = defaultReadValue(size);
    }
    this.#syncIrq();
    return maskToSize(value, size);
  }

  ioWrite(barIndex: number, offset: number, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 2) return;
    if (size !== 1 && size !== 2 && size !== 4) return;
    if (this.#mode === "modern") return;

    const off = offset >>> 0;
    if (off + size > VIRTIO_LEGACY_IO_BAR2_SIZE) return;

    const bridge = this.#bridge as unknown as Record<string, unknown>;
    const fn =
      (typeof bridge.legacy_io_write === "function"
        ? (bridge.legacy_io_write as (offset: number, size: number, value: number) => void)
        : typeof bridge.legacyIoWrite === "function"
          ? (bridge.legacyIoWrite as (offset: number, size: number, value: number) => void)
          : typeof bridge.io_write === "function"
            ? (bridge.io_write as (offset: number, size: number, value: number) => void)
            : typeof bridge.ioWrite === "function"
              ? (bridge.ioWrite as (offset: number, size: number, value: number) => void)
              : undefined) ?? undefined;
    if (typeof fn === "function") {
      try {
        fn.call(this.#bridge, off, size, maskToSize(value >>> 0, size));
      } catch {
        // ignore device errors during guest IO
      }
    }
    this.#syncIrq();
  }

  onPciCommandWrite(command: number): void {
    if (this.#destroyed) return;
    const cmd = command & 0xffff;
    this.#pciCommand = cmd;

    // Mirror into the WASM bridge so it can enforce PCI Bus Master Enable gating for DMA.
    const setCmd = this.#setPciCommandFn;
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
        this.#pollFn.call(this.#bridge);
      } catch {
        // ignore device errors during tick
      }
    }
    this.#syncIrq();
  }

  driverOk(): boolean {
    let ok = false;
    try {
      ok = Boolean(this.#driverOkFn.call(this.#bridge));
    } catch {
      ok = false;
    }
    if (ok && !this.#driverOkLogged) {
      this.#driverOkLogged = true;
      console.info("[virtio-snd] driver_ok");
    }
    return ok;
  }

  canSaveState(): boolean {
    const b = this.#bridge as unknown as Record<string, unknown>;
    return (
      typeof b["save_state"] === "function" ||
      typeof b["snapshot_state"] === "function" ||
      typeof b["saveState"] === "function" ||
      typeof b["snapshotState"] === "function"
    );
  }

  canLoadState(): boolean {
    const b = this.#bridge as unknown as Record<string, unknown>;
    return (
      typeof b["load_state"] === "function" ||
      typeof b["restore_state"] === "function" ||
      typeof b["loadState"] === "function" ||
      typeof b["restoreState"] === "function"
    );
  }

  saveState(): Uint8Array | null {
    if (this.#destroyed) return null;
    const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
    const save = bridgeAny.save_state ?? bridgeAny.snapshot_state ?? bridgeAny.saveState ?? bridgeAny.snapshotState;
    if (typeof save !== "function") return null;
    try {
      const bytes = (save as () => unknown).call(this.#bridge) as unknown;
      if (bytes instanceof Uint8Array) return bytes;
    } catch {
      // ignore
    }
    return null;
  }

  loadState(bytes: Uint8Array): boolean {
    if (this.#destroyed) return false;
    const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
    const load = bridgeAny.load_state ?? bridgeAny.restore_state ?? bridgeAny.loadState ?? bridgeAny.restoreState;
    if (typeof load !== "function") return false;
    try {
      (load as (bytes: Uint8Array) => unknown).call(this.#bridge, bytes);
      this.#syncIrq();
      return true;
    } catch {
      return false;
    }
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
        this.#setHostSampleRateHzFn.call(this.#bridge, dstSampleRateHz);

        // The Rust virtio-snd device can track its capture sample rate to the host/output sample
        // rate until a distinct capture rate is configured. Reassert the configured capture rate
        // after updating the host/output rate so mic capture stays pinned to the host mic
        // AudioContext sample rate.
        const micSr = this.#micSampleRateHz >>> 0;
        if (micSr > 0) {
          try {
            this.#setCaptureSampleRateHzFn.call(this.#bridge, micSr);
          } catch {
            // ignore
          }
        }
      } catch {
        // ignore invalid/missing rate plumbing
      }
    }

    try {
      // Rust expects an `Option<SharedArrayBuffer>`; pass `undefined` when detaching.
      this.#setAudioRingBufferFn.call(this.#bridge, ring ?? undefined, capacityFrames, channelCount);
    } catch {
      // ignore invalid ring attachment
    }
  }

  setMicRingBuffer(ringBuffer: SharedArrayBuffer | null): void {
    if (this.#destroyed) return;
    try {
      this.#setMicRingBufferFn.call(this.#bridge, ringBuffer ?? undefined);
    } catch {
      // ignore
    }
  }

  setCaptureSampleRateHz(sampleRateHz: number): void {
    if (this.#destroyed) return;
    const sr = sampleRateHz >>> 0;
    if (!sr) return;
    this.#micSampleRateHz = sr;
    try {
      this.#setCaptureSampleRateHzFn.call(this.#bridge, sr);
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
      this.#freeFn.call(this.#bridge);
    } catch {
      // ignore
    }
  }

  #syncIrq(): void {
    let asserted = false;
    try {
      asserted = Boolean(this.#irqAssertedFn.call(this.#bridge));
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
