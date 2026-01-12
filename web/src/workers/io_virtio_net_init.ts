import type { DeviceManager } from "../io/device_manager";
import { VirtioNetPciDevice, type VirtioNetPciMode } from "../io/devices/virtio_net";
import type { WasmApi } from "../runtime/wasm_context";

export function tryInitVirtioNetDevice(opts: {
  api: WasmApi | null;
  mgr: DeviceManager | null;
  guestBase: number;
  guestSize: number;
  ioIpc: SharedArrayBuffer | null;
  mode?: VirtioNetPciMode;
}): VirtioNetPciDevice | null {
  const api = opts.api;
  const mgr = opts.mgr;
  if (!api || !mgr) return null;
  if (!opts.guestBase) return null;
  if (!Number.isFinite(opts.guestSize) || opts.guestSize < 0) return null;
  if (!opts.ioIpc) return null;

  const Bridge = api.VirtioNetPciBridge;
  if (!Bridge) return null;

  const mode: VirtioNetPciMode = opts.mode ?? "modern";

  let bridge: InstanceType<NonNullable<WasmApi["VirtioNetPciBridge"]>>;
  try {
    // Some wasm-bindgen builds enforce constructor arity. Prefer the 4-arg
    // signature (transport selector) when available, but gracefully fall back to
    // the legacy 3-arg constructor.
    //
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const AnyCtor = Bridge as any;
    if (mode === "modern") {
      bridge = new AnyCtor(opts.guestBase >>> 0, opts.guestSize >>> 0, opts.ioIpc);
    } else {
      const arg = mode === "transitional" ? true : mode;
      try {
        bridge = new AnyCtor(opts.guestBase >>> 0, opts.guestSize >>> 0, opts.ioIpc, arg);
      } catch {
        bridge = new AnyCtor(opts.guestBase >>> 0, opts.guestSize >>> 0, opts.ioIpc);
      }
    }
  } catch (err) {
    console.warn("[io.worker] Failed to initialize virtio-net PCI bridge", err);
    return null;
  }

  if (mode !== "modern") {
    const bridgeAny = bridge as any;
    const read =
      typeof bridgeAny.legacy_io_read === "function"
        ? bridgeAny.legacy_io_read
        : typeof bridgeAny.io_read === "function"
          ? bridgeAny.io_read
          : null;
    const write =
      typeof bridgeAny.legacy_io_write === "function"
        ? bridgeAny.legacy_io_write
        : typeof bridgeAny.io_write === "function"
          ? bridgeAny.io_write
          : null;
    if (!read || !write) {
      try {
        bridge.free();
      } catch {
        // ignore
      }
      return null;
    }

    // Some builds may expose legacy IO methods even when the underlying transport is modern-only.
    // Detect whether legacy I/O is actually enabled via a basic HOST_FEATURES probe.
    try {
      const probe = (read.call(bridge, 0, 4) as number) >>> 0;
      if (probe === 0xffff_ffff) {
        bridge.free();
        return null;
      }
    } catch {
      try {
        bridge.free();
      } catch {
        // ignore
      }
      return null;
    }
  }

  try {
    const dev = new VirtioNetPciDevice({ bridge, irqSink: mgr.irqSink, mode });
    mgr.registerPciDevice(dev);
    mgr.addTickable(dev);
    return dev;
  } catch (err) {
    console.warn("[io.worker] Failed to register virtio-net PCI device", err);
    try {
      bridge.free();
    } catch {
      // ignore
    }
    return null;
  }
}
