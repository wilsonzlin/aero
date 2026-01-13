import { describe, expect, it } from "vitest";

import { HIGH_RAM_START, LOW_RAM_END, guestPaddrToRamOffset, guestRangeInBounds } from "./guest_ram_translate";

describe("arch/guest_ram_translate", () => {
  it("guestPaddrToRamOffset maps small RAM configurations identity", () => {
    const ramBytes = 0x2000;

    expect(guestPaddrToRamOffset(ramBytes, 0)).toBe(0);
    expect(guestPaddrToRamOffset(ramBytes, 0x1234)).toBe(0x1234);
    expect(guestPaddrToRamOffset(ramBytes, ramBytes - 1)).toBe(ramBytes - 1);
    expect(guestPaddrToRamOffset(ramBytes, ramBytes)).toBeNull();
  });

  it("guestPaddrToRamOffset rejects the ECAM/PCI hole and maps high RAM above 4GiB", () => {
    const ramBytes = LOW_RAM_END + 0x2000;

    expect(guestPaddrToRamOffset(ramBytes, 0)).toBe(0);
    expect(guestPaddrToRamOffset(ramBytes, LOW_RAM_END)).toBeNull();
    expect(guestPaddrToRamOffset(ramBytes, HIGH_RAM_START)).toBe(LOW_RAM_END);
    expect(guestPaddrToRamOffset(ramBytes, HIGH_RAM_START + 0x1fff)).toBe(LOW_RAM_END + 0x1fff);
    expect(guestPaddrToRamOffset(ramBytes, HIGH_RAM_START + 0x2000)).toBeNull();
  });

  it("guestRangeInBounds rejects ranges that touch the ECAM/PCI hole and accepts ranges in both RAM segments", () => {
    const ramBytes = LOW_RAM_END + 0x2000;
    const highEnd = HIGH_RAM_START + 0x2000;

    // Low RAM.
    expect(guestRangeInBounds(ramBytes, 0, 1)).toBe(true);
    expect(guestRangeInBounds(ramBytes, LOW_RAM_END - 4, 4)).toBe(true);
    expect(guestRangeInBounds(ramBytes, LOW_RAM_END - 4, 8)).toBe(false);

    // Hole.
    expect(guestRangeInBounds(ramBytes, LOW_RAM_END, 1)).toBe(false);
    expect(guestRangeInBounds(ramBytes, HIGH_RAM_START - 4, 4)).toBe(false);

    // High RAM.
    expect(guestRangeInBounds(ramBytes, HIGH_RAM_START, 1)).toBe(true);
    expect(guestRangeInBounds(ramBytes, HIGH_RAM_START + 0x1ff0, 0x10)).toBe(true);
    expect(guestRangeInBounds(ramBytes, HIGH_RAM_START + 0x1ff0, 0x20)).toBe(false);

    // Zero-length ranges may sit on segment boundaries.
    expect(guestRangeInBounds(ramBytes, LOW_RAM_END, 0)).toBe(true);
    expect(guestRangeInBounds(ramBytes, HIGH_RAM_START, 0)).toBe(true);
    expect(guestRangeInBounds(ramBytes, highEnd, 0)).toBe(true);
    expect(guestRangeInBounds(ramBytes, highEnd, 1)).toBe(false);
  });
});

