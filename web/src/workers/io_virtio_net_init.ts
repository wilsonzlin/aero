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
    const base = opts.guestBase >>> 0;
    const size = opts.guestSize >>> 0;
    if (mode === "modern") {
      // Prefer the 3-arg signature for modern mode (matches the Aero Win7 virtio contract v1).
      // Some older/newer wasm-bindgen outputs may enforce arity; if 3 args fails, retry with a
      // 4th `transport_mode` argument that selects modern-only.
      try {
        bridge = new AnyCtor(base, size, opts.ioIpc);
      } catch {
        bridge = new AnyCtor(base, size, opts.ioIpc, false);
      }
    } else {
      // `transport_mode` accepts multiple encodings on the WASM side:
      // - `true` / `"transitional"` / `1` for transitional devices (legacy + modern)
      // - `"legacy"` / `2` for legacy-only
      // - `false` / `"modern"` / `0` for modern-only
      const arg = mode === "transitional" ? true : mode;
      try {
        bridge = new AnyCtor(base, size, opts.ioIpc, arg);
      } catch {
        bridge = new AnyCtor(base, size, opts.ioIpc);
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
    //
    // Note: virtio-pci legacy IO reads are gated by PCI command bit0 (I/O enable). For the probe
    // we temporarily enable I/O decoding inside the bridge so the read is meaningful.
    const setCmd = typeof bridgeAny.set_pci_command === "function" ? bridgeAny.set_pci_command : null;
    let ok = false;
    try {
      if (setCmd) {
        try {
          setCmd.call(bridge, 0x0001);
        } catch {
          // ignore
        }
      }
      const probe = (read.call(bridge, 0, 4) as number) >>> 0;
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
