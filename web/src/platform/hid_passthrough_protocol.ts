export type GuestUsbRootPort = 0 | 1;

/**
 * @deprecated Prefer {@link GuestUsbPath}. This only represents a UHCI root port.
 */
export type GuestUsbPort = GuestUsbRootPort;

/**
 * Guest-side USB attachment path.
 *
 * - `guestPath[0]` is the UHCI root port index (0-based).
 * - `guestPath[1..]` are downstream hub port numbers (1-based, per USB spec).
 */
export type GuestUsbPath = number[];

export function isGuestUsbPath(value: unknown): value is GuestUsbPath {
  if (!Array.isArray(value)) return false;
  if (value.length === 0) return false;

  for (let i = 0; i < value.length; i += 1) {
    const part = value[i];
    if (typeof part !== "number") return false;
    if (!Number.isInteger(part)) return false;

    if (i === 0) {
      if (part !== 0 && part !== 1) return false;
      continue;
    }

    if (part < 1 || part > 255) return false;
  }

  return true;
}

type HidAttachMessageV0 = {
  type: "hid:attach";
  deviceId: string;
  guestPort: GuestUsbPort;
  /**
   * Optional for transition/interop with newer senders.
   */
  guestPath?: GuestUsbPath;
};

type HidAttachMessageV1 = {
  type: "hid:attach";
  deviceId: string;
  guestPath: GuestUsbPath;
  /**
   * @deprecated Present for backwards compatibility. When `guestPath` is set,
   * this should match `guestPath[0]`.
   */
  guestPort?: GuestUsbPort;
};

export type HidAttachMessage = HidAttachMessageV0 | HidAttachMessageV1;

type HidDetachMessageV0 = {
  type: "hid:detach";
  deviceId: string;
  guestPort?: GuestUsbPort;
  /**
   * Optional for transition/interop with newer senders.
   */
  guestPath?: GuestUsbPath;
};

type HidDetachMessageV1 = {
  type: "hid:detach";
  deviceId: string;
  guestPath?: GuestUsbPath;
  /**
   * @deprecated Present for backwards compatibility. When `guestPath` is set,
   * this should match `guestPath[0]`.
   */
  guestPort?: GuestUsbPort;
};

export type HidDetachMessage = HidDetachMessageV0 | HidDetachMessageV1;

export type HidPassthroughMessage = HidAttachMessage | HidDetachMessage;
