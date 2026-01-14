import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";
import { AudioFrameClock, perfNowMsToNs } from "../../audio/audio_frame_clock";

export type HdaControllerBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;

  /**
   * Optional PCI command register mirror (offset 0x04, 16-bit).
   *
   * Newer WASM builds may enforce DMA gating on Bus Master Enable internally, so the JS PCI bus
   * must forward command register writes into the WASM device model.
   */
  set_pci_command?: (command: number) => void;

  /**
   * Newer stepping API.
   */
  process?: (frames: number) => void;
  /**
   * Alias for {@link process} retained by some WASM builds.
   */
  step_frames?: (frames: number) => void;
  /**
   * Older stepping API.
   */
  tick?: (frames: number) => void;
  /**
   * Slow fallback for unexpected/old exports.
   */
  step_frame?: () => void;

  /**
   * PCI INTx line level (level-triggered).
   */
  irq_level?: () => boolean;
  irq_asserted?: () => boolean;

  /**
   * Optional host/output sample rate plumbing (newer WASM builds).
   *
   * Must match the output AudioContext's `sampleRate` when streaming into an
   * AudioWorklet ring buffer.
   */
  set_output_rate_hz?: (rateHz: number) => void;
  /**
   * Alias for {@link set_output_rate_hz} retained for older call sites/spec drafts.
   */
  set_output_sample_rate_hz?: (rateHz: number) => void;
  /**
   * Current output sample rate as reported by the WASM bridge (newer builds only).
   */
  readonly output_sample_rate_hz?: number;

  // Output ring buffer attachment (AudioWorklet producer ring).
  attach_audio_ring?: (sab: SharedArrayBuffer, capacityFrames: number, channelCount: number) => void;
  detach_audio_ring?: () => void;
  /**
   * Optional legacy/compat output ring attachment helper.
   *
   * Some WASM builds expose `set_audio_ring_buffer(undefined)` as the detach mechanism.
   */
  set_audio_ring_buffer?: (
    ringSab?: SharedArrayBuffer | null,
    capacityFrames?: number,
    channelCount?: number,
  ) => void;
  // Legacy/alternate names for output ring attachment.
  attach_output_ring?: (sab: SharedArrayBuffer, capacityFrames: number, channelCount: number) => void;
  detach_output_ring?: () => void;

  // Microphone capture ring attachment.
  attach_mic_ring?: (sab: SharedArrayBuffer, sampleRateHz: number) => void;
  detach_mic_ring?: () => void;
  // Legacy mic ring attachment helpers (used by older call sites).
  set_mic_ring_buffer(sab?: SharedArrayBuffer): void;
  set_capture_sample_rate_hz(sampleRateHz: number): void;
  free(): void;
};

// PCI identity matches `crates/devices/src/pci/profile.rs::HDA_ICH6` (Intel ICH6 HDA).
const HDA_CLASS_CODE = 0x04_03_00;
const HDA_MMIO_BAR_SIZE = 0x4000;

// Stable legacy IRQ line value reported in PCI config space (0x3C). This is writable by the guest
// and PCI INTx lines are refcounted, so sharing is OK.
const HDA_IRQ_LINE = 0x0b;
const HDA_DEFAULT_OUTPUT_RATE_HZ = 48_000;

// Avoid pathological catch-up work if the tab is backgrounded or the worker stalls.
// If the delta exceeds this, we advance by this amount and drop the remainder.
const HDA_MAX_DELTA_MS = 100;

const NS_PER_SEC = 1_000_000_000n;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Intel ICH6 HDA PCI function backed by a WASM `HdaControllerBridge`.
 *
 * Exposes BAR0 (MMIO, 0x4000 bytes) and advances the device model in {@link tick} based on the
 * host clock.
 *
 * IRQ semantics:
 * HDA uses PCI INTx, which is level-triggered. The WASM bridge exposes the current INTx level and
 * this device forwards only level transitions (`raiseIrq` on 0→1, `lowerIrq` on 1→0).
 */
export class HdaPciDevice implements PciDevice, TickableDevice {
  readonly name = "ich6-hda";
  readonly vendorId = 0x8086;
  readonly deviceId = 0x2668;
  readonly subsystemVendorId = 0x8086;
  readonly subsystemId = 0x2668;
  readonly classCode = HDA_CLASS_CODE;
  readonly revisionId = 0x01;
  readonly irqLine = HDA_IRQ_LINE;
  readonly interruptPin = 0x01 as const; // INTA#
  readonly bdf = { bus: 0, device: 4, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio32", size: HDA_MMIO_BAR_SIZE }, null, null, null, null, null];

  readonly #bridge: HdaControllerBridgeLike;
  readonly #mmioRead: (offset: number, size: number) => number;
  readonly #mmioWrite: (offset: number, size: number, value: number) => void;
  readonly #irqSink: IrqSink;

  #clock: AudioFrameClock | null = null;
  #outputRateHz = HDA_DEFAULT_OUTPUT_RATE_HZ;
  #busMasterEnabled = false;
  #intxDisabled = false;
  #irqLevel = false;
  #destroyed = false;
  #micRingBuffer: SharedArrayBuffer | null = null;
  #micSampleRateHz = 0;

  // Observability for host/worker stalls: when `tick()` clamps large host deltas to
  // `HDA_MAX_DELTA_MS`, we count how often that happens and how much audio time is
  // dropped. These are u32 counters (wrapping) to match other perf/telemetry counters.
  #tickClampEvents = 0;
  #tickClampedFramesTotal = 0;
  #tickDroppedFramesTotal = 0;

  constructor(opts: { bridge: HdaControllerBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;

    // Backwards compatibility: accept both snake_case and camelCase MMIO methods.
    // Pre-resolve these since they're on the hot MMIO path.
    const bridgeAny = opts.bridge as unknown as Record<string, unknown>;
    const mmioRead = bridgeAny.mmio_read ?? bridgeAny.mmioRead;
    const mmioWrite = bridgeAny.mmio_write ?? bridgeAny.mmioWrite;
    if (typeof mmioRead !== "function" || typeof mmioWrite !== "function") {
      throw new Error("HDA bridge missing mmio_read/mmio_write exports.");
    }
    this.#mmioRead = mmioRead as (offset: number, size: number) => number;
    this.#mmioWrite = mmioWrite as (offset: number, size: number, value: number) => void;
  }

  getTickStats(): {
    tickClampEvents: number;
    tickClampedFramesTotal: number;
    tickDroppedFramesTotal: number;
  } {
    // Avoid hot-path allocations: this object is only created when the caller
    // explicitly requests stats (e.g. low-rate perf sampling).
    return {
      tickClampEvents: this.#tickClampEvents,
      tickClampedFramesTotal: this.#tickClampedFramesTotal,
      tickDroppedFramesTotal: this.#tickDroppedFramesTotal,
    };
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > HDA_MMIO_BAR_SIZE) return defaultReadValue(size);

    let value = defaultReadValue(size);
    try {
      value = this.#mmioRead.call(this.#bridge, off >>> 0, size >>> 0) >>> 0;
    } catch {
      value = defaultReadValue(size);
    }

    // Reads of interrupt status registers can clear the IRQ; keep the line level accurate.
    this.#syncIrq();
    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > HDA_MMIO_BAR_SIZE) return;

    try {
      this.#mmioWrite.call(this.#bridge, off >>> 0, size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest MMIO
    }
    this.#syncIrq();
  }

  onPciCommandWrite(command: number): void {
    if (this.#destroyed) return;
    const cmd = command & 0xffff;
    // PCI Command bit 2: Bus Master Enable (DMA allowed).
    this.#busMasterEnabled = (cmd & (1 << 2)) !== 0;
    // PCI Command bit 10: Interrupt Disable (INTx must not be asserted).
    this.#intxDisabled = (cmd & (1 << 10)) !== 0;

    // Mirror into the WASM device model (when supported) so it can enforce DMA gating coherently.
    const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
    const setCmd = bridgeAny.set_pci_command ?? bridgeAny.setPciCommand;
    if (typeof setCmd === "function") {
      try {
        setCmd.call(this.#bridge, cmd >>> 0);
      } catch {
        // ignore device errors during PCI config writes
      }
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

    const sr = this.#outputRateHz;
    if (!this.#clock) {
      this.#clock = new AudioFrameClock(sr, nowNs);
      this.#syncIrq();
      return;
    }

    // Avoid pathological catch-up work if the tab is backgrounded or the worker stalls.
    // Drop excess time beyond `HDA_MAX_DELTA_MS`.
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
      // Record clamp stats before mutating the clock. The dropped frame estimate uses the same
      // fixed-point math as `AudioFrameClock.advanceTo()`, but without advancing the clock.
      const fracBefore = clock.fracNsTimesRate;
      const srBig = BigInt(clock.sampleRateHz);
      const framesWouldHaveElapsed = (fracBefore + deltaNs * srBig) / NS_PER_SEC;

      frames = clock.advanceTo(lastNs + maxDeltaNs);

      this.#tickClampEvents = (this.#tickClampEvents + 1) >>> 0;
      this.#tickClampedFramesTotal = (this.#tickClampedFramesTotal + (frames >>> 0)) >>> 0;

      const dropped = framesWouldHaveElapsed - BigInt(frames);
      const droppedU32 = Number((dropped > 0n ? dropped : 0n) & 0xffff_ffffn);
      this.#tickDroppedFramesTotal = (this.#tickDroppedFramesTotal + droppedU32) >>> 0;

      // Drop the remaining time (do not "catch up" on the next tick).
      clock.lastTimeNs = nowNs;
      clock.fracNsTimesRate = 0n;
    } else {
      frames = clock.advanceTo(nowNs);
    }

    // Only allow the device to DMA when PCI Bus Mastering is enabled (PCI command bit 2).
    if (frames > 0 && this.#busMasterEnabled) {
      const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
      try {
        const process = bridgeAny.process;
        if (typeof process === "function") {
          (process as (frames: number) => void).call(this.#bridge, frames);
        } else {
          const stepFrames = bridgeAny.step_frames ?? bridgeAny.stepFrames;
          if (typeof stepFrames === "function") {
            (stepFrames as (frames: number) => void).call(this.#bridge, frames);
          } else {
            const tick = bridgeAny.tick;
            if (typeof tick === "function") {
              (tick as (frames: number) => void).call(this.#bridge, frames);
            } else {
              const stepFrame = bridgeAny.step_frame ?? bridgeAny.stepFrame;
              if (typeof stepFrame === "function") {
                // Slow fallback for unexpected/old exports.
                const step = stepFrame as () => void;
                for (let i = 0; i < frames; i++) step.call(this.#bridge);
              }
            }
          }
        }
      } catch {
        // ignore device errors during tick
      }
    }

    this.#syncIrq();
  }

  setMicRingBuffer(sab: SharedArrayBuffer | null): void {
    if (this.#destroyed) return;
    this.#micRingBuffer = sab;
    this.#syncMic();
  }

  setCaptureSampleRateHz(sampleRateHz: number): void {
    if (this.#destroyed) return;
    const sr = sampleRateHz >>> 0;
    if (sr === 0) return;
    const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
    const setCaptureRate = bridgeAny.set_capture_sample_rate_hz ?? bridgeAny.setCaptureSampleRateHz;

    // Even if the sample rate is unchanged from the JS wrapper's perspective, the WASM-side
    // controller can drift (e.g. `set_output_rate_hz` may implicitly update the capture rate when
    // it was still tracking the previous output rate). Keep the device model in sync by mirroring
    // the rate into WASM on every call.
    if (sr === this.#micSampleRateHz) {
      try {
        if (typeof setCaptureRate === "function") (setCaptureRate as (rateHz: number) => void).call(this.#bridge, sr);
      } catch {
        // ignore
      }
      return;
    }
    this.#micSampleRateHz = sr;
    this.#syncMic();
  }

  /**
   * Attach/detach the shared AudioWorklet output ring buffer (producer-side).
   *
   * When attached, the WASM-side HDA controller writes interleaved `f32` frames
   * into the ring buffer. When detached, produced audio is dropped but the
   * device model can still advance.
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

    const bridgeAny = this.#bridge as unknown as Record<string, unknown>;

    // Plumb host output sample rate first so the HDA controller's time base matches
    // the `frames` argument passed via {@link tick}.
    const setRate =
      bridgeAny.set_output_rate_hz ??
      bridgeAny.setOutputRateHz ??
      bridgeAny.set_output_sample_rate_hz ??
      bridgeAny.setOutputSampleRateHz;
    if (dstSampleRateHz > 0 && typeof setRate === "function") {
      try {
        (setRate as (rateHz: number) => void).call(this.#bridge, dstSampleRateHz);

        // Some WASM builds clamp the rate internally (see `MAX_HOST_SAMPLE_RATE_HZ`). Read back the
        // reported output rate when available so our tick clock stays consistent with the device.
        let reported = bridgeAny.output_sample_rate_hz ?? bridgeAny.outputSampleRateHz;
        if (typeof reported === "function") {
          try {
            reported = (reported as () => unknown).call(this.#bridge);
          } catch {
            reported = undefined;
          }
        }
        const effectiveRate =
          typeof reported === "number" && Number.isFinite(reported) && reported > 0 ? (reported >>> 0) : dstSampleRateHz;

        if (effectiveRate !== this.#outputRateHz) {
          this.#outputRateHz = effectiveRate;
          // Recreate the clock at the new rate, preserving the last observed time
          // so we don't introduce a large delta on the next tick.
          if (this.#clock) {
            this.#clock = new AudioFrameClock(effectiveRate, this.#clock.lastTimeNs);
          }
        }

        // The Rust controller defaults to tracking the capture sample rate to the output sample
        // rate until the host explicitly configures a distinct capture rate. If the guest is
        // attached to the mic ring, keep the capture rate pinned to the host mic AudioContext even
        // when the output rate changes.
        if (this.#micSampleRateHz > 0) {
          const setCaptureRate = bridgeAny.set_capture_sample_rate_hz ?? bridgeAny.setCaptureSampleRateHz;
          try {
            if (typeof setCaptureRate === "function") (setCaptureRate as (rateHz: number) => void).call(this.#bridge, this.#micSampleRateHz);
          } catch {
            // ignore
          }
        }
      } catch {
        // ignore invalid/missing rate plumbing
      }
    }

    // Prefer a single call if the WASM bridge exposes a combined helper.
    const setRing = bridgeAny.set_audio_ring_buffer ?? bridgeAny.setAudioRingBuffer;
    if (typeof setRing === "function") {
      try {
        if (ring && capacityFrames > 0 && channelCount > 0) {
          (setRing as (sab?: SharedArrayBuffer | null, cap?: number, ch?: number) => void).call(
            this.#bridge,
            ring,
            capacityFrames,
            channelCount,
          );
        } else {
          // Detach. Some wasm-bindgen bindings accept `null`, others `undefined`.
          try {
            (setRing as (sab?: SharedArrayBuffer | null, cap?: number, ch?: number) => void).call(this.#bridge, undefined, 0, 0);
          } catch {
            (setRing as (sab?: SharedArrayBuffer | null, cap?: number, ch?: number) => void).call(this.#bridge, null, 0, 0);
          }
        }
        return;
      } catch {
        // fall through to explicit attach/detach
      }
    }

    // Otherwise use the explicit attach/detach API (or legacy aliases).
    if (ring && capacityFrames > 0 && channelCount > 0) {
      const attach =
        bridgeAny.attach_audio_ring ??
        bridgeAny.attachAudioRing ??
        bridgeAny.attach_output_ring ??
        bridgeAny.attachOutputRing;
      if (typeof attach === "function") {
        try {
          (attach as (sab: SharedArrayBuffer, cap: number, ch: number) => void).call(
            this.#bridge,
            ring,
            capacityFrames,
            channelCount,
          );
        } catch {
          // ignore
        }
      }
    } else {
      const detach =
        bridgeAny.detach_audio_ring ??
        bridgeAny.detachAudioRing ??
        bridgeAny.detach_output_ring ??
        bridgeAny.detachOutputRing;
      if (typeof detach === "function") {
        try {
          (detach as () => void).call(this.#bridge);
        } catch {
          // ignore
        }
      }
    }
  }

  destroy(): void {
    if (this.#destroyed) return;
    // Best-effort detach to ensure no further samples are written into an orphaned ring buffer.
    try {
      const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
      const detachOut =
        bridgeAny.detach_audio_ring ??
        bridgeAny.detachAudioRing ??
        bridgeAny.detach_output_ring ??
        bridgeAny.detachOutputRing;
      if (typeof detachOut === "function") (detachOut as () => void).call(this.#bridge);
    } catch {
      // ignore
    }
    try {
      const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
      const detachMic = bridgeAny.detach_mic_ring ?? bridgeAny.detachMicRing;
      if (typeof detachMic === "function") {
        (detachMic as () => void).call(this.#bridge);
      } else {
        const setBuf = bridgeAny.set_mic_ring_buffer ?? bridgeAny.setMicRingBuffer;
        if (typeof setBuf === "function") (setBuf as (sab?: SharedArrayBuffer) => void).call(this.#bridge, undefined);
      }
    } catch {
      // ignore
    }

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
      const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
      const irqLevel = bridgeAny.irq_level ?? bridgeAny.irqLevel;
      const irqAsserted = bridgeAny.irq_asserted ?? bridgeAny.irqAsserted;
      if (this.#intxDisabled) {
        asserted = false;
      } else if (typeof irqLevel === "function") {
        asserted = Boolean((irqLevel as () => unknown).call(this.#bridge));
      } else if (typeof irqAsserted === "function") {
        asserted = Boolean((irqAsserted as () => unknown).call(this.#bridge));
      }
    } catch {
      asserted = false;
    }

    if (asserted === this.#irqLevel) return;
    this.#irqLevel = asserted;
    if (asserted) this.#irqSink.raiseIrq(this.irqLine);
    else this.#irqSink.lowerIrq(this.irqLine);
  }

  #syncMic(): void {
    if (this.#destroyed) return;

    const ring = this.#micRingBuffer;
    const sr = this.#micSampleRateHz >>> 0;
    const bridgeAny = this.#bridge as unknown as Record<string, unknown>;
    const attachMic = bridgeAny.attach_mic_ring ?? bridgeAny.attachMicRing;
    const detachMic = bridgeAny.detach_mic_ring ?? bridgeAny.detachMicRing;
    const setMicRing = bridgeAny.set_mic_ring_buffer ?? bridgeAny.setMicRingBuffer;
    const setCaptureRate = bridgeAny.set_capture_sample_rate_hz ?? bridgeAny.setCaptureSampleRateHz;

    // Prefer the newer attach/detach helpers when available so capture sample-rate
    // is applied alongside ring attachment.
    if (ring) {
      if (typeof attachMic === "function" && sr > 0) {
        try {
          (attachMic as (ring: SharedArrayBuffer, sr: number) => void).call(this.#bridge, ring, sr);
          return;
        } catch {
          // fall through to legacy path
        }
      }

      // Legacy: attach ring first, then apply sample rate if available.
      try {
        if (typeof setMicRing === "function") (setMicRing as (sab?: SharedArrayBuffer) => void).call(this.#bridge, ring);
      } catch {
        // ignore
      }
    } else {
      // Detach.
      let detached = false;
      if (typeof detachMic === "function") {
        try {
          (detachMic as () => void).call(this.#bridge);
          detached = true;
        } catch {
          detached = false;
        }
      }

      if (!detached) {
        try {
          if (typeof setMicRing === "function") (setMicRing as (sab?: SharedArrayBuffer) => void).call(this.#bridge, undefined);
        } catch {
          // ignore
        }
      }
    }

    // Keep the capture sample-rate in sync even if the ring is not yet attached.
    if (sr > 0 && typeof setCaptureRate === "function") {
      try {
        (setCaptureRate as (rateHz: number) => void).call(this.#bridge, sr);
      } catch {
        // ignore
      }
    }
  }
}
