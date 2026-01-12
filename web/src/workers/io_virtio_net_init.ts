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
  const transitional = mode === "transitional";

  let bridge: InstanceType<NonNullable<WasmApi["VirtioNetPciBridge"]>>;
  try {
    // Some wasm-bindgen builds enforce constructor arity. Prefer the 4-arg
    // signature (`transitional`) when available, but gracefully fall back.
    //
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const AnyCtor = Bridge as any;
    try {
      bridge = new AnyCtor(opts.guestBase >>> 0, opts.guestSize >>> 0, opts.ioIpc, transitional);
    } catch {
      bridge = new AnyCtor(opts.guestBase >>> 0, opts.guestSize >>> 0, opts.ioIpc);
    }
    if (transitional) {
      const ioRead = (bridge as any).io_read;
      if (typeof ioRead !== "function") {
        // Transitional requested but legacy I/O accessors are not present.
        try {
          bridge.free();
        } catch {
          // ignore
        }
        return null;
      }
      // Some builds may expose `io_read` even when the underlying transport is modern-only.
      // Detect whether legacy I/O is actually enabled via a basic HOST_FEATURES probe.
      try {
        const probe = (ioRead.call(bridge, 0, 4) as number) >>> 0;
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
  } catch (err) {
    console.warn("[io.worker] Failed to initialize virtio-net PCI bridge", err);
    return null;
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
