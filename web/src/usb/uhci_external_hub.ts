import type { GuestUsbRootPort } from "../platform/hid_passthrough_protocol";

/**
 * UHCI guest-visible USB topology constants.
 *
 * Root ports:
 * - 0: external USB hub (for WebHID passthrough + synthetic HID devices)
 * - 1: reserved for WebUSB passthrough
 */
export const UHCI_ROOT_PORTS: readonly GuestUsbRootPort[] = [0, 1];
export const EXTERNAL_HUB_ROOT_PORT: GuestUsbRootPort = 0;
export const WEBUSB_GUEST_ROOT_PORT: GuestUsbRootPort = 1;

/**
 * Default downstream port count for the external hub on root port 0.
 *
 * Note: this is the *total* number of downstream ports on the hub, including the
 * reserved ports used for synthetic devices.
 */
export const DEFAULT_EXTERNAL_HUB_PORT_COUNT = 16;

/**
 * Synthetic browser input devices are exposed as fixed USB HID devices behind the
 * external hub on root port 0.
 */
export const UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT = 1;
export const UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT = 2;
export const UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT = 3;

export const UHCI_SYNTHETIC_HID_HUB_PORT_COUNT = 3;

/**
 * First hub port number that may be allocated for dynamic passthrough devices
 * (e.g. WebHID) without colliding with the built-in synthetic devices.
 */
export const UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT = UHCI_SYNTHETIC_HID_HUB_PORT_COUNT + 1;

/**
 * Backwards-compatible mapping for older callers that only specify a root-port-only
 * path (`[0]` or `[1]`).
 *
 * Those root ports are no longer directly attachable:
 * - root port 0 hosts the external hub
 * - root port 1 is reserved for WebUSB
 *
 * Remap `[0]` -> `[0, 4]` and `[1]` -> `[0, 5]` so legacy callers don't clobber
 * the synthetic keyboard/mouse/gamepad devices on hub ports 1..=UHCI_SYNTHETIC_HID_HUB_PORT_COUNT.
 */
export function remapLegacyRootPortToExternalHubPort(rootPort: number): number {
  return UHCI_SYNTHETIC_HID_HUB_PORT_COUNT + (rootPort >>> 0) + 1;
}
