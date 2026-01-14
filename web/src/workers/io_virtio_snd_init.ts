import type { DeviceManager } from "../io/device_manager";
import { VirtioSndPciDevice, type VirtioSndPciMode } from "../io/devices/virtio_snd";
import type { WasmApi } from "../runtime/wasm_context";

export function tryInitVirtioSndDevice(opts: {
  api: WasmApi | null;
  mgr: DeviceManager | null;
  guestBase: number;
  guestSize: number;
  mode?: VirtioSndPciMode;
}): VirtioSndPciDevice | null {
  const api = opts.api;
  const mgr = opts.mgr;
  if (!api || !mgr) return null;
  if (!opts.guestBase) return null;
  if (!Number.isFinite(opts.guestSize) || opts.guestSize < 0) return null;

  const Bridge = api.VirtioSndPciBridge;
  if (!Bridge) return null;

  const mode: VirtioSndPciMode = opts.mode ?? "modern";

  type BridgeHandle = InstanceType<NonNullable<WasmApi["VirtioSndPciBridge"]>>;
  type AnyBridgeCtor = { new (...args: unknown[]): BridgeHandle };

  let bridge: BridgeHandle;
  try {
    // wasm-bindgen glue can enforce constructor arity. Prefer the newer 3-arg
    // signature (transport selector) when available, but fall back to older 2-arg
    // / 1-arg constructors.
    //
    const AnyCtor = Bridge as unknown as AnyBridgeCtor;
    const base = opts.guestBase >>> 0;
    const size = opts.guestSize >>> 0;
    if (mode === "modern") {
      // Prefer the default constructor for modern mode; some builds require an explicit
      // `transport_mode` argument, so try both arities.
      try {
        bridge = new AnyCtor(base, size, undefined);
      } catch {
        try {
          bridge = new AnyCtor(base, size);
        } catch {
          bridge = new AnyCtor(base);
        }
      }
    } else {
      // `transport_mode` accepts multiple encodings on the WASM side:
      // - `true` / `"transitional"` / `1` for transitional devices (legacy + modern)
      // - `"legacy"` / `2` for legacy-only
      // - `false` / `"modern"` / `0` for modern-only
      const transportArg = mode === "transitional" ? true : mode;
      try {
        bridge = new AnyCtor(base, size, transportArg);
      } catch {
        try {
          bridge = new AnyCtor(base, size);
        } catch {
          bridge = new AnyCtor(base);
        }
      }
    }
  } catch (err) {
    console.warn("[io.worker] Failed to initialize virtio-snd PCI bridge", err);
    return null;
  }

  if (mode !== "modern") {
    const anyBridge = bridge as unknown as Record<string, unknown>;
    const read =
      typeof anyBridge.legacy_io_read === "function"
        ? (anyBridge.legacy_io_read as (offset: number, size: number) => number)
        : typeof anyBridge.legacyIoRead === "function"
          ? (anyBridge.legacyIoRead as (offset: number, size: number) => number)
          : typeof anyBridge.io_read === "function"
            ? (anyBridge.io_read as (offset: number, size: number) => number)
            : typeof anyBridge.ioRead === "function"
              ? (anyBridge.ioRead as (offset: number, size: number) => number)
              : null;
    const write =
      typeof anyBridge.legacy_io_write === "function"
        ? (anyBridge.legacy_io_write as (offset: number, size: number, value: number) => void)
        : typeof anyBridge.legacyIoWrite === "function"
          ? (anyBridge.legacyIoWrite as (offset: number, size: number, value: number) => void)
          : typeof anyBridge.io_write === "function"
            ? (anyBridge.io_write as (offset: number, size: number, value: number) => void)
            : typeof anyBridge.ioWrite === "function"
              ? (anyBridge.ioWrite as (offset: number, size: number, value: number) => void)
              : null;
    if (!read || !write) {
      try {
        bridge.free();
      } catch {
        // ignore
      }
      return null;
    }

    // Some WASM builds may expose legacy IO methods even when the underlying transport is
    // modern-only. Detect whether legacy I/O is actually enabled via a basic HOST_FEATURES probe.
    //
    // Note: virtio-pci legacy IO reads are gated by PCI command bit0 (I/O enable). For the probe
    // we temporarily enable I/O decoding inside the bridge so the read is meaningful.
    const setCmdAny = anyBridge.set_pci_command ?? anyBridge.setPciCommand;
    const setCmd = typeof setCmdAny === "function" ? (setCmdAny as (command: number) => void) : null;
    let ok = false;
    try {
      if (setCmd) {
        try {
          setCmd.call(bridge, 0x0001);
        } catch {
          // ignore
        }
      }
      const probe = read.call(bridge, 0, 4) >>> 0;
      ok = probe !== 0xffff_ffff;
    } catch {
      ok = false;
    } finally {
      if (setCmd) {
        try {
          setCmd.call(bridge, 0x0000);
        } catch {
          // ignore
        }
      }
    }

    if (!ok) {
      try {
        bridge.free();
      } catch {
        // ignore
      }
      return null;
    }
  }

  try {
    const dev = new VirtioSndPciDevice({ bridge, irqSink: mgr.irqSink, mode });
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
