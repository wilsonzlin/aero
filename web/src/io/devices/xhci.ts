import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciAddress, PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type XhciControllerBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  /**
   * Optional hook for mirroring PCI command register writes into the underlying device model.
   *
   * When present, this can be used by WASM bridges to enforce DMA gating based on Bus Master Enable.
   */
  set_pci_command?(command: number): void;
  /**
   * Optional stepping APIs.
   *
   * The JS wrapper treats one "frame" as 1ms of guest time (USB frame). WASM builds may expose
   * either a batched stepping API (`step_frames`) or an older per-frame API (`tick`), with
   * `step_frame`/`tick_1ms` as a slow fallback.
   */
  step_frames?: (frames: number) => void;
  tick?: (frames: number) => void;
  step_frame?: () => void;
  tick_1ms?: () => void;
  /**
   * Optional non-advancing progress hook.
   *
   * Treated as equivalent to `step_frames(0)` for legacy/alternate WASM builds.
   */
  poll?: () => void;
  irq_asserted(): boolean;
  free(): void;
};

const XHCI_CLASS_CODE = 0x0c_03_30;

// Match the native PCI identity in `crates/devices/src/pci/profile.rs`.
//
// We intentionally use QEMU's canonical xHCI PCI IDs (`1b36:000d`, "qemu-xhci") so modern guests
// bind their generic xHCI drivers by default. (Windows 7 has no inbox xHCI driver.)
const XHCI_VENDOR_ID = 0x1b36; // Red Hat/QEMU
const XHCI_DEVICE_ID = 0x000d; // qemu-xhci
const XHCI_REVISION_ID = 0x01;

// Typical xHCI register window size (CAP/OP/RT/DB blocks). Keep power-of-two for PCI BAR sizing.
const XHCI_MMIO_BAR_SIZE = 0x1_0000;
// Stable legacy IRQ line value reported in PCI config space (0x3C).
const XHCI_IRQ_LINE = 0x0b;

// The IO worker tick runs at ~8ms; treat xHCI stepping as 1ms frames and clamp catch-up work so
// the worker doesn't stall after long pauses.
const XHCI_FRAME_MS = 1;
const XHCI_MAX_FRAMES_PER_TICK = 32;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Minimal xHCI PCI function backed by the WASM `XhciControllerBridge`.
 *
 * Exposes BAR0 as a MMIO register window and advances the controller in {@link tick}.
 *
 * IRQ semantics:
 * xHCI uses PCI INTx, which is level-triggered. The WASM bridge exposes the current INTx level via
 * {@link XhciControllerBridgeLike.irq_asserted}, and this device forwards only level transitions
 * to the runtime's {@link IrqSink} (`raiseIrq` on 0→1, `lowerIrq` on 1→0).
 *
 * See `docs/irq-semantics.md`.
 */
export class XhciPciDevice implements PciDevice, TickableDevice {
  readonly name = "xhci";
  // Match the native xHCI PCI identity (qemu-xhci).
  readonly vendorId = XHCI_VENDOR_ID;
  readonly deviceId = XHCI_DEVICE_ID;
  // Match the canonical subsystem IDs used by QEMU's `qemu-xhci` device.
  //
  // `PciBus` currently defaults subsystem IDs to vendor/device IDs when omitted, but we set them
  // explicitly so the xHCI identity stays stable even if `PciBus` defaults change in the future.
  readonly subsystemVendorId = XHCI_VENDOR_ID;
  readonly subsystemDeviceId = XHCI_DEVICE_ID;
  readonly classCode = XHCI_CLASS_CODE;
  readonly revisionId = XHCI_REVISION_ID;
  readonly irqLine = XHCI_IRQ_LINE;
  readonly interruptPin = 0x01 as const; // INTA#
  // Keep a stable BDF so guest driver installation and snapshots are deterministic.
  bdf: PciAddress = { bus: 0, device: 0x0d, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio32", size: XHCI_MMIO_BAR_SIZE }, null, null, null, null, null];

  readonly #bridge: XhciControllerBridgeLike;
  readonly #irqSink: IrqSink;

  #lastTickMs: number | null = null;
  #accumulatedMs = 0;
  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: XhciControllerBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    // PCI Command bit 1: Memory Space Enable. When unset, the device must not respond to MMIO.
    if ((this.#pciCommand & (1 << 1)) === 0) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > XHCI_MMIO_BAR_SIZE) return defaultReadValue(size);

    let value = defaultReadValue(size);
    try {
      value = this.#bridge.mmio_read(off >>> 0, size >>> 0) >>> 0;
    } catch {
      value = defaultReadValue(size);
    }

    // Reads of status registers can clear the IRQ; keep the line level accurate.
    this.#syncIrq();
    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    // PCI Command bit 1: Memory Space Enable.
    if ((this.#pciCommand & (1 << 1)) === 0) return;

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > XHCI_MMIO_BAR_SIZE) return;

    try {
      this.#bridge.mmio_write(off >>> 0, size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest MMIO
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

  tick(nowMs: number): void {
    if (this.#destroyed) return;

    if (this.#lastTickMs === null) {
      this.#lastTickMs = nowMs;
      this.#syncIrq();
      return;
    }

    let deltaMs = nowMs - this.#lastTickMs;
    this.#lastTickMs = nowMs;

    if (!Number.isFinite(deltaMs) || deltaMs <= 0) {
      this.#syncIrq();
      return;
    }

    // PCI Bus Master Enable (command bit 2) gates whether the controller is allowed to DMA into
    // guest memory (TRBs/rings/event updates, transfer buffers, etc).
    //
    // Unlike DMA, internal controller time (e.g. port timers) continues to advance even when BME
    // is disabled. When the underlying bridge supports `set_pci_command`, it can enforce DMA
    // gating internally and we can keep advancing time while BME=0.
    //
    // For backwards compatibility with older WASM builds that may not implement BME gating, we
    // conservatively freeze time until BME is enabled *unless* `set_pci_command` is available.
    const busMasterEnabled = (this.#pciCommand & (1 << 2)) !== 0;
    if (!busMasterEnabled && typeof this.#bridge.set_pci_command !== "function") {
      this.#accumulatedMs = 0;
      this.#syncIrq();
      return;
    }

    // Clamp catch-up work so long pauses (e.g. tab backgrounded) do not stall the worker.
    deltaMs = Math.min(deltaMs, XHCI_MAX_FRAMES_PER_TICK * XHCI_FRAME_MS);
    this.#accumulatedMs += deltaMs;

    let frames = Math.floor(this.#accumulatedMs / XHCI_FRAME_MS);
    frames = Math.min(frames, XHCI_MAX_FRAMES_PER_TICK);
    if (frames > 0) {
      const bridge = this.#bridge;
      try {
        if (typeof bridge.step_frames === "function") {
          bridge.step_frames(frames);
        } else if (typeof bridge.tick === "function") {
          bridge.tick(frames);
        } else if (typeof bridge.step_frame === "function") {
          for (let i = 0; i < frames; i++) bridge.step_frame();
        } else if (typeof bridge.tick_1ms === "function") {
          for (let i = 0; i < frames; i++) bridge.tick_1ms();
        }
      } catch {
        // ignore device errors during tick
      }
      this.#accumulatedMs -= frames * XHCI_FRAME_MS;
    }

    // Some WASM bridges expose a `poll()` hook that performs non-time-advancing work (e.g. draining
    // pending completions). Treat it as a legacy alias for `step_frames(0)`.
    const poll = this.#bridge.poll;
    if (busMasterEnabled && typeof poll === "function") {
      try {
        poll.call(this.#bridge);
      } catch {
        // ignore device errors during poll
      }
    }

    this.#syncIrq();
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
