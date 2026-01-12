import type { DeviceManager } from "../io/device_manager";
import { VirtioNetPciDevice } from "../io/devices/virtio_net";
import type { WasmApi } from "../runtime/wasm_context";

export function tryInitVirtioNetDevice(opts: {
  api: WasmApi | null;
  mgr: DeviceManager | null;
  guestBase: number;
  guestSize: number;
  ioIpc: SharedArrayBuffer | null;
}): VirtioNetPciDevice | null {
  const api = opts.api;
  const mgr = opts.mgr;
  if (!api || !mgr) return null;
  if (!opts.guestBase) return null;
  if (!Number.isFinite(opts.guestSize) || opts.guestSize < 0) return null;
  if (!opts.ioIpc) return null;

  const Bridge = api.VirtioNetPciBridge;
  if (!Bridge) return null;

  let bridge: InstanceType<NonNullable<WasmApi["VirtioNetPciBridge"]>>;
  try {
    bridge = new Bridge(opts.guestBase >>> 0, opts.guestSize >>> 0, opts.ioIpc);
  } catch (err) {
    console.warn("[io.worker] Failed to initialize virtio-net PCI bridge", err);
    return null;
  }

  try {
    const dev = new VirtioNetPciDevice({ bridge, irqSink: mgr.irqSink });
    // Match the canonical chipset layout used by Aero's native PCI profiles.
    mgr.registerPciDevice(dev, { device: 8, function: 0 });
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
