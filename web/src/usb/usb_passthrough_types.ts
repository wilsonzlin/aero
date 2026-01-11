export interface SetupPacket {
  bmRequestType: number;
  bRequest: number;
  wValue: number;
  wIndex: number;
  wLength: number;
}

export type UsbHostAction =
  | { kind: "controlIn"; id: number; setup: SetupPacket }
  | { kind: "controlOut"; id: number; setup: SetupPacket; data: Uint8Array }
  | { kind: "bulkIn"; id: number; endpoint: number; length: number }
  | { kind: "bulkOut"; id: number; endpoint: number; data: Uint8Array };

export type UsbHostCompletion =
  | { kind: "controlIn"; id: number; status: "success"; data: Uint8Array }
  | { kind: "controlIn"; id: number; status: "stall" }
  | { kind: "controlIn"; id: number; status: "error"; message: string }
  | { kind: "controlOut"; id: number; status: "success"; bytesWritten: number }
  | { kind: "controlOut"; id: number; status: "stall" }
  | { kind: "controlOut"; id: number; status: "error"; message: string }
  | { kind: "bulkIn"; id: number; status: "success"; data: Uint8Array }
  | { kind: "bulkIn"; id: number; status: "stall" }
  | { kind: "bulkIn"; id: number; status: "error"; message: string }
  | { kind: "bulkOut"; id: number; status: "success"; bytesWritten: number }
  | { kind: "bulkOut"; id: number; status: "stall" }
  | { kind: "bulkOut"; id: number; status: "error"; message: string };

