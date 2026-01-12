import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { I8042Controller } from "./i8042";

function sendMouseCommand(i8042: I8042Controller, cmd: number): void {
  i8042.portWrite(0x64, 1, 0xd4);
  i8042.portWrite(0x60, 1, cmd & 0xff);
}

function sendMouseCommandWithData(i8042: I8042Controller, cmd: number, data: number): void {
  sendMouseCommand(i8042, cmd);
  expect(i8042.portRead(0x60, 1)).toBe(0xfa); // ACK
  sendMouseCommand(i8042, data);
  expect(i8042.portRead(0x60, 1)).toBe(0xfa); // ACK
}

function drainOutput(i8042: I8042Controller): number[] {
  const out: number[] = [];
  while ((i8042.portRead(0x64, 1) & 0x01) !== 0) {
    out.push(i8042.portRead(0x60, 1) & 0xff);
  }
  return out;
}

describe("io/devices/I8042Controller (PS/2 mouse)", () => {
  it("routes 0xD4 + 0xFF reset to the mouse and returns the standard reply bytes", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    sendMouseCommand(i8042, 0xff);

    expect(i8042.portRead(0x60, 1)).toBe(0xfa); // ACK
    expect(i8042.portRead(0x60, 1)).toBe(0xaa); // self test pass
    expect(i8042.portRead(0x60, 1)).toBe(0x00); // device id
  });

  it("drops injected motion while reporting is disabled (avoids buffering host deltas)", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // Motion before enabling data reporting should be dropped (no output bytes queued).
    i8042.injectMouseMove(10, 20);
    expect(i8042.portRead(0x64, 1) & 0x01).toBe(0x00); // STATUS_OBF

    // Enable data reporting.
    sendMouseCommand(i8042, 0xf4);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);

    i8042.injectMouseMove(10, 20);

    // Now a PS/2 movement packet should be present.
    expect(i8042.portRead(0x60, 1)).toBe(0x08);
    expect(i8042.portRead(0x60, 1)).toBe(10);
    expect(i8042.portRead(0x60, 1)).toBe(20);
  });

  it("encodes injected mouse movement into 3-byte PS/2 packets with correct sign bits", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // Enable data reporting.
    sendMouseCommand(i8042, 0xf4);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);

    i8042.injectMouseMotion(-1, -2, 0);

    expect(i8042.portRead(0x60, 1)).toBe(0x38); // bit3=1 + xSign + ySign
    expect(i8042.portRead(0x60, 1)).toBe(0xff); // dx=-1
    expect(i8042.portRead(0x60, 1)).toBe(0xfe); // dy=-2
  });

  it("splits large injected deltas into multiple PS/2 packets", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // Enable data reporting.
    sendMouseCommand(i8042, 0xf4);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);

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

  it("tracks button state while the mouse port is disabled and reflects it in the next packet", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // Enable data reporting.
    sendMouseCommand(i8042, 0xf4);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);

    // Disable the AUX (mouse) port.
    i8042.portWrite(0x64, 1, 0xa7);

    // Update the host button mask while the port is disabled. No output bytes should be queued.
    i8042.injectMouseButtons(0x01);
    expect(i8042.portRead(0x64, 1) & 0x01).toBe(0x00);

    // Re-enable the port and inject motion; the packet should include the held button bit.
    i8042.portWrite(0x64, 1, 0xa8);
    i8042.injectMouseMove(1, 0);

    expect(i8042.portRead(0x60, 1)).toBe(0x09); // left button + bit3
    expect(i8042.portRead(0x60, 1)).toBe(0x01);
    expect(i8042.portRead(0x60, 1)).toBe(0x00);
  });

  it("sets X overflow bit when remote-mode read data clamps a large delta", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // Set remote mode.
    sendMouseCommand(i8042, 0xf0);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);

    // Inject a delta larger than what one PS/2 packet can represent.
    i8042.injectMouseMotion(300, 0, 0);

    // Read data in remote mode.
    sendMouseCommand(i8042, 0xeb);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);

    // Packet: status (bit6 overflow), dx saturated to 255, dy 0.
    expect(i8042.portRead(0x60, 1)).toBe(0x48);
    expect(i8042.portRead(0x60, 1)).toBe(0xff);
    expect(i8042.portRead(0x60, 1)).toBe(0x00);
  });

  it("supports IntelliMouse wheel extension (device ID 0x03) and emits a 4th byte with wheel delta", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // IntelliMouse wheel extension sequence: 200, 100, 80.
    sendMouseCommandWithData(i8042, 0xf3, 200);
    sendMouseCommandWithData(i8042, 0xf3, 100);
    sendMouseCommandWithData(i8042, 0xf3, 80);

    // Ensure the device reports the expected ID.
    sendMouseCommand(i8042, 0xf2);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa); // ACK
    expect(i8042.portRead(0x60, 1)).toBe(0x03);

    // Enable data reporting.
    sendMouseCommand(i8042, 0xf4);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);

    i8042.injectMouseWheel(1);

    // Expect a 4-byte packet where the final byte encodes the wheel delta in the low nibble.
    expect(i8042.portRead(0x60, 1)).toBe(0x08);
    expect(i8042.portRead(0x60, 1)).toBe(0x00);
    expect(i8042.portRead(0x60, 1)).toBe(0x00);
    expect(i8042.portRead(0x60, 1)).toBe(0x01);
  });

  it("supports IntelliMouse Explorer (device ID 0x04) and encodes back/forward buttons in the 4th byte", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const i8042 = new I8042Controller(irqSink);

    // IntelliMouse Explorer sequence: 200, 200, 80.
    sendMouseCommandWithData(i8042, 0xf3, 200);
    sendMouseCommandWithData(i8042, 0xf3, 200);
    sendMouseCommandWithData(i8042, 0xf3, 80);

    // Ensure the device reports the expected ID.
    sendMouseCommand(i8042, 0xf2);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);
    expect(i8042.portRead(0x60, 1)).toBe(0x04);

    // Enable data reporting.
    sendMouseCommand(i8042, 0xf4);
    expect(i8042.portRead(0x60, 1)).toBe(0xfa);

    // Emit a packet carrying only the extra button bits. (Primary buttons are in byte 0; extra buttons are in byte 3.)
    i8042.injectMouseButtons(0x08 | 0x10);
    const packet = drainOutput(i8042);

    // Expect 4-byte packet: status + dx + dy + (wheel nibble + button bits).
    expect(packet).toEqual([0x08, 0x00, 0x00, 0x30]);
  });
});
