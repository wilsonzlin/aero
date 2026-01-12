import { describe, expect, it } from "vitest";

import type { IrqSink } from "../device_manager";
import { I8042Controller } from "./i8042";

function drainPort60(ctrl: I8042Controller, limit = 10000): number[] {
  const out: number[] = [];
  for (let i = 0; i < limit; i++) {
    const status = ctrl.portRead(0x64, 1) & 0xff;
    if ((status & 0x01) === 0) break;
    out.push(ctrl.portRead(0x60, 1) & 0xff);
  }
  return out;
}

function makeController(): I8042Controller {
  const irq: IrqSink = {
    raiseIrq: () => {},
    lowerIrq: () => {},
  };
  return new I8042Controller(irq);
}

describe("io/devices/i8042 snapshot", () => {
  it("round-trips pending keyboard and mouse bytes", () => {
    const ctrl = makeController();

    ctrl.injectKeyboardBytes(Uint8Array.of(0x1c, 0xf0, 0x1c));

    // Ask the mouse for its device ID (controller command 0xD4 routes the next data byte to the mouse).
    ctrl.portWrite(0x64, 1, 0xd4);
    ctrl.portWrite(0x60, 1, 0xf2);

    const snap = ctrl.saveState();

    const restored = makeController();
    restored.loadState(snap);

    // Default i8042 command byte enables Set-2 -> Set-1 translation, so KeyA
    // make/break becomes 0x1E/0x9E.
    expect(drainPort60(restored)).toEqual([0x1e, 0x9e, 0xfa, 0x00]);
  });

  it("preserves pending controller command state (0xD4 awaiting data) across restore", () => {
    const ctrl = makeController();
    ctrl.portWrite(0x64, 1, 0xd4);
    const snap = ctrl.saveState();

    const restored = makeController();
    restored.loadState(snap);

    // This byte should be routed to the mouse (device ID response is 0x00) rather than the keyboard.
    restored.portWrite(0x60, 1, 0xf2);
    expect(drainPort60(restored)).toEqual([0xfa, 0x00]);
  });

  it("truncates oversized output queues during load", () => {
    const max = I8042Controller.MAX_CONTROLLER_OUTPUT_QUEUE;
    const rawLen = max + 10;
    // Snapshot layout matches `I8042Controller.saveState()`:
    // - 16-byte `aero-io-snapshot` header ("AERO", format/device versions, device id "8042")
    // - controller fields + out queue
    // - keyboard state
    // - mouse state
    const totalLen = 65 + rawLen * 2;
    const bytes = new Uint8Array(totalLen);
    const view = new DataView(bytes.buffer);

    let off = 0;
    // Header.
    bytes[off++] = 0x41; // A
    bytes[off++] = 0x45; // E
    bytes[off++] = 0x52; // R
    bytes[off++] = 0x4f; // O
    view.setUint16(off, 1, true);
    off += 2;
    view.setUint16(off, 0, true);
    off += 2;
    // device id = "8042"
    bytes[off++] = 0x38;
    bytes[off++] = 0x30;
    bytes[off++] = 0x34;
    bytes[off++] = 0x32;
    view.setUint16(off, 1, true);
    off += 2;
    view.setUint16(off, 0, true);
    off += 2;

    // Controller fields.
    bytes[off++] = 0x04; // STATUS_SYS
    bytes[off++] = 0x00; // command byte
    bytes[off++] = 0x01; // output port
    bytes[off++] = 0xff; // pending command = null
    view.setUint32(off, rawLen, true);
    off += 4;

    for (let i = 0; i < rawLen; i++) {
      bytes[off++] = i < max ? 0x11 : 0x22; // value
      bytes[off++] = 1; // source = keyboard
    }

    // Keyboard state (defaults, empty out queue).
    bytes[off++] = 2; // scancode set
    bytes[off++] = 0; // leds
    bytes[off++] = 0x0b; // typematic delay
    bytes[off++] = 0x0b; // typematic rate
    bytes[off++] = 1; // scanning enabled
    bytes[off++] = 0; // expecting data
    bytes[off++] = 0; // last command
    bytes[off++] = 0; // padding
    view.setUint32(off, 0, true); // kbd out queue len
    off += 4;

    // Mouse state (defaults, empty out queue).
    bytes[off++] = 0; // mode=stream
    bytes[off++] = 0; // scaling=linear
    bytes[off++] = 4; // resolution
    bytes[off++] = 100; // sample rate
    bytes[off++] = 0; // reporting disabled
    bytes[off++] = 0; // device id
    bytes[off++] = 0; // buttons
    bytes[off++] = 0; // expecting data
    bytes[off++] = 0; // last command
    bytes[off++] = 0; // seqLen
    bytes[off++] = 0;
    bytes[off++] = 0;
    bytes[off++] = 0;
    view.setInt32(off, 0, true);
    off += 4;
    view.setInt32(off, 0, true);
    off += 4;
    view.setInt32(off, 0, true);
    off += 4;
    view.setUint32(off, 0, true); // mouse out queue len
    off += 4;

    expect(off).toBe(totalLen);

    const ctrl = makeController();
    ctrl.loadState(bytes);
    const drained = drainPort60(ctrl);

    expect(drained).toHaveLength(max);
    expect(new Set(drained)).toEqual(new Set([0x11]));
  });

  it("resynchronizes the A20 gate via systemControl sink on restore", () => {
    const ctrl = makeController();
    // Non-standard enable-A20 command supported by the model.
    ctrl.portWrite(0x64, 1, 0xdf);
    const snap = ctrl.saveState();

    const calls: boolean[] = [];
    const irq: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const restored = new I8042Controller(irq, {
      systemControl: {
        setA20: (enabled) => calls.push(Boolean(enabled)),
        requestReset: () => {},
      },
    });
    restored.loadState(snap);

    expect(calls).toEqual([true]);
  });
});
