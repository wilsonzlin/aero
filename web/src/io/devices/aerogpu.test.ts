import { describe, expect, it } from "vitest";

import {
  AEROGPU_MMIO_REG_CURSOR_X,
  AerogpuFormat,
} from "../../../../emulator/protocol/aerogpu/aerogpu_pci";
import type { GuestRamLayout } from "../../runtime/shared_layout";
import { snapshotCursorState, wrapCursorState, CURSOR_STATE_U32_LEN } from "../../ipc/cursor_state";
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
    const cursorWords = wrapCursorState(new SharedArrayBuffer(CURSOR_STATE_U32_LEN * 4), 0);
    const dev = new AeroGpuPciDevice({ guestU8: guest, guestLayout: mkLayout(guest.byteLength), cursorStateWords: cursorWords });

    dev.tick(0);
    const snap = snapshotCursorState(cursorWords);
    expect(snap.generation >>> 0).toBe(0);
    expect(snap.enable >>> 0).toBe(0);
    expect(snap.width >>> 0).toBe(0);
    expect(snap.height >>> 0).toBe(0);
  });

  it("publishes CursorState on cursor programming (generation bump + surface pointer)", () => {
    const guest = new Uint8Array(4096);
    const gpa = 0x100;

    // One pixel, BGRA.
    guest.set([3, 2, 1, 4], gpa);
    const cursorWords = wrapCursorState(new SharedArrayBuffer(CURSOR_STATE_U32_LEN * 4), 0);
    const dev = new AeroGpuPciDevice({ guestU8: guest, guestLayout: mkLayout(guest.byteLength), cursorStateWords: cursorWords });

    dev.debugProgramCursor({
      enabled: true,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      format: AerogpuFormat.B8G8R8A8Unorm,
      fbGpa: gpa,
      pitchBytes: 4,
    });
    dev.tick(0);

    const snap = snapshotCursorState(cursorWords);
    expect(snap.generation >>> 0).toBe(1);
    expect(snap.enable >>> 0).toBe(1);
    expect(snap.x | 0).toBe(0);
    expect(snap.y | 0).toBe(0);
    expect(snap.hotX >>> 0).toBe(0);
    expect(snap.hotY >>> 0).toBe(0);
    expect(snap.width >>> 0).toBe(1);
    expect(snap.height >>> 0).toBe(1);
    expect(snap.pitchBytes >>> 0).toBe(4);
    expect(snap.format >>> 0).toBe(AerogpuFormat.B8G8R8A8Unorm);
    expect(snap.basePaddrLo >>> 0).toBe(gpa >>> 0);
    expect(snap.basePaddrHi >>> 0).toBe(0);
  });

  it("updates cursor position without bumping generation", () => {
    const guest = new Uint8Array(4096);
    const gpa = 0x180;
    guest.set([3, 2, 1, 0], gpa);
    const cursorWords = wrapCursorState(new SharedArrayBuffer(CURSOR_STATE_U32_LEN * 4), 0);
    const dev = new AeroGpuPciDevice({ guestU8: guest, guestLayout: mkLayout(guest.byteLength), cursorStateWords: cursorWords });

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

    const g1 = snapshotCursorState(cursorWords).generation >>> 0;
    expect(g1).toBe(1);

    dev.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_X), 4, 123);
    dev.tick(1);

    const snap = snapshotCursorState(cursorWords);
    expect(snap.generation >>> 0).toBe(g1);
    expect(snap.x | 0).toBe(123);
  });

  it("bumps generation when the guest updates the cursor bytes in-place", () => {
    const guest = new Uint8Array(4096);
    const gpa = 0x200;
    guest.set([0, 0, 0, 255], gpa);

    const cursorWords = wrapCursorState(new SharedArrayBuffer(CURSOR_STATE_U32_LEN * 4), 0);
    const dev = new AeroGpuPciDevice({ guestU8: guest, guestLayout: mkLayout(guest.byteLength), cursorStateWords: cursorWords });

    dev.debugProgramCursor({
      enabled: true,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      format: AerogpuFormat.B8G8R8A8Unorm,
      fbGpa: gpa,
      pitchBytes: 4,
    });
    dev.tick(0);
    const g1 = snapshotCursorState(cursorWords).generation >>> 0;
    expect(g1).toBe(1);

    // Mutate the guest pixel (BGRA): change blue from 0 to 255.
    guest.set([255, 0, 0, 255], gpa);
    // Poll interval is 64ms; advance past it so the device hashes the bytes and bumps generation.
    dev.tick(65);
    const g2 = snapshotCursorState(cursorWords).generation >>> 0;
    expect(g2).toBe(2);
  });
});
