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
    expect(dev.classCode).toBe(0x04_03_00);
    expect(dev.bdf).toEqual({ bus: 0, device: 4, function: 0 });
    expect(dev.bars).toEqual([{ kind: "mmio32", size: 0x4000 }, null, null, null, null, null]);
  });
});
