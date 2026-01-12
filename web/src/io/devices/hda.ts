import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";
import { AudioFrameClock, perfNowMsToNs } from "../../audio/audio_frame_clock";

export type HdaControllerBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  step_frames(frames: number): void;
  irq_level(): boolean;
  set_mic_ring_buffer(sab?: SharedArrayBuffer): void;
  set_capture_sample_rate_hz(sampleRateHz: number): void;
  /**
   * Optional audio output ring attachment helpers (newer WASM builds).
   *
   * When attached, the WASM-side HDA controller writes interleaved stereo `f32`
   * into the shared AudioWorklet ring buffer.
   */
  attach_audio_ring?: (ringSab: SharedArrayBuffer, capacityFrames: number, channelCount: number) => void;
  detach_audio_ring?: () => void;
  /**
   * Optional host sample-rate plumbing (newer WASM builds).
   *
   * Must match the output AudioContext's `sampleRate` when streaming into an
   * AudioWorklet ring buffer.
   */
  set_output_rate_hz?: (rate: number) => void;
  free(): void;
};

const HDA_CLASS_CODE = 0x04_03_00;
const HDA_MMIO_BAR_SIZE = 0x4000;
const HDA_IRQ_LINE = 0x0b;
const HDA_DEFAULT_OUTPUT_RATE_HZ = 48_000;

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

  #clock: AudioFrameClock | null = null;
  #outputRateHz = HDA_DEFAULT_OUTPUT_RATE_HZ;
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

    let nowNs: bigint;
    try {
      nowNs = perfNowMsToNs(nowMs);
    } catch {
      this.#syncIrq();
      return;
    }

    if (!this.#clock) {
      this.#clock = new AudioFrameClock(this.#outputRateHz, nowNs);
      this.#syncIrq();
      return;
    }

    // Avoid pathological catch-up work if the tab is backgrounded or the worker stalls.
    // Match the legacy behavior of dropping excess time beyond `HDA_MAX_DELTA_MS`.
    const clock = this.#clock;
    const lastNs = clock.lastTimeNs;
    if (nowNs <= lastNs) {
      this.#syncIrq();
      return;
    }

    const maxDeltaNs = BigInt(HDA_MAX_DELTA_MS) * 1_000_000n;
    const deltaNs = nowNs - lastNs;

    let frames = 0;
    if (deltaNs > maxDeltaNs) {
      frames = clock.advanceTo(lastNs + maxDeltaNs);
      // Drop the remaining time (do not "catch up" on the next tick).
      clock.lastTimeNs = nowNs;
    } else {
      frames = clock.advanceTo(nowNs);
    }

    if (frames > 0) {
      try {
        this.#bridge.step_frames(frames >>> 0);
      } catch {
        // ignore device errors during tick
      }
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

  /**
   * Attach/detach the shared AudioWorklet output ring buffer.
   *
   * When attached, the WASM-side HDA controller writes interleaved stereo `f32`
   * frames into the ring buffer (producer side). When detached, produced audio is
   * dropped but the device still advances.
   */
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

    // Plumb host output sample rate first so the HDA controller's time base matches
    // the `frames` argument passed via {@link tick}.
    if (dstSampleRateHz > 0 && typeof this.#bridge.set_output_rate_hz === "function") {
      try {
        this.#bridge.set_output_rate_hz(dstSampleRateHz);
        if (dstSampleRateHz !== this.#outputRateHz) {
          this.#outputRateHz = dstSampleRateHz;
          // Recreate the clock at the new rate, preserving the last observed time
          // so we don't introduce a large delta on the next tick.
          if (this.#clock) {
            this.#clock = new AudioFrameClock(dstSampleRateHz, this.#clock.lastTimeNs);
          }
        }
      } catch {
        // ignore invalid/missing rate plumbing
      }
    }

    // Attach/detach the output ring buffer (newer WASM builds only).
    if (ring) {
      if (typeof this.#bridge.attach_audio_ring === "function") {
        try {
          this.#bridge.attach_audio_ring(ring, capacityFrames, channelCount);
        } catch {
          // ignore
        }
      }
    } else if (typeof this.#bridge.detach_audio_ring === "function") {
      try {
        this.#bridge.detach_audio_ring();
      } catch {
        // ignore
      }
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
