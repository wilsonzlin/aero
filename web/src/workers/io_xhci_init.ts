import type { DeviceManager } from "../io/device_manager";
import { XhciPciDevice } from "../io/devices/xhci";
import type { WasmApi } from "../runtime/wasm_context";

export type XhciInitResult = {
  device: XhciPciDevice;
  bridge: InstanceType<NonNullable<WasmApi["XhciControllerBridge"]>>;
};

export function tryInitXhciDevice(opts: {
  api: WasmApi | null;
  mgr: DeviceManager | null;
  guestBase: number;
  guestSize: number;
}): XhciInitResult | null {
  const api = opts.api;
  const mgr = opts.mgr;
  if (!api || !mgr) return null;
  if (!opts.guestBase) return null;
  if (!Number.isFinite(opts.guestSize) || opts.guestSize < 0) return null;

  const Bridge = api.XhciControllerBridge;
  if (!Bridge) return null;

  let bridge: InstanceType<NonNullable<WasmApi["XhciControllerBridge"]>>;
  try {
    // `XhciControllerBridge` may have multiple wasm-bindgen constructor signatures depending on
    // the deployed WASM build:
    // - legacy: `new (guestBase)`
    // - current: `new (guestBase, guestSize)` (guestSize=0 means "use remainder of linear memory")
    //
    // wasm-bindgen glue sometimes enforces constructor arity, so pick based on `length` and
    // fall back to the other variant if instantiation fails.
    const base = opts.guestBase >>> 0;
    const size = opts.guestSize >>> 0;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const Ctor = Bridge as any;
    try {
      bridge = (Ctor.length >= 2 ? new Ctor(base, size) : new Ctor(base)) as typeof bridge;
    } catch {
      bridge = (Ctor.length >= 2 ? new Ctor(base) : new Ctor(base, size)) as typeof bridge;
    }
  } catch (err) {
    console.warn("[io.worker] Failed to initialize xHCI controller bridge", err);
    return null;
  }

  try {
    const dev = new XhciPciDevice({ bridge: bridge as unknown as any, irqSink: mgr.irqSink });
    // Prefer the canonical BDF requested by the device (00:02.0). If that slot is occupied,
    // fall back to auto allocation so xHCI can still attach in test/experimental setups.
    const anyDev = dev as unknown as { bdf?: { bus: number; device: number; function: number } };
    try {
      const addr = mgr.registerPciDevice(dev);
      anyDev.bdf = addr;
    } catch (err) {
      const prevBdf = anyDev.bdf;
      try {
        anyDev.bdf = undefined;
        const addr = mgr.registerPciDevice(dev);
        anyDev.bdf = addr;
      } catch (err2) {
        anyDev.bdf = prevBdf;
        throw err2;
      }
    }
    mgr.addTickable(dev);
    return { device: dev, bridge };
  } catch (err) {
    console.warn("[io.worker] Failed to register xHCI PCI device", err);
    try {
      bridge.free();
    } catch {
      // ignore
    }
    return null;
  }
}
