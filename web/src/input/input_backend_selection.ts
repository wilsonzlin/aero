export type InputBackend = "ps2" | "usb" | "virtio";
export type InputBackendOverride = "auto" | InputBackend;

export function chooseKeyboardInputBackend(opts: {
  current: InputBackend;
  keysHeld: boolean;
  virtioOk: boolean;
  usbOk: boolean;
  force?: InputBackendOverride;
}): InputBackend {
  // Do not switch backends while any key is held down; switching mid-press risks leaving the
  // previous backend in a pressed state (no matching release), causing "stuck keys" in the guest.
  if (opts.keysHeld) return opts.current;

  if (opts.force && opts.force !== "auto") {
    if (opts.force === "virtio" && opts.virtioOk) return "virtio";
    if (opts.force === "usb" && opts.usbOk) return "usb";
    if (opts.force === "ps2") return "ps2";
    // Forced backend requested, but unavailable right now; fall back to the normal
    // auto-selection heuristics.
  }

  if (opts.virtioOk) return "virtio";
  if (opts.usbOk) return "usb";
  return "ps2";
}

export function chooseMouseInputBackend(opts: {
  current: InputBackend;
  buttonsHeld: boolean;
  virtioOk: boolean;
  usbOk: boolean;
  force?: InputBackendOverride;
}): InputBackend {
  // Avoid switching mouse backends while any button is held down; switching mid-press risks
  // leaving the previous backend with a latched button-down state (stuck drag).
  if (opts.buttonsHeld) return opts.current;

  if (opts.force && opts.force !== "auto") {
    if (opts.force === "virtio" && opts.virtioOk) return "virtio";
    if (opts.force === "usb" && opts.usbOk) return "usb";
    if (opts.force === "ps2") return "ps2";
    // Forced backend requested, but unavailable right now; fall back to the normal
    // auto-selection heuristics.
  }

  if (opts.virtioOk) return "virtio";
  if (opts.usbOk) return "usb";
  return "ps2";
}
