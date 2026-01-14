import { describe, expect, it } from "vitest";

import type { WasmApi } from "../../runtime/wasm_loader";
import { initWasm } from "../../runtime/wasm_loader";
import type { IrqSink } from "../device_manager";
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

async function createDevices(): Promise<{ wasm: I8042Bridge; ts: I8042Controller } | null> {
  const api = await getWasmApi();
  if (!api?.I8042Bridge) return null;

  const wasm = new api.I8042Bridge();
  const irqSink: IrqSink = { raiseIrq() {}, lowerIrq() {} };
  const ts = new I8042Controller(irqSink, { systemControl: { setA20() {}, requestReset() {} } });
  return { wasm, ts };
}

interface I8042Ports {
  read(port: number): number;
  write(port: number, value: number): void;
}

function portsForWasm(dev: I8042Bridge): I8042Ports {
  return {
    read: (port) => dev.port_read(port) & 0xff,
    write: (port, value) => dev.port_write(port, value & 0xff),
  };
}

function portsForTs(dev: I8042Controller): I8042Ports {
  return {
    read: (port) => dev.portRead(port, 1) & 0xff,
    write: (port, value) => dev.portWrite(port, 1, value & 0xff),
  };
}

function drainOutput(dev: I8042Ports): number[] {
  const out: number[] = [];
  // The i8042 output buffer is bounded; keep a hard stop to avoid infinite loops in tests.
  for (let i = 0; i < 512; i++) {
    const status = dev.read(0x64) & 0xff;
    if ((status & 0x01) === 0) return out;
    out.push(dev.read(0x60) & 0xff);
  }
  throw new Error("i8042 output drain exceeded 512 iterations (possible stuck OBF).");
}

function writeToMouse(dev: I8042Ports, byte: number): void {
  // i8042 controller command 0xD4: next data byte goes to the auxiliary (mouse) device.
  dev.write(0x64, 0xd4);
  dev.write(0x60, byte & 0xff);
}

function injectKeyboardBytes(wasm: I8042Bridge, ts: I8042Controller, bytes: Uint8Array): void {
  ts.injectKeyboardBytes(bytes);
  if (wasm.inject_keyboard_bytes) wasm.inject_keyboard_bytes(bytes);
  else wasm.inject_key_scancode_bytes(bytes[0] ?? 0, bytes.length);
}

function injectMouseButtons(wasm: I8042Bridge, ts: I8042Controller, buttons: number): void {
  ts.injectMouseButtons(buttons);
  if (wasm.inject_ps2_mouse_buttons) wasm.inject_ps2_mouse_buttons(buttons & 0xff);
  else wasm.inject_mouse_buttons(buttons & 0xff);
}

function injectMouseMotion(
  wasm: I8042Bridge,
  ts: I8042Controller,
  dx: number,
  dy: number,
  wheel: number,
): { combined: boolean } {
  if (wasm.inject_ps2_mouse_motion) {
    wasm.inject_ps2_mouse_motion(dx | 0, dy | 0, wheel | 0);
    ts.injectMouseMotion(dx | 0, dy | 0, wheel | 0);
    return { combined: true };
  }

  wasm.inject_mouse_move(dx | 0, dy | 0);
  ts.injectMouseMove(dx | 0, dy | 0);
  if ((wheel | 0) !== 0) {
    wasm.inject_mouse_wheel(wheel | 0);
    ts.injectMouseWheel(wheel | 0);
  }
  return { combined: false };
}

describe("io/devices/i8042 TS-vs-WASM parity", () => {
  it("keyboard: injected Set-2 'A' make matches WASM (Set-1 translated output)", async () => {
    const devs = await createDevices();
    if (!devs) return;
    const { wasm, ts } = devs;
    try {
      injectKeyboardBytes(wasm, ts, new Uint8Array([0x1c])); // Set-2 "A" make

      const tsOut = drainOutput(portsForTs(ts));
      const wasmOut = drainOutput(portsForWasm(wasm));
      expect(tsOut).toEqual(wasmOut);
      expect(tsOut).toEqual([0x1e]); // Set-1 "A" make
    } finally {
      wasm.free();
    }
  });

  it("mouse: enable reporting then inject buttons+motion and match WASM packet bytes", async () => {
    const devs = await createDevices();
    if (!devs) return;
    const { wasm, ts } = devs;
    try {
      const tsPorts = portsForTs(ts);
      const wasmPorts = portsForWasm(wasm);

      // Enable mouse reporting (0xD4 0xF4).
      writeToMouse(tsPorts, 0xf4);
      writeToMouse(wasmPorts, 0xf4);

      // Toggle a single button bit to avoid multi-packet differences between APIs that
      // inject per-button deltas.
      injectMouseButtons(wasm, ts, 0x01);

      // Inject motion with a non-zero wheel delta. In default (non-IntelliMouse) mode the wheel
      // should be ignored, producing a standard 3-byte packet.
      injectMouseMotion(wasm, ts, 5, 3, 1);

      const tsOut = drainOutput(tsPorts);
      const wasmOut = drainOutput(wasmPorts);
      expect(tsOut).toEqual(wasmOut);
      expect(tsOut).toEqual([0xfa, 0x09, 0x00, 0x00, 0x09, 0x05, 0x03]);
    } finally {
      wasm.free();
    }
  });

  it("mouse: IntelliMouse enable sequence + ID query + wheel motion matches WASM", async () => {
    const devs = await createDevices();
    if (!devs) return;
    const { wasm, ts } = devs;
    try {
      const tsPorts = portsForTs(ts);
      const wasmPorts = portsForWasm(wasm);

      // IntelliMouse wheel extension sequence: sample rate 200, 100, 80.
      for (const rate of [200, 100, 80]) {
        writeToMouse(tsPorts, 0xf3);
        writeToMouse(wasmPorts, 0xf3);
        writeToMouse(tsPorts, rate);
        writeToMouse(wasmPorts, rate);
      }

      // ID query should report 0x03.
      writeToMouse(tsPorts, 0xf2);
      writeToMouse(wasmPorts, 0xf2);

      // Enable reporting.
      writeToMouse(tsPorts, 0xf4);
      writeToMouse(wasmPorts, 0xf4);

      const { combined } = injectMouseMotion(wasm, ts, 5, 3, 1);

      const tsOut = drainOutput(tsPorts);
      const wasmOut = drainOutput(wasmPorts);
      expect(tsOut).toEqual(wasmOut);

      const expected = combined
        ? // Combined motion+wheel injector emits a single 4-byte packet.
          [0xfa, 0xfa, 0xfa, 0xfa, 0xfa, 0xfa, 0xfa, 0x03, 0xfa, 0x08, 0x05, 0x03, 0x01]
        : // Older WASM builds inject motion + wheel as separate packets.
          [
            0xfa,
            0xfa,
            0xfa,
            0xfa,
            0xfa,
            0xfa,
            0xfa,
            0x03,
            0xfa,
            0x08,
            0x05,
            0x03,
            0x00,
            0x08,
            0x00,
            0x00,
            0x01,
          ];
      expect(tsOut).toEqual(expected);
    } finally {
      wasm.free();
    }
  });
});

