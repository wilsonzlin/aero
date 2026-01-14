// Canonical WebUSB passthrough wire contract types.
//
// Keep in sync with:
// - Rust types: `crates/aero-usb/src/passthrough.rs`
// - Cross-language fixture: `docs/fixtures/webusb_passthrough_wire.json`
// - Stack selection ADR: `docs/adr/0015-canonical-usb-stack.md`

export interface SetupPacket {
  bmRequestType: number;
  bRequest: number;
  wValue: number;
  wIndex: number;
  wLength: number;
}

// `id` is a Rust-generated *non-zero* u32 used to correlate an action with its completion.
// Keep it representable as a JS number (`1..=0xffff_ffff`; `0` is reserved/invalid).
//
// For bulk transfers, `endpoint` is a USB endpoint *address* (not just the endpoint number):
// - IN endpoints have bit7 set (e.g. `0x81`)
// - OUT endpoints have bit7 clear (e.g. `0x02`)
export type UsbHostAction =
  | { kind: "controlIn"; id: number; setup: SetupPacket }
  | {
      kind: "controlOut";
      id: number;
      setup: SetupPacket;
      /** Data stage bytes (must match `setup.wLength`; use empty payload when `wLength` is 0). */
      data: Uint8Array;
    }
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
