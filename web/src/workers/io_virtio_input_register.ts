import type { PciAddress, PciDevice } from "../io/bus/pci";
import type { DeviceManager } from "../io/device_manager";
import { VIRTIO_INPUT_PCI_DEVICE } from "../io/devices/virtio_input";

function isPciAddrInUseError(err: unknown): boolean {
  return err instanceof Error && /PCI address already in use/i.test(err.message);
}

export function registerVirtioInputKeyboardPciFunction(opts: {
  mgr: DeviceManager;
  keyboardFn: PciDevice;
  log?: Pick<Console, "warn">;
}): { addr: PciAddress; usedCanonical: boolean } {
  const mgr = opts.mgr;
  const keyboardFn = opts.keyboardFn;
  const log = opts.log ?? console;

  try {
    const addr = mgr.registerPciDevice(keyboardFn, { device: VIRTIO_INPUT_PCI_DEVICE, function: 0 });
    // Keep `device.bdf` consistent with what was actually registered for debugging.
    (keyboardFn as unknown as { bdf?: PciAddress }).bdf = addr;
    return { addr, usedCanonical: true };
  } catch (err) {
    if (!isPciAddrInUseError(err)) throw err;
    log.warn(
      `[io.worker] virtio-input keyboard PCI address 0:${VIRTIO_INPUT_PCI_DEVICE}.0 is already in use; falling back to auto allocation`,
      err,
    );

    // `VirtioInputPciFunction` provides a canonical `bdf` value, but we want the PCI bus
    // allocator to pick a free slot when the canonical address is unavailable. Temporarily clear
    // `bdf` so registerPciDevice() uses auto allocation instead.
    const anyDev = keyboardFn as unknown as { bdf?: PciAddress };
    const prevBdf = anyDev.bdf;
    try {
      anyDev.bdf = undefined;
      const addr = mgr.registerPciDevice(keyboardFn, { function: 0 });
      anyDev.bdf = addr;
      return { addr, usedCanonical: false };
    } catch (err2) {
      anyDev.bdf = prevBdf;
      throw err2;
    }
  }
}

