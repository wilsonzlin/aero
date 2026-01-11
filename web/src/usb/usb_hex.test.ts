import { describe, expect, it } from "vitest";

import { formatHexBytes, hex16, hex8 } from "./usb_hex";

describe("usb/formatHexBytes", () => {
  it("formats bytes into 16-byte rows", () => {
    const bytes = Uint8Array.from({ length: 17 }, (_, i) => i);
    expect(formatHexBytes(bytes)).toBe(
      [
        "00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f",
        "10",
      ].join("\n"),
    );
  });

  it("adds a truncation suffix when maxBytes is exceeded", () => {
    const bytes = Uint8Array.from({ length: 20 }, (_, i) => i);
    expect(formatHexBytes(bytes, 16)).toBe(
      [
        "00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f",
        "… (+4 bytes)",
      ].join("\n"),
    );
  });

  it("does not emit a leading newline when truncating with maxBytes=0", () => {
    expect(formatHexBytes(Uint8Array.of(1, 2, 3), 0)).toBe("… (+3 bytes)");
  });

  it("formats numbers as padded hex", () => {
    expect(hex8(0)).toBe("0x00");
    expect(hex8(255)).toBe("0xff");
    // Do not truncate: if out-of-range values are passed, show the full value to aid debugging.
    expect(hex8(0x1ff)).toBe("0x1ff");
    expect(hex16(0)).toBe("0x0000");
    expect(hex16(0x1234)).toBe("0x1234");
    expect(hex16(0x1_0000)).toBe("0x10000");
  });
});
