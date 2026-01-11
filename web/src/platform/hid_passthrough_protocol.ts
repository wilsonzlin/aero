export type GuestUsbPort = 0 | 1;

export type HidAttachMessage = {
  type: "hid:attach";
  deviceId: string;
  guestPort: GuestUsbPort;
};

export type HidDetachMessage = {
  type: "hid:detach";
  deviceId: string;
  guestPort?: GuestUsbPort;
};

export type HidPassthroughMessage = HidAttachMessage | HidDetachMessage;
