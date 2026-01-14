import { describe, expect, it, vi } from "vitest";

import { AerogpuFormat } from "../../../../emulator/protocol/aerogpu/aerogpu_pci";
import type { GuestRamLayout } from "../../runtime/shared_layout";
import { AeroGpuPciDevice } from "./aerogpu";

function mkLayout(bytes: number): GuestRamLayout {
  return {
    guest_base: 0,
    guest_size: bytes >>> 0,
    runtime_reserved: 0,
    wasm_pages: Math.ceil(bytes / (64 * 1024)),
  };
}

describe("io/devices/aerogpu cursor forwarding", () => {
  it("stays inert until the guest touches cursor MMIO regs", () => {
    const guest = new Uint8Array(4096);
    const sink = { setImage: vi.fn(), setState: vi.fn() };
    const dev = new AeroGpuPciDevice({ guestU8: guest, guestLayout: mkLayout(guest.byteLength), sink });

    dev.tick(0);
    expect(sink.setImage).not.toHaveBeenCalled();
    expect(sink.setState).not.toHaveBeenCalled();
  });

  it("converts BGRA cursor data to RGBA8888 (and preserves alpha)", () => {
    const guest = new Uint8Array(4096);
    const gpa = 0x100;

    // Two pixels, BGRA:
    // (R=1,G=2,B=3,A=4), (R=10,G=20,B=30,A=40)
    guest.set([3, 2, 1, 4, 30, 20, 10, 40], gpa);

    let image: Uint8Array | null = null;
    const sink = {
      setImage: (_w: number, _h: number, rgba8: ArrayBuffer) => {
        image = new Uint8Array(rgba8);
      },
      setState: vi.fn(),
    };
    const dev = new AeroGpuPciDevice({ guestU8: guest, guestLayout: mkLayout(guest.byteLength), sink });

    dev.debugProgramCursor({
      enabled: true,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 2,
      height: 1,
      format: AerogpuFormat.B8G8R8A8Unorm,
      fbGpa: gpa,
      pitchBytes: 8,
    });
    dev.tick(0);

    expect(image).toBeTruthy();
    expect(Array.from(image!)).toEqual([1, 2, 3, 4, 10, 20, 30, 40]);
  });

  it("treats X8 formats as opaque (alpha=0xff)", () => {
    const guest = new Uint8Array(4096);
    const gpa = 0x180;
    // One pixel BGRX with X=0x00 (should become alpha=0xff).
    guest.set([3, 2, 1, 0], gpa);

    let image: Uint8Array | null = null;
    const sink = {
      setImage: (_w: number, _h: number, rgba8: ArrayBuffer) => {
        image = new Uint8Array(rgba8);
      },
      setState: vi.fn(),
    };
    const dev = new AeroGpuPciDevice({ guestU8: guest, guestLayout: mkLayout(guest.byteLength), sink });

    dev.debugProgramCursor({
      enabled: true,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      format: AerogpuFormat.B8G8R8X8Unorm,
      fbGpa: gpa,
      pitchBytes: 4,
    });
    dev.tick(0);

    expect(image).toBeTruthy();
    expect(Array.from(image!)).toEqual([1, 2, 3, 255]);
  });

  it("disables cursor overlay when the programmed cursor layout is invalid (pitch < rowBytes)", () => {
    const guest = new Uint8Array(4096);
    const gpa = 0x200;
    guest.set([0, 0, 0, 0], gpa);

    const sink = { setImage: vi.fn(), setState: vi.fn() };
    const dev = new AeroGpuPciDevice({ guestU8: guest, guestLayout: mkLayout(guest.byteLength), sink });

    dev.debugProgramCursor({
      enabled: true,
      x: 123,
      y: 456,
      hotX: 0,
      hotY: 0,
      width: 2,
      height: 1,
      format: AerogpuFormat.R8G8B8A8Unorm,
      fbGpa: gpa,
      pitchBytes: 4, // invalid (needs 8)
    });
    dev.tick(0);

    expect(sink.setImage).not.toHaveBeenCalled();
    // The device reports cursor.set_state enabled=false when it can't safely read pixels.
    expect(sink.setState).toHaveBeenCalledWith(false, 123, 456, 0, 0);
  });
});

