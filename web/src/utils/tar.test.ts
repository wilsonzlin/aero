import { describe, expect, it } from "vitest";

import { createTarArchive } from "./tar";

function parseTarEntries(tar: Uint8Array): Array<{ name: string; data: Uint8Array }> {
  const entries: Array<{ name: string; data: Uint8Array }> = [];
  let off = 0;
  while (off + 512 <= tar.byteLength) {
    const header = tar.subarray(off, off + 512);
    off += 512;

    // End of archive: two consecutive zero blocks.
    let allZero = true;
    for (let i = 0; i < header.length; i += 1) {
      if (header[i] !== 0) {
        allZero = false;
        break;
      }
    }
    if (allZero) break;

    const nameBytes = header.subarray(0, 100);
    const name = new TextDecoder().decode(nameBytes).split("\0")[0]!;
    const sizeRaw = new TextDecoder().decode(header.subarray(124, 136)).split("\0")[0]!.trim();
    const size = sizeRaw ? Number.parseInt(sizeRaw, 8) : 0;
    const data = tar.subarray(off, off + size);
    off += size;
    const pad = (512 - (size % 512)) % 512;
    off += pad;
    entries.push({ name, data });
  }
  return entries;
}

describe("createTarArchive()", () => {
  it("packs files in ustar format with correct padding", () => {
    const enc = new TextEncoder();
    const tar = createTarArchive(
      [
        { path: "foo.txt", data: enc.encode("hello") },
        { path: "bar.bin", data: new Uint8Array([1, 2, 3, 4]) },
      ],
      { mtimeSec: 0 },
    );

    // Should end with at least 2 * 512 zero bytes.
    expect(tar.byteLength).toBeGreaterThanOrEqual(512 * 3);
    const trailer = tar.subarray(tar.byteLength - 1024);
    expect(trailer.every((b) => b === 0)).toBe(true);

    const entries = parseTarEntries(tar);
    expect(entries.map((e) => e.name)).toEqual(["foo.txt", "bar.bin"]);
    expect(new TextDecoder().decode(entries[0]!.data)).toBe("hello");
    expect(Array.from(entries[1]!.data)).toEqual([1, 2, 3, 4]);
  });

  it("supports directory prefixes in paths", () => {
    const enc = new TextEncoder();
    const tar = createTarArchive([{ path: "dir/sub/file.txt", data: enc.encode("ok") }], { mtimeSec: 0 });
    const entries = parseTarEntries(tar);
    expect(entries.map((e) => e.name)).toEqual(["dir/sub/file.txt"]);
    expect(new TextDecoder().decode(entries[0]!.data)).toBe("ok");
  });
});

