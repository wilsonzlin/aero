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
  for (let i = 0; i < bytes.length; i += 4) {
    const b0 = bytes[i] ?? 0;
    const b1 = bytes[i + 1] ?? 0;
    const b2 = bytes[i + 2] ?? 0;
    const b3 = bytes[i + 3] ?? 0;
    const len = Math.min(4, bytes.length - i);
    const packed = (b0 & 0xff) | ((b1 & 0xff) << 8) | ((b2 & 0xff) << 16) | ((b3 & 0xff) << 24);
    bridge.inject_key_scancode_bytes(packed >>> 0, len);
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
      const assertDrainParity = (label: string): void => {
        const outTs = drainTsOutput(ts);
        const outWasm = drainWasmOutput(bridge);
        expect(outTs, label).toEqual(outWasm);
      };

      // Sanity: nothing buffered at start.
      assertDrainParity("initial output parity");

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

        ts.injectKeyboardBytes(Uint8Array.from(press));
        injectWasmKeyboardBytes(bridge, press);
        assertDrainParity(`keyboard press parity for ${code}`);

        ts.injectKeyboardBytes(Uint8Array.from(release));
        injectWasmKeyboardBytes(bridge, release);
        assertDrainParity(`keyboard release parity for ${code}`);
      }

      // --- Mouse parity ---
      // Enable mouse reporting via the real command path (0xD4 routes the next data byte to the mouse).
      writeToMouseTs(ts, 0xf4);
      writeToMouseWasm(bridge, 0xf4);
      assertDrainParity("mouse enable reporting ACK parity");

      const buttonMasks = [
        0x01, // left
        0x03, // left + right
        0x00, // release
      ];
      for (const mask of buttonMasks) {
        ts.injectMouseButtons(mask);
        bridge.inject_mouse_buttons(mask);
        assertDrainParity(`mouse buttons parity mask=${mask.toString(16)}`);
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
        assertDrainParity(`mouse motion parity dx=${dx} dy=${dy}`);
      }

      // Enable IntelliMouse wheel mode (200,100,80 sample rate sequence) so wheel injections
      // produce 4-byte packets.
      const wheelEnableSeq = [0xf3, 200, 0xf3, 100, 0xf3, 80];
      for (const b of wheelEnableSeq) {
        writeToMouseTs(ts, b);
        writeToMouseWasm(bridge, b);
        assertDrainParity(`mouse wheel-enable parity byte=0x${b.toString(16)}`);
      }

      // Confirm device ID now reports the wheel mouse (0x03). (Parity-only; we don't care about the exact value here.)
      writeToMouseTs(ts, 0xf2);
      writeToMouseWasm(bridge, 0xf2);
      assertDrainParity("mouse device id parity (wheel enabled)");

      // Motion packets should now include a 4th (wheel) byte of 0.
      ts.injectMouseMove(5, 3);
      bridge.inject_mouse_move(5, 3);
      assertDrainParity("mouse motion parity (wheel mode)");

      const wheels = [
        1,
        // Requires splitting/clamping to the IntelliMouse 4-bit signed wheel delta.
        20,
        -20,
      ];
      for (const dz of wheels) {
        ts.injectMouseWheel(dz);
        bridge.inject_mouse_wheel(dz);
        assertDrainParity(`mouse wheel parity dz=${dz}`);
      }
    } finally {
      bridge.free();
    }
  });
});
