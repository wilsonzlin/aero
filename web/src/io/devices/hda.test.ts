import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { HdaPciDevice, type HdaControllerBridgeLike } from "./hda";

function tick60HzNs(tickIndex: number): bigint {
  // 1 second / 60 = 16_666_666.666... ns. Distribute the extra 40ns across the
  // second: 40 ticks of 16_666_667ns and 20 ticks of 16_666_666ns.
  return tickIndex < 40 ? 16_666_667n : 16_666_666n;
}

describe("io/devices/hda tick scheduling", () => {
  it("advances audio frames deterministically at 60Hz without drift", () => {
    const irq: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    let totalFrames = 0;

    const bridge: HdaControllerBridgeLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      step_frames: (frames) => {
        totalFrames += frames >>> 0;
      },
      irq_level: () => false,
      set_mic_ring_buffer: () => {},
      set_capture_sample_rate_hz: () => {},
      free: () => {},
    };

    const dev = new HdaPciDevice({ bridge, irqSink: irq });
    // Enable bus mastering so the device tick will actually advance the HDA model.
    dev.onPciCommandWrite?.(1 << 2);

    // First tick initializes the internal clock.
    dev.tick(0);

    let nowNs = 0n;
    for (let second = 0; second < 600; second++) {
      for (let tick = 0; tick < 60; tick++) {
        nowNs += tick60HzNs(tick);
        // `perfNowMsToNs()` rounds via `Math.floor(nowMs * 1e6)`, so provide ms
        // values derived from integer nanoseconds.
        dev.tick(Number(nowNs) / 1e6);
      }
    }

    expect(nowNs).toBe(1_000_000_000n * 600n);
    expect(totalFrames).toBe(48_000 * 600);
  });

  it("drops excess host time beyond the max delta budget (does not catch up)", () => {
    const irq: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };

    const bridge: HdaControllerBridgeLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      step_frames: vi.fn(),
      irq_level: () => false,
      set_mic_ring_buffer: () => {},
      set_capture_sample_rate_hz: () => {},
      free: () => {},
    };

    const dev = new HdaPciDevice({ bridge, irqSink: irq });
    dev.onPciCommandWrite?.(1 << 2);

    // First tick initializes the clock.
    dev.tick(0);

    // Simulate a long stall (e.g. tab backgrounded). The device should advance by the
    // clamp budget (~100ms), then discard the remainder so it doesn't "catch up" later.
    dev.tick(500);
    // Next tick should only process the new delta (100ms), not the dropped 400ms.
    dev.tick(600);

    // Default output sample rate is 48kHz; max delta clamp is 100ms => 4800 frames.
    expect(bridge.step_frames).toHaveBeenCalledTimes(2);
    expect(bridge.step_frames).toHaveBeenNthCalledWith(1, 4_800);
    expect(bridge.step_frames).toHaveBeenNthCalledWith(2, 4_800);

    const stats = dev.getTickStats();
    expect(stats.tickClampEvents).toBe(1);
    expect(stats.tickClampedFramesTotal).toBe(4_800);
    // Dropped 400ms at 48kHz => 19,200 frames dropped.
    expect(stats.tickDroppedFramesTotal).toBe(19_200);
  });
});

describe("io/devices/HdaPciDevice", () => {
  it("exposes the expected PCI identity, canonical BDF, and BAR0 layout", () => {
    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new HdaPciDevice({ bridge, irqSink });
    expect(dev.vendorId).toBe(0x8086);
    expect(dev.deviceId).toBe(0x2668);
    expect(dev.subsystemVendorId).toBe(0x8086);
    expect(dev.subsystemId).toBe(0x2668);
    expect(dev.classCode).toBe(0x04_03_00);
    expect(dev.interruptPin).toBe(0x01);
    expect(dev.bdf).toEqual({ bus: 0, device: 4, function: 0 });
    expect(dev.bars).toEqual([{ kind: "mmio32", size: 0x4000 }, null, null, null, null, null]);
  });

  it("accepts camelCase HDA bridge exports (backwards compatibility)", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const mmioRead = vi.fn(() => 0);
    const mmioWrite = vi.fn();
    const stepFrames = vi.fn();
    const irqLevel = vi.fn(() => false);
    const setPciCommand = vi.fn();
    const setMicRingBuffer = vi.fn();
    const setCaptureSampleRateHz = vi.fn();
    const setOutputRateHz = vi.fn();
    const attachAudioRing = vi.fn();
    const detachAudioRing = vi.fn();
    const attachMicRing = vi.fn();
    const detachMicRing = vi.fn();
    const free = vi.fn();

    // Simulate an older/newer wasm-bindgen output (or manual shim) that exposes camelCase helpers.
    const bridge = {
      mmioRead,
      mmioWrite,
      stepFrames,
      irqLevel,
      setPciCommand,
      setMicRingBuffer,
      setCaptureSampleRateHz,
      setOutputRateHz,
      attachAudioRing,
      detachAudioRing,
      attachMicRing,
      detachMicRing,
      free,
    };

    const dev = new HdaPciDevice({ bridge: bridge as unknown as HdaControllerBridgeLike, irqSink });

    // Ensure MMIO plumbing uses the resolved methods.
    dev.mmioRead(0, 0n, 4);
    expect(mmioRead).toHaveBeenCalledWith(0, 4);
    dev.mmioWrite(0, 0n, 4, 0x1234);
    expect(mmioWrite).toHaveBeenCalledWith(0, 4, 0x1234);

    // PCI command mirror should pick up `setPciCommand`.
    dev.onPciCommandWrite?.(0x1_0004);
    expect(setPciCommand).toHaveBeenCalledTimes(1);
    expect(setPciCommand).toHaveBeenLastCalledWith(0x0004);

    // With bus mastering enabled, ticking should advance via `stepFrames`.
    dev.tick(0);
    dev.tick(1);
    expect(stepFrames).toHaveBeenCalledTimes(1);
    expect(stepFrames).toHaveBeenCalledWith(48);

    const ringBuffer =
      typeof SharedArrayBuffer === "function" ? new SharedArrayBuffer(256) : ({} as unknown as SharedArrayBuffer);

    dev.setAudioRingBuffer({ ringBuffer, capacityFrames: 128, channelCount: 2, dstSampleRateHz: 48_000 });
    expect(setOutputRateHz).toHaveBeenCalledWith(48_000);
    expect(attachAudioRing).toHaveBeenCalledWith(ringBuffer, 128, 2);

    // Configure capture, then attach/detach the mic ring. Prefer the explicit attach/detach helpers.
    dev.setCaptureSampleRateHz(44_100);
    // Ignore any eager detach calls while the ring is still detached.
    attachMicRing.mockClear();
    detachMicRing.mockClear();
    setMicRingBuffer.mockClear();

    dev.setMicRingBuffer(ringBuffer);
    expect(attachMicRing).toHaveBeenCalledWith(ringBuffer, 44_100);
    expect(setMicRingBuffer).not.toHaveBeenCalledWith(ringBuffer);

    dev.setMicRingBuffer(null);
    expect(detachMicRing).toHaveBeenCalled();

    dev.destroy();
    expect(detachAudioRing).toHaveBeenCalled();
    expect(free).toHaveBeenCalled();
  });

  it("forwards PCI command register writes into the WASM bridge when set_pci_command is available", () => {
    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_pci_command: vi.fn(),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    // Use a value with upper bits set to ensure the device masks to 16-bit.
    dev.onPciCommandWrite?.(0x1_0004);
    expect(bridge.set_pci_command).toHaveBeenCalledTimes(1);
    expect(bridge.set_pci_command).toHaveBeenLastCalledWith(0x0004);
  });

  it("gates DMA/time progression on PCI Command.BusMasterEnable (bit 2)", () => {
    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    // First tick initializes the clock.
    dev.tick(0);
    // No bus mastering yet -> no processing.
    dev.tick(1);
    expect(bridge.step_frames).not.toHaveBeenCalled();

    // Enable bus mastering: next tick should process only the new delta (not "catch up").
    dev.onPciCommandWrite?.(1 << 2);
    dev.tick(2);
    expect(bridge.step_frames).toHaveBeenCalledTimes(1);
    expect(bridge.step_frames).toHaveBeenLastCalledWith(48);
  });

  it("gates INTx assertion when PCI Command.InterruptDisable (bit 10) is set", () => {
    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => true),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    // Initial tick should observe irq_level=true and raise the IRQ.
    dev.tick(0);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(0);

    // Disabling INTx should force-deassert the line immediately.
    dev.onPciCommandWrite?.(1 << 10);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(1);

    // While disabled, the IRQ must not be asserted.
    dev.tick(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);

    // Re-enable INTx: since irq_level() is still true, it should re-assert.
    dev.onPciCommandWrite?.(0);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(2);
  });
});

describe("io/devices/HdaPciDevice audio ring attachment", () => {
  it("plumbs output sample rate and attaches/detaches the worklet ring when exports are available", () => {
    const attach = vi.fn();
    const detach = vi.fn();
    const setOutputRate = vi.fn();
    const setCaptureRate = vi.fn();

    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: setCaptureRate,
      attach_audio_ring: attach,
      detach_audio_ring: detach,
      set_output_rate_hz: setOutputRate,
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    const ringBuffer =
      typeof SharedArrayBuffer === "function" ? new SharedArrayBuffer(256) : ({} as unknown as SharedArrayBuffer);

    // Configure a mic capture rate that differs from the output rate, then change the output
    // rate via setAudioRingBuffer. The wrapper should reassert the capture sample rate so the
    // WASM device doesn't drift back to tracking output.
    dev.setCaptureSampleRateHz(44_100);
    setCaptureRate.mockClear();

    dev.setAudioRingBuffer({ ringBuffer, capacityFrames: 128, channelCount: 2, dstSampleRateHz: 48_000 });
    expect(setOutputRate).toHaveBeenCalledWith(48_000);
    expect(attach).toHaveBeenCalledWith(ringBuffer, 128, 2);
    expect(setCaptureRate).toHaveBeenCalledWith(44_100);

    dev.setAudioRingBuffer({ ringBuffer: null, capacityFrames: 0, channelCount: 0, dstSampleRateHz: 0 });
    expect(detach).toHaveBeenCalled();
  });

  it("falls back to set_audio_ring_buffer when attach/detach exports are unavailable", () => {
    const setAudioRing = vi.fn();
    const setOutputRate = vi.fn();

    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      set_audio_ring_buffer: setAudioRing,
      set_output_rate_hz: setOutputRate,
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    const ringBuffer =
      typeof SharedArrayBuffer === "function" ? new SharedArrayBuffer(256) : ({} as unknown as SharedArrayBuffer);

    dev.setAudioRingBuffer({ ringBuffer, capacityFrames: 128, channelCount: 2, dstSampleRateHz: 48_000 });
    expect(setAudioRing).toHaveBeenNthCalledWith(1, ringBuffer, 128, 2);

    dev.setAudioRingBuffer({ ringBuffer: null, capacityFrames: 0, channelCount: 0, dstSampleRateHz: 0 });
    expect(setAudioRing).toHaveBeenNthCalledWith(2, undefined, 0, 0);
  });

  it("falls back to set_output_sample_rate_hz when set_output_rate_hz is unavailable", () => {
    const setOutputSampleRate = vi.fn();
    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      set_output_sample_rate_hz: setOutputSampleRate,
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    dev.setAudioRingBuffer({ ringBuffer: null, capacityFrames: 0, channelCount: 0, dstSampleRateHz: 44_100 });
    expect(setOutputSampleRate).toHaveBeenCalledWith(44_100);
  });

  it("uses the configured output sample rate as the tick time base when set_output_rate_hz is available", () => {
    const irq: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    let totalFrames = 0;

    const setOutputRate = vi.fn();
    const bridge: HdaControllerBridgeLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      step_frames: (frames) => {
        totalFrames += frames >>> 0;
      },
      irq_level: () => false,
      set_mic_ring_buffer: () => {},
      set_capture_sample_rate_hz: () => {},
      set_output_rate_hz: setOutputRate,
      free: () => {},
    };

    const dev = new HdaPciDevice({ bridge, irqSink: irq });

    // Configure before the first tick so the clock initializes at 44.1kHz.
    dev.setAudioRingBuffer({ ringBuffer: null, capacityFrames: 0, channelCount: 0, dstSampleRateHz: 44_100 });
    expect(setOutputRate).toHaveBeenCalledWith(44_100);

    // Enable bus mastering so `tick()` actually advances the WASM-side model.
    dev.onPciCommandWrite?.(1 << 2);

    // First tick initializes the internal clock.
    dev.tick(0);

    let nowNs = 0n;
    for (let second = 0; second < 600; second++) {
      for (let tick = 0; tick < 60; tick++) {
        nowNs += tick60HzNs(tick);
        dev.tick(Number(nowNs) / 1e6);
      }
    }

    expect(totalFrames).toBe(44_100 * 600);
  });

  it("tracks the bridge-reported output_sample_rate_hz when set_output_rate_hz clamps the requested rate", () => {
    const irq: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    let totalFrames = 0;

    // Simulate a WASM bridge that clamps the output rate (e.g. to MAX_HOST_SAMPLE_RATE_HZ)
    // and exposes the effective rate via `output_sample_rate_hz`.
    let reportedRate = 0;

    const bridge: HdaControllerBridgeLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      step_frames: (frames) => {
        totalFrames += frames >>> 0;
      },
      irq_level: () => false,
      set_mic_ring_buffer: () => {},
      set_capture_sample_rate_hz: () => {},
      set_output_rate_hz: () => {
        // Clamp 96kHz down to 48kHz.
        reportedRate = 48_000;
      },
      free: () => {},
    };

    Object.defineProperty(bridge, "output_sample_rate_hz", {
      get: () => reportedRate,
      configurable: true,
    });

    const dev = new HdaPciDevice({ bridge, irqSink: irq });

    // Configure a high output rate (96kHz); the bridge reports it clamped to 48kHz.
    dev.setAudioRingBuffer({ ringBuffer: null, capacityFrames: 0, channelCount: 0, dstSampleRateHz: 96_000 });

    dev.onPciCommandWrite?.(1 << 2);
    dev.tick(0);

    // Tick for 1 second at 60Hz with deterministic nanosecond deltas.
    let nowNs = 0n;
    for (let tick = 0; tick < 60; tick++) {
      nowNs += tick60HzNs(tick);
      dev.tick(Number(nowNs) / 1e6);
    }

    // The device should advance based on the effective (clamped) sample rate.
    expect(totalFrames).toBe(48_000);
  });
});

describe("io/devices/HdaPciDevice microphone ring attachment", () => {
  it("mirrors capture sample rate into the WASM bridge even when unchanged", () => {
    const setCaptureRate = vi.fn();
    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: setCaptureRate,
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    dev.setCaptureSampleRateHz(48_000);
    expect(setCaptureRate).toHaveBeenCalledWith(48_000);

    setCaptureRate.mockClear();
    dev.setCaptureSampleRateHz(48_000);
    expect(setCaptureRate).toHaveBeenCalledTimes(1);
    expect(setCaptureRate).toHaveBeenCalledWith(48_000);
  });

  it("prefers attach_mic_ring/detach_mic_ring when the WASM bridge exports them", () => {
    const attachMic = vi.fn();
    const detachMic = vi.fn();
    const setMicRing = vi.fn();
    const setCaptureRate = vi.fn();

    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_mic_ring_buffer: setMicRing,
      set_capture_sample_rate_hz: setCaptureRate,
      attach_mic_ring: attachMic,
      detach_mic_ring: detachMic,
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    const ringBuffer =
      typeof SharedArrayBuffer === "function" ? new SharedArrayBuffer(256) : ({} as unknown as SharedArrayBuffer);

    dev.setCaptureSampleRateHz(48_000);
    expect(setCaptureRate).toHaveBeenCalledWith(48_000);
    // Ignore any eager detach calls triggered while the ring buffer is not yet attached.
    attachMic.mockClear();
    detachMic.mockClear();
    setMicRing.mockClear();
    setCaptureRate.mockClear();

    dev.setMicRingBuffer(ringBuffer);

    expect(attachMic).toHaveBeenCalledTimes(1);
    expect(attachMic).toHaveBeenCalledWith(ringBuffer, 48_000);
    // Ensure we did not attach via the legacy set_mic_ring_buffer(ring) path.
    expect(setMicRing).not.toHaveBeenCalledWith(ringBuffer);
    // `attach_mic_ring` should set sample rate as part of the call; no need to invoke the legacy setter.
    expect(setCaptureRate).not.toHaveBeenCalled();

    dev.setMicRingBuffer(null);
    expect(detachMic).toHaveBeenCalled();
    // Prefer the explicit detach API when available.
    expect(setMicRing).not.toHaveBeenCalledWith(undefined);
    // Keep the capture sample rate in sync even if the ring is detached.
    expect(setCaptureRate).toHaveBeenCalledWith(48_000);
  });
});
