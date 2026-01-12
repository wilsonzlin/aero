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

    const bridge: HdaControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_level: vi.fn(() => false),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      attach_audio_ring: attach,
      detach_audio_ring: detach,
      set_output_rate_hz: setOutputRate,
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new HdaPciDevice({ bridge, irqSink });

    const ringBuffer =
      typeof SharedArrayBuffer === "function" ? new SharedArrayBuffer(256) : ({} as unknown as SharedArrayBuffer);

    dev.setAudioRingBuffer({ ringBuffer, capacityFrames: 128, channelCount: 2, dstSampleRateHz: 48_000 });
    expect(setOutputRate).toHaveBeenCalledWith(48_000);
    expect(attach).toHaveBeenCalledWith(ringBuffer, 128, 2);

    dev.setAudioRingBuffer({ ringBuffer: null, capacityFrames: 0, channelCount: 0, dstSampleRateHz: 0 });
    expect(detach).toHaveBeenCalled();
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
});
