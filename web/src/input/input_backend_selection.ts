export type InputBackend = "ps2" | "usb" | "virtio";

export function chooseKeyboardInputBackend(opts: {
  current: InputBackend;
  keysHeld: boolean;
  virtioOk: boolean;
  usbOk: boolean;
}): InputBackend {
  // Do not switch backends while any key is held down; switching mid-press risks leaving the
  // previous backend in a pressed state (no matching release), causing "stuck keys" in the guest.
  if (opts.keysHeld) return opts.current;

  if (opts.virtioOk) return "virtio";
  if (opts.usbOk) return "usb";
  return "ps2";
}

export function chooseMouseInputBackend(opts: {
  current: InputBackend;
  buttonsHeld: boolean;
  virtioOk: boolean;
  usbOk: boolean;
}): InputBackend {
  // Avoid switching mouse backends while any button is held down; switching mid-press risks
  // leaving the previous backend with a latched button-down state (stuck drag).
  if (opts.buttonsHeld) return opts.current;

  if (opts.virtioOk) return "virtio";
  if (opts.usbOk) return "usb";
  return "ps2";
}

