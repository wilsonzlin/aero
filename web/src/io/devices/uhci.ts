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

const IS_DEV = (import.meta as { env?: { DEV?: boolean } }).env?.DEV === true;

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
  readonly #ioReadFn: (offset: number, size: number) => number;
  readonly #ioWriteFn: (offset: number, size: number, value: number) => void;
  readonly #stepFramesFn: ((frames: number) => void) | null;
  readonly #tick1msFn: (() => void) | null;
  readonly #stepFrameFn: (() => void) | null;
  readonly #irqAssertedFn: () => boolean;
  readonly #freeFn: () => void;
  readonly #setPciCommandFn: ((command: number) => void) | null;
  readonly #irqSink: IrqSink;

  #lastTickMs: number | null = null;
  #accumulatedMs = 0;
  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: UhciControllerBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;

    // Backwards compatibility: accept both snake_case and camelCase exports and always invoke
    // extracted methods via `.call(bridge, ...)` to avoid wasm-bindgen `this` binding pitfalls.
    const bridgeAny = opts.bridge as unknown as Record<string, unknown>;
    const ioRead = bridgeAny.io_read ?? bridgeAny.ioRead;
    const ioWrite = bridgeAny.io_write ?? bridgeAny.ioWrite;
    const irqAsserted = bridgeAny.irq_asserted ?? bridgeAny.irqAsserted ?? bridgeAny.irq_level ?? bridgeAny.irqLevel;
    const free = bridgeAny.free;

    if (typeof ioRead !== "function" || typeof ioWrite !== "function") {
      throw new Error("UHCI bridge missing io_read/ioRead or io_write/ioWrite exports.");
    }
    if (typeof irqAsserted !== "function") {
      throw new Error("UHCI bridge missing irq_asserted/irqAsserted or irq_level/irqLevel export.");
    }
    if (typeof free !== "function") {
      throw new Error("UHCI bridge missing free() export.");
    }

    this.#ioReadFn = ioRead as (offset: number, size: number) => number;
    this.#ioWriteFn = ioWrite as (offset: number, size: number, value: number) => void;
    this.#irqAssertedFn = irqAsserted as () => boolean;
    this.#freeFn = free as () => void;

    const stepFrames = bridgeAny.step_frames ?? bridgeAny.stepFrames;
    this.#stepFramesFn = typeof stepFrames === "function" ? (stepFrames as (frames: number) => void) : null;
    const tick1ms = bridgeAny.tick_1ms ?? bridgeAny.tick1ms ?? bridgeAny.tick1Ms;
    this.#tick1msFn = typeof tick1ms === "function" ? (tick1ms as () => void) : null;
    const stepFrame = bridgeAny.step_frame ?? bridgeAny.stepFrame;
    this.#stepFrameFn = typeof stepFrame === "function" ? (stepFrame as () => void) : null;

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
    } catch (err) {
      if (IS_DEV) {
        try {
          const message = err instanceof Error ? err.message : String(err);
          const post = (globalThis as unknown as { postMessage?: unknown }).postMessage;
          if (typeof post === "function") {
            post.call(globalThis, {
              type: "uhci.io.error",
              op: "read",
              barIndex,
              offset: offset >>> 0,
              size: size >>> 0,
              message,
            });
          }
        } catch {
          // ignore
        }
      }
      return defaultReadValue(size);
    }
  }

  ioWrite(barIndex: number, offset: number, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 4) return;
    if (size !== 1 && size !== 2 && size !== 4) return;
    try {
      this.#ioWriteFn.call(this.#bridge, offset >>> 0, size >>> 0, maskToSize(value >>> 0, size));
    } catch (err) {
      if (IS_DEV) {
        try {
          const message = err instanceof Error ? err.message : String(err);
          const post = (globalThis as unknown as { postMessage?: unknown }).postMessage;
          if (typeof post === "function") {
            post.call(globalThis, {
              type: "uhci.io.error",
              op: "write",
              barIndex,
              offset: offset >>> 0,
              size: size >>> 0,
              value: value >>> 0,
              message,
            });
          }
        } catch {
          // ignore
        }
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
        if (this.#stepFramesFn) {
          this.#stepFramesFn.call(this.#bridge, frames);
        } else if (this.#tick1msFn) {
          for (let i = 0; i < frames; i++) this.#tick1msFn.call(this.#bridge);
        } else if (this.#stepFrameFn) {
          for (let i = 0; i < frames; i++) this.#stepFrameFn.call(this.#bridge);
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
