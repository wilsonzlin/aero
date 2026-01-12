import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { HdaPciDevice, type HdaControllerBridgeLike } from "./hda";

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

