import { describe, expect, it } from "vitest";

import type { WasmApi } from "../../runtime/wasm_loader";
import { initWasm } from "../../runtime/wasm_loader";
import type { IrqSink } from "../device_manager";
import { ps2Set2BytesForKeyEvent } from "../../input/scancodes";
import { I8042Controller } from "./i8042";

type I8042Bridge = InstanceType<NonNullable<WasmApi["I8042Bridge"]>>;

let cachedApi: WasmApi | null = null;
let apiInitAttempted = false;

async function getWasmApi(): Promise<WasmApi | null> {
  if (apiInitAttempted) return cachedApi;
  apiInitAttempted = true;
  try {
    const { api } = await initWasm({ variant: "single" });
    cachedApi = api;
    return api;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    // The wasm-pack output is generated and may be absent in some test environments;
    // skip rather than failing unrelated suites.
    if (message.includes("Missing single") && message.includes("WASM package")) {
      cachedApi = null;
      return null;
    }
    throw err;
  }
}

async function createBridge(): Promise<I8042Bridge | null> {
  const api = await getWasmApi();
  if (!api?.I8042Bridge) return null;
  return new api.I8042Bridge();
}

function drainTsOutput(dev: I8042Controller): number[] {
  const out: number[] = [];
  // i8042 queues are bounded; keep a hard stop to avoid an infinite loop in tests.
  for (let i = 0; i < 4096; i++) {
    const status = dev.portRead(0x0064, 1) & 0xff;
    if ((status & 0x01) === 0) break;
    out.push(dev.portRead(0x0060, 1) & 0xff);
  }
  return out;
}

function drainWasmOutput(bridge: I8042Bridge): number[] {
  const out: number[] = [];
  // i8042 queues are bounded; keep a hard stop to avoid an infinite loop in tests.
  for (let i = 0; i < 4096; i++) {
    const status = bridge.port_read(0x64) & 0xff;
    if ((status & 0x01) === 0) break;
    out.push(bridge.port_read(0x60) & 0xff);
  }
  return out;
}

function injectWasmKeyboardBytes(bridge: I8042Bridge, bytes: readonly number[]): void {
  if (bytes.length === 0) return;
  if (bridge.inject_keyboard_bytes) {
    bridge.inject_keyboard_bytes(Uint8Array.from(bytes));
    return;
  }
  // Fall back to the stable `inject_key_scancode_bytes(packed, len)` API.
  for (const b of bytes) {
    bridge.inject_key_scancode_bytes(b & 0xff, 1);
  }
}

function writeToMouseTs(dev: I8042Controller, byte: number): void {
  dev.portWrite(0x0064, 1, 0xd4);
  dev.portWrite(0x0060, 1, byte & 0xff);
}

function writeToMouseWasm(bridge: I8042Bridge, byte: number): void {
  bridge.port_write(0x64, 0xd4);
  bridge.port_write(0x60, byte & 0xff);
}

describe("io/devices/i8042 TS <-> WASM parity", () => {
  it("produces identical output bytes for representative keyboard + mouse host injections", async () => {
    const bridge = await createBridge();
    if (!bridge) return;

    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const ts = new I8042Controller(irqSink);

    try {
      // Sanity: nothing buffered at start.
      expect(drainTsOutput(ts)).toEqual([]);
      expect(drainWasmOutput(bridge)).toEqual([]);

      const codes: string[] = [
        // Letters.
        "KeyA",
        "KeyM",
        "KeyZ",
        // Arrows (extended).
        "ArrowUp",
        "ArrowLeft",
        "ArrowDown",
        "ArrowRight",
        // Modifiers.
        "ShiftLeft",
        "ShiftRight",
        "ControlLeft",
        "ControlRight",
        "AltLeft",
        "AltRight",
        "MetaLeft",
        "MetaRight",
        // Multi-byte special keys.
        "PrintScreen",
        "Pause",
      ];

      for (const code of codes) {
        const press = ps2Set2BytesForKeyEvent(code, true);
        const release = ps2Set2BytesForKeyEvent(code, false);
        if (!press || !release) {
          throw new Error(`Missing PS/2 Set-2 mapping for DOM code: ${code}`);
        }

        const seq = [...press, ...release];

        ts.injectKeyboardBytes(Uint8Array.from(seq));
        injectWasmKeyboardBytes(bridge, seq);

        const outTs = drainTsOutput(ts);
        const outWasm = drainWasmOutput(bridge);
        expect(outTs, `keyboard parity for ${code}`).toEqual(outWasm);
      }

      // --- Mouse parity ---
      // Enable mouse reporting via the real command path (0xD4 routes the next data byte to the mouse).
      writeToMouseTs(ts, 0xf4);
      writeToMouseWasm(bridge, 0xf4);
      expect(drainTsOutput(ts), "mouse enable reporting ACK parity").toEqual(drainWasmOutput(bridge));

      const buttonMasks = [
        0x01, // left
        0x03, // left + right
        0x00, // release
      ];
      for (const mask of buttonMasks) {
        ts.injectMouseButtons(mask);
        bridge.inject_mouse_buttons(mask);
        expect(drainTsOutput(ts), `mouse buttons parity mask=${mask.toString(16)}`).toEqual(drainWasmOutput(bridge));
      }

      const motions: Array<{ dx: number; dy: number }> = [
        { dx: 5, dy: 3 },
        // Requires splitting into multiple packets (8-bit signed deltas).
        { dx: 200, dy: 0 },
        { dx: -200, dy: 0 },
        // Multiple-packet mixed-axis case (bounded runtime).
        { dx: 300, dy: -300 },
      ];
      for (const { dx, dy } of motions) {
        ts.injectMouseMove(dx, dy);
        bridge.inject_mouse_move(dx, dy);
        expect(drainTsOutput(ts), `mouse motion parity dx=${dx} dy=${dy}`).toEqual(drainWasmOutput(bridge));
      }
    } finally {
      bridge.free();
    }
  });
});

