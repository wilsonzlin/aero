import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type UhciControllerBridgeLike = {
  io_read(offset: number, size: number): number;
  io_write(offset: number, size: number, value: number): void;
  /**
   * Optional hook for mirroring PCI command register writes into the underlying device model.
   *
   * When present, this can be used by WASM bridges to enforce DMA gating based on Bus Master Enable.
   */
  set_pci_command?(command: number): void;
  /**
   * Legacy 1ms stepping API (older WASM builds).
   */
  tick_1ms?: () => void;
  /**
   * Newer stepping APIs (batch + single-frame).
   */
  step_frames?: (frames: number) => void;
  step_frame?: () => void;
  irq_asserted(): boolean;
  free(): void;
};

const UHCI_CLASS_CODE = 0x0c_03_00;
const UHCI_IO_BAR_SIZE = 0x20;
const UHCI_IRQ_LINE = 0x0b;

// The IO worker tick runs at ~8ms; UHCI expects 1ms frames.
const UHCI_FRAME_MS = 1;
const UHCI_MAX_FRAMES_PER_TICK = 32;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Minimal UHCI PCI function backed by the WASM `UhciControllerBridge`.
 *
 * Exposes a single IO BAR (BAR4) containing the 0x20-byte UHCI register block and
 * advances the controller one 1ms frame at a time via {@link tick}.
 *
 * IRQ semantics:
 * UHCI uses PCI INTx, which is level-triggered. The WASM bridge exposes the current INTx level via
 * {@link UhciControllerBridgeLike.irq_asserted}, and this device forwards only level transitions
 * to the runtime's {@link IrqSink} (`raiseIrq` on 0→1, `lowerIrq` on 1→0).
 *
 * See `docs/irq-semantics.md`.
 */
export class UhciPciDevice implements PciDevice, TickableDevice {
  readonly name = "uhci";
  readonly vendorId = 0x8086;
  readonly deviceId = 0x7112; // PIIX4 UHCI (commonly supported by Windows in-box drivers)
  readonly classCode = UHCI_CLASS_CODE;
  readonly revisionId = 0x01;
  readonly irqLine = UHCI_IRQ_LINE;
  readonly bdf = { bus: 0, device: 1, function: 0 };

  // Intel PIIX3/4 place the UHCI I/O register window in BAR4 (offset 0x20).
  // Keep that layout so Windows' in-box UHCI driver can find the registers.
  readonly bars: ReadonlyArray<PciBar | null> = [null, null, null, null, { kind: "io", size: UHCI_IO_BAR_SIZE }, null];

  readonly #bridge: UhciControllerBridgeLike;
  readonly #irqSink: IrqSink;

  #lastTickMs: number | null = null;
  #accumulatedMs = 0;
  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: UhciControllerBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
  }

  ioRead(barIndex: number, offset: number, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 4) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);
    try {
      const value = this.#bridge.io_read(offset >>> 0, size >>> 0) >>> 0;
      return maskToSize(value, size);
    } catch {
      return defaultReadValue(size);
    }
  }

  ioWrite(barIndex: number, offset: number, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 4) return;
    if (size !== 1 && size !== 2 && size !== 4) return;
    try {
      this.#bridge.io_write(offset >>> 0, size >>> 0, maskToSize(value >>> 0, size));
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
    // guest memory (frame list / QH / TD traversal and data stage transfers).
    //
    // When bus mastering is disabled, drop elapsed time so we do not "catch up" later; DMA must not
    // occur until the guest enables BME.
    const busMasterEnabled = (this.#pciCommand & (1 << 2)) !== 0;
    if (!busMasterEnabled) {
      this.#accumulatedMs = 0;
      this.#syncIrq();
      return;
    }

    // Clamp catch-up work so long pauses (e.g. tab backgrounded) do not stall the worker.
    deltaMs = Math.min(deltaMs, UHCI_MAX_FRAMES_PER_TICK * UHCI_FRAME_MS);
    this.#accumulatedMs += deltaMs;

    let frames = Math.floor(this.#accumulatedMs / UHCI_FRAME_MS);
    frames = Math.min(frames, UHCI_MAX_FRAMES_PER_TICK);
    if (frames > 0) {
      const bridge = this.#bridge;
      try {
        if (typeof bridge.step_frames === "function") {
          bridge.step_frames(frames);
        } else if (typeof bridge.tick_1ms === "function") {
          for (let i = 0; i < frames; i++) bridge.tick_1ms();
        } else if (typeof bridge.step_frame === "function") {
          for (let i = 0; i < frames; i++) bridge.step_frame();
        }
      } catch {
        // ignore device errors during tick
      }
      this.#accumulatedMs -= frames * UHCI_FRAME_MS;
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
