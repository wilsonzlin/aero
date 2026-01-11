import { describe, expect, it } from "vitest";

import { formatHexBytes } from "./usb_hex";

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
});

