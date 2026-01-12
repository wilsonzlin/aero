import type { DeviceManager } from "../io/device_manager";
import { VirtioSndPciDevice } from "../io/devices/virtio_snd";
import type { WasmApi } from "../runtime/wasm_context";

export function tryInitVirtioSndDevice(opts: {
  api: WasmApi | null;
  mgr: DeviceManager | null;
  guestBase: number;
  guestSize: number;
}): VirtioSndPciDevice | null {
  const api = opts.api;
  const mgr = opts.mgr;
  if (!api || !mgr) return null;
  if (!opts.guestBase) return null;
  if (!Number.isFinite(opts.guestSize) || opts.guestSize < 0) return null;

  const Bridge = api.VirtioSndPciBridge;
  if (!Bridge) return null;

  let bridge: InstanceType<NonNullable<WasmApi["VirtioSndPciBridge"]>>;
  try {
    // wasm-bindgen glue can enforce constructor arity; prefer the modern 2-arg
    // signature but fall back to 1-arg for older bindings.
    //
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const AnyCtor = Bridge as any;
    const base = opts.guestBase >>> 0;
    const size = opts.guestSize >>> 0;
    try {
      bridge = AnyCtor.length >= 2 ? new AnyCtor(base, size) : new AnyCtor(base);
    } catch {
      bridge = AnyCtor.length >= 2 ? new AnyCtor(base) : new AnyCtor(base, size);
    }
  } catch (err) {
    console.warn("[io.worker] Failed to initialize virtio-snd PCI bridge", err);
    return null;
  }

  try {
    const dev = new VirtioSndPciDevice({ bridge: bridge as unknown as any, irqSink: mgr.irqSink });
    try {
      mgr.registerPciDevice(dev);
    } catch {
      // Fall back to auto-assigned BDF if the canonical slot is already occupied.
      (dev as unknown as { bdf?: undefined }).bdf = undefined;
      const addr = mgr.registerPciDevice(dev);
      // Keep bdf consistent with the actual assigned address (useful for debugging).
      (dev as unknown as { bdf?: typeof addr }).bdf = addr;
    }
    mgr.addTickable(dev);
    return dev;
  } catch (err) {
    console.warn("[io.worker] Failed to register virtio-snd PCI device", err);
    try {
      bridge.free();
    } catch {
      // ignore
    }
    return null;
  }
}

