import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type HdaControllerBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  step_frames(frames: number): void;
  irq_level(): boolean;
  set_mic_ring_buffer(sab?: SharedArrayBuffer): void;
  set_capture_sample_rate_hz(sampleRateHz: number): void;
  free(): void;
};

const HDA_CLASS_CODE = 0x04_03_00;
const HDA_MMIO_BAR_SIZE = 0x4000;
const HDA_IRQ_LINE = 0x0b;
const HDA_OUTPUT_RATE_HZ = 48_000;

// Avoid pathological catch-up work if the tab is backgrounded or the worker stalls.
const HDA_MAX_DELTA_MS = 100;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Minimal Intel HD Audio PCI function backed by a WASM-side `HdaControllerBridge`.
 *
 * Exposes BAR0 MMIO (0x4000 bytes) and advances the device model on the IO worker tick.
 *
 * Microphone ring-buffer attachment is forwarded to the WASM bridge via {@link setMicRingBuffer}.
 */
export class HdaPciDevice implements PciDevice, TickableDevice {
  readonly name = "hda";
  readonly vendorId = 0x8086;
  readonly deviceId = 0x2668; // Intel ICH6 HDA controller
  readonly classCode = HDA_CLASS_CODE;
  readonly revisionId = 0x01;
  readonly irqLine = HDA_IRQ_LINE;
  readonly bdf = { bus: 0, device: 4, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio32", size: HDA_MMIO_BAR_SIZE }, null, null, null, null, null];

  readonly #bridge: HdaControllerBridgeLike;
  readonly #irqSink: IrqSink;

  #lastTickMs: number | null = null;
  #accumulatedMs = 0;
  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: HdaControllerBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    try {
      const value = this.#bridge.mmio_read(Number(offset), size >>> 0) >>> 0;
      return maskToSize(value, size);
    } catch {
      return defaultReadValue(size);
    }
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;
    try {
      this.#bridge.mmio_write(Number(offset), size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest IO
    }
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

    deltaMs = Math.min(deltaMs, HDA_MAX_DELTA_MS);
    this.#accumulatedMs += deltaMs;

    let frames = Math.floor((this.#accumulatedMs * HDA_OUTPUT_RATE_HZ) / 1000);
    // Bound per-tick stepping so extremely slow ticks cannot request absurdly large buffers.
    frames = Math.min(frames, Math.floor((HDA_MAX_DELTA_MS * HDA_OUTPUT_RATE_HZ) / 1000));
    if (frames > 0) {
      try {
        this.#bridge.step_frames(frames >>> 0);
      } catch {
        // ignore device errors during tick
      }
      this.#accumulatedMs -= (frames * 1000) / HDA_OUTPUT_RATE_HZ;
    }

    this.#syncIrq();
  }

  setMicRingBuffer(sab: SharedArrayBuffer | null): void {
    if (this.#destroyed) return;
    try {
      if (sab) this.#bridge.set_mic_ring_buffer(sab);
      else this.#bridge.set_mic_ring_buffer(undefined);
    } catch {
      // ignore
    }
  }

  setCaptureSampleRateHz(sampleRateHz: number): void {
    if (this.#destroyed) return;
    const sr = sampleRateHz >>> 0;
    if (sr === 0) return;
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
      asserted = Boolean(this.#bridge.irq_level());
    } catch {
      asserted = false;
    }
    if (asserted === this.#irqLevel) return;
    this.#irqLevel = asserted;
    if (asserted) this.#irqSink.raiseIrq(this.irqLine);
    else this.#irqSink.lowerIrq(this.irqLine);
  }
}
