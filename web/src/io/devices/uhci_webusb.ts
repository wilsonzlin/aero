import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type WebUsbUhciBridgeLike = {
  io_read(offset: number, size: number): number;
  io_write(offset: number, size: number, value: number): void;
  step_frames(frames: number): void;
  irq_level(): boolean;
  /**
   * Optional hook for mirroring PCI command register writes into the underlying device model.
   *
   * When present, this can be used by WASM bridges to enforce DMA gating based on Bus Master Enable.
   */
  set_pci_command?(command: number): void;
  free(): void;
};

const UHCI_IO_BAR_SIZE = 0x20;
// UHCI expects 1ms frames.
const UHCI_FRAME_MS = 1;
const UHCI_MAX_FRAMES_PER_TICK = 32;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Intel PIIX3 UHCI controller (PCI function) that forwards register accesses into WASM.
 *
 * BAR4 is an I/O range with the UHCI register block (0x20 bytes). The actual controller
 * logic (TD/QH traversal, guest RAM reads/writes, passthrough device) lives in Rust
 * (`WebUsbUhciBridge`).
 *
 * IRQ semantics:
 * This device uses PCI INTx, which is level-triggered. The WASM bridge exposes the current line
 * level via `irq_level()`, and we forward only level transitions through {@link IrqSink}.
 *
 * See `docs/irq-semantics.md`.
 */
export class UhciWebUsbPciDevice implements PciDevice, TickableDevice {
  readonly name = "uhci_webusb";
  readonly vendorId = 0x8086;
  readonly deviceId = 0x7020;
  readonly classCode = 0x0c_03_00; // USB controller (UHCI)
  readonly irqLine = 0x0b;

  readonly bars: ReadonlyArray<PciBar | null> = [null, null, null, null, { kind: "io", size: UHCI_IO_BAR_SIZE }, null];

  readonly #bridge: WebUsbUhciBridgeLike;
  readonly #ioReadFn: (offset: number, size: number) => number;
  readonly #ioWriteFn: (offset: number, size: number, value: number) => void;
  readonly #stepFramesFn: (frames: number) => void;
  readonly #irqLevelFn: () => boolean;
  readonly #freeFn: () => void;
  readonly #setPciCommandFn: ((command: number) => void) | null;
  readonly #irqSink: IrqSink;

  #lastTickMs: number | null = null;
  #accumulatedMs = 0;
  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: WebUsbUhciBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;

    // Backwards compatibility: accept both snake_case and camelCase exports and always invoke
    // extracted methods via `.call(bridge, ...)` to avoid wasm-bindgen `this` binding pitfalls.
    const bridgeAny = opts.bridge as unknown as Record<string, unknown>;
    const ioRead = bridgeAny.io_read ?? bridgeAny.ioRead;
    const ioWrite = bridgeAny.io_write ?? bridgeAny.ioWrite;
    const stepFrames = bridgeAny.step_frames ?? bridgeAny.stepFrames;
    const irqLevel = bridgeAny.irq_level ?? bridgeAny.irqLevel;
    const free = bridgeAny.free;

    if (typeof ioRead !== "function" || typeof ioWrite !== "function") {
      throw new Error("WebUsbUhciBridge missing io_read/ioRead or io_write/ioWrite exports.");
    }
    if (typeof stepFrames !== "function") {
      throw new Error("WebUsbUhciBridge missing step_frames/stepFrames export.");
    }
    if (typeof irqLevel !== "function") {
      throw new Error("WebUsbUhciBridge missing irq_level/irqLevel export.");
    }
    if (typeof free !== "function") {
      throw new Error("WebUsbUhciBridge missing free() export.");
    }

    this.#ioReadFn = ioRead as (offset: number, size: number) => number;
    this.#ioWriteFn = ioWrite as (offset: number, size: number, value: number) => void;
    this.#stepFramesFn = stepFrames as (frames: number) => void;
    this.#irqLevelFn = irqLevel as () => boolean;
    this.#freeFn = free as () => void;

    const setCmd = bridgeAny.set_pci_command ?? bridgeAny.setPciCommand;
    this.#setPciCommandFn = typeof setCmd === "function" ? (setCmd as (command: number) => void) : null;
  }

  ioRead(barIndex: number, offset: number, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 4) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);
    try {
      const value = this.#ioReadFn.call(this.#bridge, offset >>> 0, size >>> 0) >>> 0;
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
      this.#ioWriteFn.call(this.#bridge, offset >>> 0, size >>> 0, maskToSize(value, size));
    } catch {
      // ignore
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
    // UHCI internal time (frame counter, port timers) should keep progressing even when BME=0; when
    // the underlying bridge supports `set_pci_command`, it can enforce DMA gating internally and
    // we can keep advancing time while BME is disabled.
    //
    // For backwards compatibility with older WASM builds that may not implement DMA gating, we
    // conservatively freeze time until BME is enabled *unless* `set_pci_command` is available.
    const busMasterEnabled = (this.#pciCommand & (1 << 2)) !== 0;
    if (!busMasterEnabled && !this.#setPciCommandFn) {
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
      try {
        this.#stepFramesFn.call(this.#bridge, frames);
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
      this.#freeFn.call(this.#bridge);
    } catch {
      // ignore
    }
  }

  #syncIrq(): void {
    let asserted = false;
    try {
      asserted = Boolean(this.#irqLevelFn.call(this.#bridge));
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
