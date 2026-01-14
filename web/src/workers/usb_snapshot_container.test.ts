import { describe, expect, it } from "vitest";

import {
  decodeUsbSnapshotContainer,
  encodeUsbSnapshotContainer,
  isUsbSnapshotContainer,
  USB_SNAPSHOT_TAG_EHCI,
  USB_SNAPSHOT_TAG_UHCI,
  USB_SNAPSHOT_TAG_XHCI,
} from "./usb_snapshot_container";

describe("workers/usb_snapshot_container", () => {
  it("encodes entries in a deterministic tag-sorted order (input order independent)", () => {
    const uhci = new Uint8Array([0x01]);
    const ehci = new Uint8Array([0x02, 0x03]);
    const xhci = new Uint8Array([0xaa]);

    const a = encodeUsbSnapshotContainer([
      { tag: USB_SNAPSHOT_TAG_XHCI, bytes: xhci },
      { tag: USB_SNAPSHOT_TAG_UHCI, bytes: uhci },
      { tag: USB_SNAPSHOT_TAG_EHCI, bytes: ehci },
    ]);
    const b = encodeUsbSnapshotContainer([
      { tag: USB_SNAPSHOT_TAG_EHCI, bytes: ehci },
      { tag: USB_SNAPSHOT_TAG_UHCI, bytes: uhci },
      { tag: USB_SNAPSHOT_TAG_XHCI, bytes: xhci },
    ]);

    expect(a).toEqual(b);
    expect(isUsbSnapshotContainer(a)).toBe(true);

    const decoded = decodeUsbSnapshotContainer(a);
    expect(decoded).not.toBeNull();
    expect(decoded!.entries.map((e) => e.tag)).toEqual([USB_SNAPSHOT_TAG_EHCI, USB_SNAPSHOT_TAG_UHCI, USB_SNAPSHOT_TAG_XHCI]);
  });

  it("rejects invalid tags on encode", () => {
    expect(() => encodeUsbSnapshotContainer([{ tag: "BAD", bytes: new Uint8Array([1]) }])).toThrow();
    expect(() => encodeUsbSnapshotContainer([{ tag: "UH\u0000I", bytes: new Uint8Array([1]) }])).toThrow();
  });
});

