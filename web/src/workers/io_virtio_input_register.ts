import type { PciAddress, PciDevice } from "../io/bus/pci";
import type { DeviceManager } from "../io/device_manager";
import { VIRTIO_INPUT_PCI_DEVICE } from "../io/devices/virtio_input";

export function registerVirtioInputKeyboardPciFunction(opts: {
  mgr: DeviceManager;
  keyboardFn: PciDevice;
  log?: Pick<Console, "warn">;
}): { addr: PciAddress; usedCanonical: boolean } {
  const mgr = opts.mgr;
  const keyboardFn = opts.keyboardFn;
  const log = opts.log ?? console;

  const canonicalDev = VIRTIO_INPUT_PCI_DEVICE;
  const usedCanonical = mgr.pciBus.isDeviceNumberFree(canonicalDev);
  const devNum = usedCanonical ? canonicalDev : mgr.pciBus.allocDeviceNumber();
  if (!usedCanonical) {
    log.warn(
      `[io.worker] virtio-input canonical PCI device number 0:${canonicalDev}.0 is already in use; falling back to auto allocation`,
    );
  }

  const addr = mgr.registerPciDevice(keyboardFn, { device: devNum, function: 0 });
  // Keep `device.bdf` consistent with what was actually registered for debugging.
  (keyboardFn as unknown as { bdf?: PciAddress }).bdf = addr;
  return { addr, usedCanonical };
}
