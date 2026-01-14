import { describe, expect, it, vi } from "vitest";

import { SCANOUT_FORMAT_B5G5R5A1, SCANOUT_FORMAT_B5G6R5 } from "../ipc/scanout_state";

describe("runtime/scanout_readback (16bpp slow path)", () => {
  it("does not require last-row pitch padding for non-contiguous B5G6R5 scanout (translates per-row)", async () => {
    vi.resetModules();

    const width = 2;
    const height = 2;
    const pitchBytes = 6; // padded (srcRowBytes=4)
    const srcRowBytes = width * 2;
    const requiredSrcBytes = pitchBytes * (height - 1) + srcRowBytes;

    const calls = { guestPaddrToRamOffset: 0 };

    // Force the helper to take the slow path by making the whole-range check fail, while still
    // allowing per-row validation to succeed.
    vi.doMock("../arch/guest_ram_translate.ts", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../arch/guest_ram_translate.ts")>();
      return {
        ...actual,
        guestRangeInBounds: (ramBytes: number, paddr: number, len: number): boolean => {
          if (len === requiredSrcBytes) return false;
          return actual.guestRangeInBounds(ramBytes, paddr, len);
        },
        guestPaddrToRamOffset: (ramBytes: number, paddr: number): number | null => {
          calls.guestPaddrToRamOffset += 1;
          return actual.guestPaddrToRamOffset(ramBytes, paddr);
        },
      };
    });

    const { readScanoutRgba8FromGuestRam } = await import("./scanout_readback");

    const guest = new Uint8Array(requiredSrcBytes);
    guest.fill(0xee);

    // Row 0 @ offset 0: red, green (RGB565 little-endian).
    guest.set(
      [
        0x00, 0xf8, // red   = 0xF800
        0xe0, 0x07, // green = 0x07E0
      ],
      0,
    );
    // Row 1 @ offset pitchBytes: blue, white.
    guest.set(
      [
        0x1f, 0x00, // blue  = 0x001F
        0xff, 0xff, // white = 0xFFFF
      ],
      pitchBytes,
    );

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B5G6R5,
    });

    // Slow path should translate once per row.
    expect(calls.guestPaddrToRamOffset).toBe(height);

    expect(Array.from(out.rgba8)).toEqual([
      // Row 0: red, green.
      0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
      // Row 1: blue, white.
      0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ]);
  });

  it("does not require last-row pitch padding for non-contiguous B5G5R5A1 scanout (translates per-row)", async () => {
    vi.resetModules();

    const width = 2;
    const height = 2;
    const pitchBytes = 6; // padded (srcRowBytes=4)
    const srcRowBytes = width * 2;
    const requiredSrcBytes = pitchBytes * (height - 1) + srcRowBytes;

    const calls = { guestPaddrToRamOffset: 0 };

    vi.doMock("../arch/guest_ram_translate.ts", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../arch/guest_ram_translate.ts")>();
      return {
        ...actual,
        guestRangeInBounds: (ramBytes: number, paddr: number, len: number): boolean => {
          if (len === requiredSrcBytes) return false;
          return actual.guestRangeInBounds(ramBytes, paddr, len);
        },
        guestPaddrToRamOffset: (ramBytes: number, paddr: number): number | null => {
          calls.guestPaddrToRamOffset += 1;
          return actual.guestPaddrToRamOffset(ramBytes, paddr);
        },
      };
    });

    const { readScanoutRgba8FromGuestRam } = await import("./scanout_readback");

    const guest = new Uint8Array(requiredSrcBytes);
    guest.fill(0xee);

    // Row 0 @ offset 0:
    // - pixel0: red, A=1 (0xFC00)
    // - pixel1: green, A=0 (0x03E0)
    guest.set([0x00, 0xfc, 0xe0, 0x03], 0);
    // Row 1 @ offset pitchBytes:
    // - pixel0: blue, A=1 (0x801F)
    // - pixel1: white, A=0 (0x7FFF)
    guest.set([0x1f, 0x80, 0xff, 0x7f], pitchBytes);

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B5G5R5A1,
    });

    expect(calls.guestPaddrToRamOffset).toBe(height);

    expect(Array.from(out.rgba8)).toEqual([
      // Row 0: red (A=1), green (A=0).
      0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0x00,
      // Row 1: blue (A=1), white (A=0).
      0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
    ]);
  });
});

