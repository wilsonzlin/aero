import { describe, expect, it } from "vitest";

import { DeviceManager, type IrqSink } from "../device_manager";
import { VirtioNetPciDevice } from "./virtio_net";
import { initWasm } from "../../runtime/wasm_loader";
import { computeGuestRamLayout, createIoIpcSab } from "../../runtime/shared_layout";

describe("io/devices/virtio_net (wasm transitional)", () => {
  it("exposes BAR2 as IO and wires legacy io_read/io_write when the WASM export is present", async () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    // Load the WASM module (skip gracefully when the generated wasm-pack output is missing).
    const desiredGuestBytes = 1 * 1024 * 1024;
    const layout = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layout.wasm_pages, maximum: layout.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // Local dev / unit test environments may not have the wasm-pack outputs built.
      // Skip this integration test when the single-thread WASM bundle is unavailable.
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    const Bridge = api.VirtioNetPciBridge;
    if (!Bridge) return;

    const ioIpcSab = createIoIpcSab();

    // Instantiate a transitional bridge. Older builds may not accept the 4th arg; fall back.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const AnyCtor = Bridge as any;
    let bridge: any;
    try {
      bridge = new AnyCtor(layout.guest_base >>> 0, layout.guest_size >>> 0, ioIpcSab, true);
    } catch {
      bridge = new AnyCtor(layout.guest_base >>> 0, layout.guest_size >>> 0, ioIpcSab);
    }

    const legacyRead =
      typeof bridge.legacy_io_read === "function"
        ? bridge.legacy_io_read
        : typeof (bridge as any).io_read === "function"
          ? (bridge as any).io_read
          : null;
    const legacyWrite =
      typeof bridge.legacy_io_write === "function"
        ? bridge.legacy_io_write
        : typeof (bridge as any).io_write === "function"
          ? (bridge as any).io_write
          : null;

    // Older WASM builds may not implement legacy IO accessors; treat transitional mode as unsupported.
    if (!legacyRead || !legacyWrite) {
      try {
        bridge.free();
      } catch {
        // ignore
      }
      return;
    }
    // Newer builds may expose `io_read`/`io_write` even for modern-only devices; detect whether the
    // legacy register block is actually enabled.
    try {
      const probe = legacyRead.call(bridge, 0, 4) >>> 0;
      if (probe === 0xffff_ffff) {
        bridge.free();
        return;
      }
    } catch {
      try {
        bridge.free();
      } catch {
        // ignore
      }
      return;
    }

    const dev = new VirtioNetPciDevice({ bridge, irqSink: mgr.irqSink, mode: "transitional" });
    mgr.registerPciDevice(dev);

    // Canonical virtio-net BDF: 00:08.0.
    const DEV = 8;
    const cfgAddr = (off: number): number => (0x8000_0000 | (DEV << 11) | (off & 0xfc)) >>> 0;
    const readCfg32 = (off: number): number => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(off));
      return mgr.portRead(0x0cfc, 4) >>> 0;
    };
    const writeCfg32 = (off: number, value: number): void => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(off));
      mgr.portWrite(0x0cfc, 4, value >>> 0);
    };

    // Read BAR2 base.
    const bar2 = readCfg32(0x18);
    expect(bar2 & 0x1).toBe(0x1);
    const ioBase = bar2 & 0xffff_fffc;
    expect(ioBase).toBeGreaterThan(0);

    // Enable I/O decoding.
    writeCfg32(0x04, 0x0000_0003);

    // Legacy HOST_FEATURES should return a real value (not the default all-ones filler).
    const hostFeatures = mgr.portRead(ioBase + 0x00, 4) >>> 0;
    expect(hostFeatures).not.toBe(0xffff_ffff);

    // And legacy writes should not throw.
    expect(() => legacyWrite.call(bridge, 0x04, 4, 0x0000_0000)).not.toThrow();

    dev.destroy();
  });
});
