import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { I8042Controller } from "./i8042";

describe("io/devices/I8042Controller", () => {
  it("pulses IRQ1 once per keyboard byte when interrupts are enabled", () => {
    const irqEvents: string[] = [];
    const irqSink: IrqSink = {
      raiseIrq: vi.fn((irq: number) => irqEvents.push(`raise:${irq}`)),
      lowerIrq: vi.fn((irq: number) => irqEvents.push(`lower:${irq}`)),
    };

    const dev = new I8042Controller(irqSink);

    // Enable IRQ1 in the command byte (bit 0) via controller command 0x60.
    dev.portWrite(0x0064, 1, 0x60);
    dev.portWrite(0x0060, 1, 0x01);

    dev.injectKeyboardBytes(Uint8Array.from([0x1c, 0x9c]));

    // Only the first byte is loaded into the output buffer immediately, so only one IRQ pulse
    // should be emitted until the guest consumes the data.
    expect(irqEvents).toEqual(["raise:1", "lower:1"]);
    expect(dev.portRead(0x0060, 1)).toBe(0x1c);

    // After consuming the first byte, the next queued byte becomes available and should generate
    // another pulse.
    expect(irqEvents).toEqual(["raise:1", "lower:1", "raise:1", "lower:1"]);
    expect(dev.portRead(0x0060, 1)).toBe(0x9c);

    // No further data means no more IRQ pulses.
    expect(irqEvents).toEqual(["raise:1", "lower:1", "raise:1", "lower:1"]);
  });
});

