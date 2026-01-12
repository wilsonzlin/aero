import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { I8042Controller } from "./i8042";

describe("io/devices/I8042Controller (PS/2 mouse)", () => {
  it("routes 0xD4 + 0xFF reset to the mouse and returns the standard reply bytes", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    i8042.portWrite(0x64, 1, 0xd4);
    i8042.portWrite(0x60, 1, 0xff);

    expect(i8042.portRead(0x60, 1)).toBe(0xfa); // ACK
    expect(i8042.portRead(0x60, 1)).toBe(0xaa); // self test pass
    expect(i8042.portRead(0x60, 1)).toBe(0x00); // device id
  });

  it("encodes injected mouse movement into 3-byte PS/2 packets with correct sign bits", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // Enable data reporting.
    i8042.portWrite(0x64, 1, 0xd4);
    i8042.portWrite(0x60, 1, 0xf4);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa); // ACK

    i8042.injectMouseMotion(-1, -2, 0);

    expect(i8042.portRead(0x60, 1)).toBe(0x38); // bit3=1 + xSign + ySign
    expect(i8042.portRead(0x60, 1)).toBe(0xff); // dx=-1
    expect(i8042.portRead(0x60, 1)).toBe(0xfe); // dy=-2
  });

  it("splits large injected deltas into multiple PS/2 packets", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // Enable data reporting.
    i8042.portWrite(0x64, 1, 0xd4);
    i8042.portWrite(0x60, 1, 0xf4);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa); // ACK

    // dx=200 doesn't fit in a signed 8-bit delta, so it should be split (127 + 73).
    i8042.injectMouseMotion(200, 0, 0);

    // Packet 1: dx=127, dy=0.
    expect(i8042.portRead(0x60, 1)).toBe(0x08);
    expect(i8042.portRead(0x60, 1)).toBe(0x7f);
    expect(i8042.portRead(0x60, 1)).toBe(0x00);

    // Packet 2: dx=73, dy=0.
    expect(i8042.portRead(0x60, 1)).toBe(0x08);
    expect(i8042.portRead(0x60, 1)).toBe(0x49);
    expect(i8042.portRead(0x60, 1)).toBe(0x00);
  });
});
