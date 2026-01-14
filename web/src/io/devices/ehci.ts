import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type EhciControllerBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  /**
   * Advance the controller by the given number of 1ms USB frames.
   */
  step_frames(frames: number): void;
  /**
   * Optional hook for mirroring PCI command register writes into the underlying device model.
   *
   * When present, this can be used by WASM bridges to enforce DMA gating based on Bus Master Enable
   * and to observe INTx disable state.
   */
  set_pci_command?: (command: number) => void;
  /**
   * Current PCI INTx line level (level-triggered).
   */
  irq_asserted(): boolean;
  free(): void;
};

// USB 2.0 EHCI controller class code (USB, EHCI programming interface).
const EHCI_CLASS_CODE = 0x0c_03_20;

// EHCI MMIO register window is typically 4KiB.
const EHCI_MMIO_BAR_SIZE = 0x1000;

// Keep a stable legacy IRQ line value reported in PCI config space (0x3C). This is writable by the
// guest and PCI INTx lines are refcounted, so sharing is OK.
const EHCI_IRQ_LINE = 0x0b;

// The IO worker tick runs at ~8ms; the EHCI model expects 1ms frames.
const EHCI_FRAME_MS = 1;
const EHCI_MAX_FRAMES_PER_TICK = 32;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Minimal EHCI PCI function backed by the WASM `EhciControllerBridge`.
 *
 * Exposes BAR0 (MMIO32, 4KiB) and advances the controller in {@link tick} by converting IO worker
 * wall-clock ticks (~8ms) into 1ms USB frames.
 *
 * IRQ semantics:
 * EHCI uses PCI INTx, which is level-triggered. The WASM bridge exposes the current INTx level via
 * {@link EhciControllerBridgeLike.irq_asserted}, and this device forwards only level transitions
 * to the runtime's {@link IrqSink} (`raiseIrq` on 0→1, `lowerIrq` on 1→0).
 *
 * See `docs/irq-semantics.md`.
 * See `docs/usb-ehci.md` for EHCI model scope and bring-up status.
 */
export class EhciPciDevice implements PciDevice, TickableDevice {
  readonly name = "ehci";

  // PCI identity: Intel ICH9-style EHCI controller.
  readonly vendorId = 0x8086;
  readonly deviceId = 0x293a;
  readonly classCode = EHCI_CLASS_CODE;
  readonly revisionId = 0x02;
  readonly irqLine = EHCI_IRQ_LINE;
  readonly interruptPin = 0x01 as const; // INTA#

  // Keep the PCI address aligned with the canonical Rust profile:
  // `aero_devices::pci::profile::USB_EHCI_ICH9` (00:12.0).
  readonly bdf = { bus: 0, device: 0x12, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio32", size: EHCI_MMIO_BAR_SIZE }, null, null, null, null, null];

  readonly #bridge: EhciControllerBridgeLike;
  readonly #irqSink: IrqSink;

  #lastTickMs: number | null = null;
  #accumulatedMs = 0;
  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: EhciControllerBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > EHCI_MMIO_BAR_SIZE) return defaultReadValue(size);

    let value = defaultReadValue(size);
    try {
      value = this.#bridge.mmio_read(off >>> 0, size >>> 0) >>> 0;
    } catch {
      value = defaultReadValue(size);
    }

    // Reads of status registers may clear the IRQ; keep the line level accurate.
    this.#syncIrq();
    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > EHCI_MMIO_BAR_SIZE) return;

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
    // guest memory (schedule traversal + data transfers).
    //
    // EHCI internal time (FRINDEX, root hub port timers) should keep progressing even when BME=0;
    // when the underlying bridge supports `set_pci_command`, it can enforce DMA gating internally
    // and we can keep advancing time while BME is disabled.
    //
    // For backwards compatibility with older WASM builds that may not implement DMA gating, we
    // conservatively freeze time until BME is enabled *unless* `set_pci_command` is available.
    const busMasterEnabled = (this.#pciCommand & (1 << 2)) !== 0;
    if (!busMasterEnabled && typeof this.#bridge.set_pci_command !== "function") {
      this.#accumulatedMs = 0;
      this.#syncIrq();
      return;
    }

    // Clamp catch-up work so long pauses (e.g. tab backgrounded) do not stall the worker.
    deltaMs = Math.min(deltaMs, EHCI_MAX_FRAMES_PER_TICK * EHCI_FRAME_MS);
    this.#accumulatedMs += deltaMs;

    let frames = Math.floor(this.#accumulatedMs / EHCI_FRAME_MS);
    frames = Math.min(frames, EHCI_MAX_FRAMES_PER_TICK);
    if (frames > 0) {
      try {
        this.#bridge.step_frames(frames);
      } catch {
        // ignore device errors during tick
      }
      this.#accumulatedMs -= frames * EHCI_FRAME_MS;
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
