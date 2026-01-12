import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { I8042Controller } from "./i8042";

function drainAllOutputBytes(dev: I8042Controller): number[] {
  const out: number[] = [];
  while ((dev.portRead(0x0064, 1) & 0x01) !== 0) {
    out.push(dev.portRead(0x0060, 1));
  }
  return out;
}

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

    // Disable IRQ1 (bit 0) while leaving translation enabled (bit 6).
    dev.portWrite(0x0064, 1, 0x60);
    dev.portWrite(0x0060, 1, 0x44);

    dev.injectKeyboardBytes(Uint8Array.from([0x1c]));

    expect(irqEvents).toEqual([]);
    // 0x1C (Set-2 'A') -> 0x1E (Set-1 'A').
    expect(dev.portRead(0x0060, 1)).toBe(0x1e);
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

  it("injects PS/2 mouse movement into the controller output buffer", () => {
    const irqEvents: string[] = [];
    const irqSink: IrqSink = {
      raiseIrq: vi.fn((irq: number) => irqEvents.push(`raise:${irq}`)),
      lowerIrq: vi.fn((irq: number) => irqEvents.push(`lower:${irq}`)),
    };

    const dev = new I8042Controller(irqSink);

    // Enable mouse IRQ (IRQ12) in the command byte (bit 1).
    dev.portWrite(0x0064, 1, 0x60);
    dev.portWrite(0x0060, 1, 0x02);

    // Enable mouse reporting via the real command path (controller command 0xD4 routes
    // the next data byte to the mouse).
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xf4);

    // Drain the ACK emitted by the mouse.
    expect(dev.portRead(0x0060, 1)).toBe(0xfa);
    expect(dev.portRead(0x0064, 1) & 0x01).toBe(0x00);

    // Ignore the IRQ pulse for the ACK; we want to observe the injection path.
    irqEvents.length = 0;

    dev.injectMouseMove(10, 20);

    // The output buffer should contain a mouse packet, and the MOBF bit should be set while
    // the head byte is from the mouse.
    expect(dev.portRead(0x0064, 1) & 0x21).toBe(0x21); // STATUS_OBF | STATUS_MOBF
    expect(irqEvents).toEqual(["raise:12", "lower:12"]);

    // 3-byte packet: status, dx, dy.
    expect(dev.portRead(0x0060, 1)).toBe(0x08);
    expect(dev.portRead(0x0060, 1)).toBe(10);
    expect(dev.portRead(0x0060, 1)).toBe(20);

    // Packet drained: output buffer empty.
    expect(dev.portRead(0x0064, 1) & 0x01).toBe(0x00);
  });

  it("injects PS/2 mouse button state into the controller output buffer", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const dev = new I8042Controller(irqSink);

    // Enable mouse reporting via the real command path.
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xf4);
    expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK

    dev.injectMouseButtons(0x01); // left button

    expect(dev.portRead(0x0064, 1) & 0x21).toBe(0x21); // STATUS_OBF | STATUS_MOBF

    // 3-byte packet: status (with left button pressed), dx=0, dy=0.
    expect(dev.portRead(0x0060, 1)).toBe(0x09);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
  });

  it("injects PS/2 mouse wheel packets when IntelliMouse mode is enabled", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const dev = new I8042Controller(irqSink);

    const sendMouseByte = (value: number) => {
      dev.portWrite(0x0064, 1, 0xd4);
      dev.portWrite(0x0060, 1, value);
      expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    };

    // Enable mouse reporting.
    sendMouseByte(0xf4);

    // Enable IntelliMouse wheel mode (200,100,80 sample rate sequence).
    sendMouseByte(0xf3);
    sendMouseByte(200);
    sendMouseByte(0xf3);
    sendMouseByte(100);
    sendMouseByte(0xf3);
    sendMouseByte(80);

    // Confirm device ID is the wheel mouse (0x03).
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xf2);
    expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    expect(dev.portRead(0x0060, 1)).toBe(0x03); // device id

    dev.injectMouseWheel(1);

    expect(dev.portRead(0x0064, 1) & 0x21).toBe(0x21); // STATUS_OBF | STATUS_MOBF

    // 4-byte packet: status, dx=0, dy=0, wheel=+1.
    expect(dev.portRead(0x0060, 1)).toBe(0x08);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
    expect(dev.portRead(0x0060, 1)).toBe(0x01);
  });

  it("recognizes the IntelliMouse Explorer (5-button) sample rate sequence and reports device ID 0x04", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const dev = new I8042Controller(irqSink);

    const sendMouseByte = (value: number) => {
      dev.portWrite(0x0064, 1, 0xd4);
      dev.portWrite(0x0060, 1, value);
      expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    };

    // IntelliMouse Explorer mode (200,200,80 sample rate sequence).
    sendMouseByte(0xf3);
    sendMouseByte(200);
    sendMouseByte(0xf3);
    sendMouseByte(200);
    sendMouseByte(0xf3);
    sendMouseByte(80);

    // Confirm device ID is 0x04.
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xf2);
    expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    expect(dev.portRead(0x0060, 1)).toBe(0x04); // device id
  });

  it("encodes IntelliMouse Explorer side/extra button bits in the 4th packet byte (device ID 0x04)", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const dev = new I8042Controller(irqSink);

    const sendMouseByte = (value: number) => {
      dev.portWrite(0x0064, 1, 0xd4);
      dev.portWrite(0x0060, 1, value);
      expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    };

    // Enable IntelliMouse Explorer mode (200,200,80 sample rate sequence).
    sendMouseByte(0xf3);
    sendMouseByte(200);
    sendMouseByte(0xf3);
    sendMouseByte(200);
    sendMouseByte(0xf3);
    sendMouseByte(80);

    // Verify device ID.
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xf2);
    expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    expect(dev.portRead(0x0060, 1)).toBe(0x04);

    // Enable reporting so button updates emit packets.
    sendMouseByte(0xf4);

    // Side button (bit3) should set bit4 in the fourth packet byte.
    dev.injectMouseButtons(0x08);
    expect(dev.portRead(0x0060, 1)).toBe(0x08);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
    expect(dev.portRead(0x0060, 1)).toBe(0x10);

    // Extra button (bit4) should set bit5 in the fourth packet byte, preserving bit4 while held.
    dev.injectMouseButtons(0x18);
    expect(dev.portRead(0x0060, 1)).toBe(0x08);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
    expect(dev.portRead(0x0060, 1)).toBe(0x00);
    expect(dev.portRead(0x0060, 1)).toBe(0x30);
  });

  it("clears IntelliMouse mode back to device ID 0x00 when Set Defaults (0xF6) is issued", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const dev = new I8042Controller(irqSink);

    const sendMouseByte = (value: number) => {
      dev.portWrite(0x0064, 1, 0xd4);
      dev.portWrite(0x0060, 1, value);
      expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    };

    // Enable IntelliMouse wheel mode (200,100,80 sample rate sequence).
    sendMouseByte(0xf3);
    sendMouseByte(200);
    sendMouseByte(0xf3);
    sendMouseByte(100);
    sendMouseByte(0xf3);
    sendMouseByte(80);

    // Confirm device ID is 0x03.
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xf2);
    expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    expect(dev.portRead(0x0060, 1)).toBe(0x03); // device id

    // Set Defaults should clear the extension.
    sendMouseByte(0xf6);

    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xf2);
    expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    expect(dev.portRead(0x0060, 1)).toBe(0x00); // device id
  });

  it("includes the remote-mode bit in the PS/2 mouse status byte (0xE9) after Set Remote Mode (0xF0)", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const dev = new I8042Controller(irqSink);

    const sendMouseByte = (value: number) => {
      dev.portWrite(0x0064, 1, 0xd4);
      dev.portWrite(0x0060, 1, value);
      expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    };

    // Enter remote mode.
    sendMouseByte(0xf0);

    // Status request should include bit6=remote mode.
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xe9);
    expect(dev.portRead(0x0060, 1)).toBe(0xfa); // ACK
    const stRemote = dev.portRead(0x0060, 1);
    expect(stRemote & 0x40).toBe(0x40);
    expect(dev.portRead(0x0060, 1)).toBe(0x04); // resolution default
    expect(dev.portRead(0x0060, 1)).toBe(0x64); // sample rate default (100)

    // Back to stream mode.
    sendMouseByte(0xea);
    dev.portWrite(0x0064, 1, 0xd4);
    dev.portWrite(0x0060, 1, 0xe9);
    expect(dev.portRead(0x0060, 1)).toBe(0xfa);
    const stStream = dev.portRead(0x0060, 1);
    expect(stStream & 0x40).toBe(0x00);
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
    expect(restored.portRead(0x0064, 1) & 0x01).toBe(0x01);

    // Reading the first byte causes the pending byte to become available and should generate an IRQ1 pulse.
    expect(restored.portRead(0x0060, 1)).toBe(0x1c);
    expect(irqEvents).toEqual(["raise:1", "lower:1"]);

    // Draining the remaining byte should not generate any additional pulses (output buffer becomes empty).
    expect(restored.portRead(0x0060, 1)).toBe(0x9c);
    expect(irqEvents).toEqual(["raise:1", "lower:1"]);
    expect(restored.portRead(0x0064, 1) & 0x01).toBe(0x00);
  });

  it("translates Set-2 keyboard scancodes to Set-1 when translation is enabled", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new I8042Controller(irqSink);

    // Set-2: KeyA make (0x1C), break (0xF0 0x1C).
    dev.injectKeyboardBytes(new Uint8Array([0x1c, 0xf0, 0x1c]));

    // First translated byte should be ready and attributed to the keyboard.
    let st = dev.portRead(0x64, 1);
    expect(st & 0x01).toBe(0x01); // OBF
    expect(st & 0x20).toBe(0x00); // AUX_OBF
    expect(dev.portRead(0x60, 1)).toBe(0x1e); // Set-1 'A' make

    st = dev.portRead(0x64, 1);
    expect(st & 0x01).toBe(0x01);
    expect(st & 0x20).toBe(0x00);
    expect(dev.portRead(0x60, 1)).toBe(0x9e); // Set-1 'A' break (make | 0x80)

    expect(irqSink.raiseIrq).toHaveBeenCalledWith(1);
    expect(irqSink.raiseIrq).not.toHaveBeenCalledWith(12);
  });

  it("passes through raw Set-2 keyboard scancodes when translation is disabled", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new I8042Controller(irqSink);

    // Disable Set-2 -> Set-1 translation (command byte bit 6) while leaving IRQ1 enabled.
    dev.portWrite(0x0064, 1, 0x60);
    dev.portWrite(0x0060, 1, 0x05);

    dev.injectKeyboardBytes(new Uint8Array([0x1c, 0xf0, 0x1c]));
    expect(drainAllOutputBytes(dev)).toEqual([0x1c, 0xf0, 0x1c]);
  });

  it("translates PrintScreen Set-2 scancode sequences to Set-1 multi-byte sequences", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new I8042Controller(irqSink);

    // PrintScreen make: E0 12 E0 7C -> E0 2A E0 37.
    dev.injectKeyboardBytes(new Uint8Array([0xe0, 0x12, 0xe0, 0x7c]));
    expect(drainAllOutputBytes(dev)).toEqual([0xe0, 0x2a, 0xe0, 0x37]);

    // PrintScreen break: E0 F0 7C E0 F0 12 -> E0 B7 E0 AA.
    dev.injectKeyboardBytes(new Uint8Array([0xe0, 0xf0, 0x7c, 0xe0, 0xf0, 0x12]));
    expect(drainAllOutputBytes(dev)).toEqual([0xe0, 0xb7, 0xe0, 0xaa]);
  });

  it("translates Pause Set-2 scancode sequence to Set-1 make-only sequence", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new I8042Controller(irqSink);

    // Pause make: E1 14 77 E1 F0 14 F0 77 -> E1 1D 45 E1 9D C5.
    dev.injectKeyboardBytes(new Uint8Array([0xe1, 0x14, 0x77, 0xe1, 0xf0, 0x14, 0xf0, 0x77]));
    expect(drainAllOutputBytes(dev)).toEqual([0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5]);
  });

  it("translates extended and numpad keys consistently with the Rust i8042 model", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new I8042Controller(irqSink);

    // MetaLeft make/break.
    dev.injectKeyboardBytes(new Uint8Array([0xe0, 0x1f]));
    expect(drainAllOutputBytes(dev)).toEqual([0xe0, 0x5b]);
    dev.injectKeyboardBytes(new Uint8Array([0xe0, 0xf0, 0x1f]));
    expect(drainAllOutputBytes(dev)).toEqual([0xe0, 0xdb]);

    // Numpad7 make/break (no E0 prefix).
    dev.injectKeyboardBytes(new Uint8Array([0x6c]));
    expect(drainAllOutputBytes(dev)).toEqual([0x47]);
    dev.injectKeyboardBytes(new Uint8Array([0xf0, 0x6c]));
    expect(drainAllOutputBytes(dev)).toEqual([0xc7]);
  });

  it("produces PS/2 mouse packets with AUX_OBF set and gates IRQ12 via the command byte", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new I8042Controller(irqSink);

    // Enable mouse reporting via the controller's "write-to-mouse" command (0xD4).
    // Default command byte does NOT enable IRQ12; ensure no IRQ is raised.
    dev.portWrite(0x64, 1, 0xd4);
    dev.portWrite(0x60, 1, 0xf4);

    let st = dev.portRead(0x64, 1);
    expect(st & 0x01).toBe(0x01); // OBF
    expect(st & 0x20).toBe(0x20); // AUX_OBF
    expect(dev.portRead(0x60, 1)).toBe(0xfa); // ACK

    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    // Enable IRQ12 (bit 1) while preserving the default 0x45 bits.
    dev.portWrite(0x64, 1, 0x60);
    dev.portWrite(0x60, 1, 0x47);

    dev.injectMouseMotion(10, 5, 0);

    const packet: number[] = [];
    for (let i = 0; i < 3; i++) {
      st = dev.portRead(0x64, 1);
      expect(st & 0x01).toBe(0x01);
      expect(st & 0x20).toBe(0x20);
      packet.push(dev.portRead(0x60, 1));
    }

    expect(packet).toEqual([0x08, 10, 5]);

    expect(irqSink.raiseIrq).toHaveBeenCalledWith(12);
    expect(irqSink.raiseIrq).not.toHaveBeenCalledWith(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledWith(12);
  });

  it("supports controller command byte read/write via 0x20/0x60", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new I8042Controller(irqSink);

    dev.portWrite(0x64, 1, 0x20);
    expect(dev.portRead(0x60, 1)).toBe(0x45);

    dev.portWrite(0x64, 1, 0x60);
    dev.portWrite(0x60, 1, 0x47);

    dev.portWrite(0x64, 1, 0x20);
    expect(dev.portRead(0x60, 1)).toBe(0x47);
  });
});
