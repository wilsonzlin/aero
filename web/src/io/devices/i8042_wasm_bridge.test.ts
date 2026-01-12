import { describe, expect, it } from "vitest";

import type { WasmApi } from "../../runtime/wasm_loader";
import { initWasm } from "../../runtime/wasm_loader";

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
    if (message.includes("Missing single-thread WASM package")) {
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

function drainOutput(bridge: I8042Bridge): number[] {
  const out: number[] = [];
  // i8042 queues are bounded; keep a hard stop to avoid an infinite loop in tests.
  for (let i = 0; i < 4096; i++) {
    const status = bridge.port_read(0x64) & 0xff;
    if ((status & 0x01) === 0) break;
    out.push(bridge.port_read(0x60) & 0xff);
  }
  return out;
}

function writeToMouse(bridge: I8042Bridge, byte: number): void {
  // i8042 command 0xD4: next data byte goes to the auxiliary (mouse) device.
  bridge.port_write(0x64, 0xd4);
  bridge.port_write(0x60, byte & 0xff);
}

describe("io/devices/i8042 WASM bridge", () => {
  it("translates injected Set-2 keyboard bytes to Set-1 by default", async () => {
    const bridge = await createBridge();
    if (!bridge) return;
    try {
      // Set-2 "A" make (0x1c).
      if (bridge.inject_keyboard_bytes) {
        bridge.inject_keyboard_bytes(new Uint8Array([0x1c]));
      } else {
        bridge.inject_key_scancode_bytes(0x1c, 1);
      }
      expect(bridge.port_read(0x64) & 0x01).toBe(1);
      expect(bridge.port_read(0x60)).toBe(0x1e); // Set-1 "A" make
    } finally {
      bridge.free();
    }
  });

  it("emits IntelliMouse packets after enabling wheel + reporting", async () => {
    const bridge = await createBridge();
    if (!bridge) return;
    try {
      // Enable IntelliMouse wheel extension: sample rate sequence 200, 100, 80.
      writeToMouse(bridge, 0xf3);
      writeToMouse(bridge, 200);
      writeToMouse(bridge, 0xf3);
      writeToMouse(bridge, 100);
      writeToMouse(bridge, 0xf3);
      writeToMouse(bridge, 80);

      // Set an initial button state *before* enabling reporting so we don't get an extra
      // "button-only" packet.
      if (bridge.inject_ps2_mouse_buttons) {
        bridge.inject_ps2_mouse_buttons(0x01);
      } else {
        bridge.inject_mouse_buttons(0x01);
      }

      // Enable reporting.
      writeToMouse(bridge, 0xf4);

      const acks = drainOutput(bridge);
      expect(acks.length).toBe(7);
      expect(acks.every((b) => b === 0xfa)).toBe(true);

      if (bridge.inject_ps2_mouse_motion) {
        bridge.inject_ps2_mouse_motion(5, 3, 1);
        const packet = drainOutput(bridge);
        expect(packet).toEqual([0x09, 0x05, 0x03, 0x01]);
      } else {
        // Older WASM builds expose mouse motion and wheel injection as separate calls.
        bridge.inject_mouse_move(5, 3);
        bridge.inject_mouse_wheel(1);
        const packets = drainOutput(bridge);
        expect(packets).toEqual([0x09, 0x05, 0x03, 0x00, 0x09, 0x00, 0x00, 0x01]);
      }
    } finally {
      bridge.free();
    }
  });

  it("splits large injected mouse deltas into multiple PS/2 packets", async () => {
    const bridge = await createBridge();
    if (!bridge) return;
    try {
      // Enable reporting.
      writeToMouse(bridge, 0xf4);
      // Drain the ACK.
      expect(drainOutput(bridge)).toEqual([0xfa]);

      bridge.inject_mouse_move(200, 0);
      expect(drainOutput(bridge)).toEqual([0x08, 0x7f, 0x00, 0x08, 0x49, 0x00]);
    } finally {
      bridge.free();
    }
  });

  it("clears IntelliMouse mode when the mouse receives Set Defaults (0xF6)", async () => {
    const bridge = await createBridge();
    if (!bridge) return;
    try {
      // Enable IntelliMouse wheel extension: sample rate sequence 200, 100, 80.
      writeToMouse(bridge, 0xf3);
      writeToMouse(bridge, 200);
      writeToMouse(bridge, 0xf3);
      writeToMouse(bridge, 100);
      writeToMouse(bridge, 0xf3);
      writeToMouse(bridge, 80);

      // Drain ACKs for the 3x (F3 + rate) sequence.
      const seqAcks = drainOutput(bridge);
      expect(seqAcks).toHaveLength(6);
      expect(seqAcks.every((b) => b === 0xfa)).toBe(true);

      // Verify device ID is 0x03.
      writeToMouse(bridge, 0xf2);
      expect(drainOutput(bridge)).toEqual([0xfa, 0x03]);

      // Set Defaults should reset to the base device ID.
      writeToMouse(bridge, 0xf6);
      expect(drainOutput(bridge)).toEqual([0xfa]);

      writeToMouse(bridge, 0xf2);
      expect(drainOutput(bridge)).toEqual([0xfa, 0x00]);
    } finally {
      bridge.free();
    }
  });

  it("save_state/load_state roundtrips pending output", async () => {
    const bridge1 = await createBridge();
    if (!bridge1) return;
    const bridge2 = await createBridge();
    if (!bridge2) {
      bridge1.free();
      return;
    }
    try {
      if (bridge1.inject_keyboard_bytes) {
        bridge1.inject_keyboard_bytes(new Uint8Array([0x1c]));
      } else {
        bridge1.inject_key_scancode_bytes(0x1c, 1);
      }
      const snap = bridge1.save_state();

      bridge2.load_state(snap);
      expect(bridge2.port_read(0x64) & 0x01).toBe(1);
      expect(bridge2.port_read(0x60)).toBe(0x1e);
    } finally {
      bridge1.free();
      bridge2.free();
    }
  });
});
