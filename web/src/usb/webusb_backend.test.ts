import { describe, expect, it } from "vitest";

import { dataViewToUint8Array, parseBmRequestType, validateControlTransferDirection } from "./webusb_backend";

describe("webusb_backend helpers", () => {
  it("maps bmRequestType to WebUSB {requestType, recipient}", () => {
    expect(parseBmRequestType(0x80)).toMatchObject({ requestType: "standard", recipient: "device" });
    expect(parseBmRequestType(0x21)).toMatchObject({ requestType: "class", recipient: "interface" });
    expect(parseBmRequestType(0xc2)).toMatchObject({ requestType: "vendor", recipient: "endpoint" });
    expect(parseBmRequestType(0xa3)).toMatchObject({ requestType: "class", recipient: "other" });
  });

  it("validates control transfer direction against bmRequestType", () => {
    expect(validateControlTransferDirection("controlIn", 0x80).ok).toBe(true);
    expect(validateControlTransferDirection("controlOut", 0x00).ok).toBe(true);

    const wrongIn = validateControlTransferDirection("controlIn", 0x00);
    expect(wrongIn.ok).toBe(false);
    if (!wrongIn.ok) expect(wrongIn.message).toContain("expected deviceToHost");

    const wrongOut = validateControlTransferDirection("controlOut", 0x80);
    expect(wrongOut.ok).toBe(false);
    if (!wrongOut.ok) expect(wrongOut.message).toContain("expected hostToDevice");
  });

  it("converts DataView to a trimmed Uint8Array copy", () => {
    const buf = Uint8Array.from([0xaa, 0xbb, 0xcc, 0xdd]).buffer;
    const view = new DataView(buf, 1, 2); // [0xbb, 0xcc]

    const out = dataViewToUint8Array(view);
    expect(Array.from(out)).toEqual([0xbb, 0xcc]);
    expect(out.byteLength).toBe(2);
  });
});

