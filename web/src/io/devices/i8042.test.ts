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

  it("does not pulse IRQ1 when interrupts are disabled", () => {
    const irqEvents: string[] = [];
    const irqSink: IrqSink = {
      raiseIrq: vi.fn((irq: number) => irqEvents.push(`raise:${irq}`)),
      lowerIrq: vi.fn((irq: number) => irqEvents.push(`lower:${irq}`)),
    };

    const dev = new I8042Controller(irqSink);

    dev.injectKeyboardBytes(Uint8Array.from([0x1c]));

    expect(irqEvents).toEqual([]);
    expect(dev.portRead(0x0060, 1)).toBe(0x1c);
    expect(irqEvents).toEqual([]);
  });

  it("pulses IRQ12 once per mouse byte when interrupts are enabled", () => {
    const irqEvents: string[] = [];
    const irqSink: IrqSink = {
      raiseIrq: vi.fn((irq: number) => irqEvents.push(`raise:${irq}`)),
      lowerIrq: vi.fn((irq: number) => irqEvents.push(`lower:${irq}`)),
    };

    const dev = new I8042Controller(irqSink);

    // Enable mouse IRQ (IRQ12) in the command byte (bit 1).
    dev.portWrite(0x0064, 1, 0x60);
    dev.portWrite(0x0060, 1, 0x02);

    // Ask the mouse for its device ID: controller command 0xD4 routes the next data byte to the mouse.
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xf2);

    // The mouse responds with ACK (0xFA) and ID (0x00). Only the first output byte is loaded
    // immediately, so expect one IRQ12 pulse so far.
    expect(irqEvents).toEqual(["raise:12", "lower:12"]);
    expect(dev.portRead(0x0064, 1) & 0x20).toBe(0x20); // STATUS_MOBF
    expect(dev.portRead(0x0060, 1)).toBe(0xfa);

    // Consuming the ACK causes the ID byte to become available, generating a second pulse.
    expect(irqEvents).toEqual(["raise:12", "lower:12", "raise:12", "lower:12"]);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
  });

  it("does not emit spurious IRQ pulses during snapshot restore", () => {
    const srcIrqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const src = new I8042Controller(srcIrqSink);

    // Enable IRQ1 and queue two bytes (one in the output buffer, one pending).
    src.portWrite(0x0064, 1, 0x60);
    src.portWrite(0x0060, 1, 0x01);
    src.injectKeyboardBytes(Uint8Array.from([0x1c, 0x9c]));

    const snap = src.saveState();

    const irqEvents: string[] = [];
    const irqSink: IrqSink = {
      raiseIrq: vi.fn((irq: number) => irqEvents.push(`raise:${irq}`)),
      lowerIrq: vi.fn((irq: number) => irqEvents.push(`lower:${irq}`)),
    };
    const restored = new I8042Controller(irqSink);

    restored.loadState(snap);
    expect(irqEvents).toEqual([]);

    // Reading the first byte causes the pending byte to become available and should generate an IRQ1 pulse.
    expect(restored.portRead(0x0060, 1)).toBe(0x1c);
    expect(irqEvents).toEqual(["raise:1", "lower:1"]);
  });
});
