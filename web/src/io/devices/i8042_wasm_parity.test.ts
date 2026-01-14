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

function drainAndAssertParity(ts: I8042Controller, bridge: I8042Bridge, label: string): number[] {
  const out: number[] = [];
  // i8042 queues are bounded; keep a hard stop to avoid an infinite loop in tests.
  for (let i = 0; i < 4096; i++) {
    const stTs = ts.portRead(0x0064, 1) & 0xff;
    const stWasm = bridge.port_read(0x64) & 0xff;
    // Check output-buffer + AUX status parity. These bits are guest-observable and must stay
    // consistent when deciding whether a byte belongs to the keyboard or the auxiliary (mouse).
    expect(stTs & 0x21, `${label}: status OBF/AUX parity`).toBe(stWasm & 0x21);

    if ((stTs & 0x01) === 0) break;

    const bTs = ts.portRead(0x0060, 1) & 0xff;
    const bWasm = bridge.port_read(0x60) & 0xff;
    expect(bTs, `${label}: byte[${out.length}]`).toBe(bWasm);
    out.push(bTs);
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
      const assertDrainParity = (label: string): number[] => drainAndAssertParity(ts, bridge, label);

      // Sanity: nothing buffered at start.
      expect(assertDrainParity("initial output parity")).toEqual([]);

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
        const pressOut = assertDrainParity(`keyboard press parity for ${code}`);
        expect(pressOut.length, `keyboard press produced output for ${code}`).toBeGreaterThan(0);

        ts.injectKeyboardBytes(Uint8Array.from(release));
        injectWasmKeyboardBytes(bridge, release);
        const releaseOut = assertDrainParity(`keyboard release parity for ${code}`);
        if (code === "Pause") {
          // Pause is make-only (no break sequence).
          expect(releaseOut).toEqual([]);
        } else {
          expect(releaseOut.length, `keyboard release produced output for ${code}`).toBeGreaterThan(0);
        }
      }

      // --- Mouse parity ---
      // Enable mouse reporting via the real command path (0xD4 routes the next data byte to the mouse).
      writeToMouseTs(ts, 0xf4);
      writeToMouseWasm(bridge, 0xf4);
      expect(assertDrainParity("mouse enable reporting ACK parity")).toEqual([0xfa]);

      const buttonMasks = [
        0x01, // left
        0x03, // left + right
        0x00, // release
      ];
      for (const mask of buttonMasks) {
        ts.injectMouseButtons(mask);
        bridge.inject_mouse_buttons(mask);
        expect(assertDrainParity(`mouse buttons parity mask=${mask.toString(16)}`).length).toBeGreaterThan(0);
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
        expect(assertDrainParity(`mouse motion parity dx=${dx} dy=${dy}`).length).toBeGreaterThan(0);
      }

      // Enable IntelliMouse wheel mode (200,100,80 sample rate sequence) so wheel injections
      // produce 4-byte packets.
      const wheelEnableSeq = [0xf3, 200, 0xf3, 100, 0xf3, 80];
      for (const b of wheelEnableSeq) {
        writeToMouseTs(ts, b);
        writeToMouseWasm(bridge, b);
        expect(assertDrainParity(`mouse wheel-enable parity byte=0x${b.toString(16)}`)).toEqual([0xfa]);
      }

      // Confirm device ID now reports the wheel mouse (0x03). (Parity-only; we don't care about the exact value here.)
      writeToMouseTs(ts, 0xf2);
      writeToMouseWasm(bridge, 0xf2);
      expect(assertDrainParity("mouse device id parity (wheel enabled)").length).toBeGreaterThan(0);

      // Motion packets should now include a 4th (wheel) byte of 0.
      ts.injectMouseMove(5, 3);
      bridge.inject_mouse_move(5, 3);
      expect(assertDrainParity("mouse motion parity (wheel mode)")).toHaveLength(4);

      // Button-only packets should also include a 4th (wheel) byte in wheel mode.
      ts.injectMouseButtons(0x01);
      bridge.inject_mouse_buttons(0x01);
      expect(assertDrainParity("mouse buttons parity (wheel mode)")).toHaveLength(4);

      const wheels = [
        1,
        // Requires splitting/clamping to the IntelliMouse 4-bit signed wheel delta.
        20,
        -20,
      ];
      for (const dz of wheels) {
        ts.injectMouseWheel(dz);
        bridge.inject_mouse_wheel(dz);
        expect(assertDrainParity(`mouse wheel parity dz=${dz}`).length).toBeGreaterThan(0);
      }

      // Disable Set-2 -> Set-1 translation (command byte bit 6) and ensure raw Set-2 output stays in parity.
      ts.portWrite(0x0064, 1, 0x60);
      ts.portWrite(0x0060, 1, 0x05);
      bridge.port_write(0x64, 0x60);
      bridge.port_write(0x60, 0x05);
      expect(assertDrainParity("disable translation command parity")).toEqual([]);

      const rawCode = "ArrowUp";
      const rawPress = ps2Set2BytesForKeyEvent(rawCode, true)!;
      const rawRelease = ps2Set2BytesForKeyEvent(rawCode, false)!;
      ts.injectKeyboardBytes(Uint8Array.from(rawPress));
      injectWasmKeyboardBytes(bridge, rawPress);
      expect(assertDrainParity(`keyboard raw press parity for ${rawCode}`)).toEqual(rawPress.map((b) => b & 0xff));
      ts.injectKeyboardBytes(Uint8Array.from(rawRelease));
      injectWasmKeyboardBytes(bridge, rawRelease);
      expect(assertDrainParity(`keyboard raw release parity for ${rawCode}`)).toEqual(rawRelease.map((b) => b & 0xff));
    } finally {
      bridge.free();
    }
  });
});
