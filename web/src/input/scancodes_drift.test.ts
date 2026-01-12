import { describe, expect, it } from "vitest";

import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { PS2_SET2_CODE_TO_SCANCODE, ps2Set2BytesForKeyEvent } from "./scancodes";

type RawMapping = {
  ps2_set2: Record<
    string,
    {
      make: string[];
      break?: string[];
    }
  >;
};

function parseHexByte(hex: string): number {
  if (!/^[0-9A-Fa-f]{2}$/.test(hex)) {
    throw new Error(`Invalid hex byte string: ${JSON.stringify(hex)}`);
  }
  return Number.parseInt(hex, 16);
}

function parseHexBytes(bytes: readonly string[]): number[] {
  return bytes.map(parseHexByte);
}

function expectedBytesForEntry(entry: { make: string[]; break?: string[] }, pressed: boolean): number[] {
  const make = parseHexBytes(entry.make);

  if (entry.break !== undefined) {
    const brk = parseHexBytes(entry.break);
    return pressed ? make : brk;
  }

  // Simple (1-byte) make, with optional 0xE0 prefix.
  if (make.length === 1) {
    const b = make[0]!;
    return pressed ? [b] : [0xf0, b];
  }
  if (make.length === 2 && make[0] === 0xe0) {
    const b = make[1]!;
    return pressed ? [0xe0, b] : [0xe0, 0xf0, b];
  }

  throw new Error(
    `Non-simple key mapping for make=${JSON.stringify(entry.make)} missing explicit break sequence`,
  );
}

describe("scancodes.json drift prevention", () => {
  it("generated web/src/input/scancodes.ts matches tools/gen_scancodes/scancodes.json", async () => {
    const testDir = path.dirname(fileURLToPath(import.meta.url));
    const repoRoot = path.resolve(testDir, "../../../");
    const mappingPath = path.join(repoRoot, "tools/gen_scancodes/scancodes.json");

    const raw = JSON.parse(await fs.readFile(mappingPath, "utf8")) as RawMapping;
    const jsonCodes = Object.keys(raw.ps2_set2).sort((a, b) => a.localeCompare(b));
    const tsCodes = Object.keys(PS2_SET2_CODE_TO_SCANCODE).sort((a, b) => a.localeCompare(b));

    expect(tsCodes).toEqual(jsonCodes);

    for (const code of jsonCodes) {
      const entry = raw.ps2_set2[code];
      if (!entry) throw new Error(`missing JSON entry for ${code}`);

      for (const pressed of [true, false]) {
        const expected = expectedBytesForEntry(entry, pressed);
        const actual = ps2Set2BytesForKeyEvent(code, pressed);
        expect(actual, `code=${code} pressed=${pressed}`).toEqual(expected);
      }
    }
  });

  it("covers standard, extended (E0), and multi-byte (PrintScreen/Pause) sequences", () => {
    // Standard.
    expect(ps2Set2BytesForKeyEvent("KeyA", true)).toEqual([0x1c]);
    expect(ps2Set2BytesForKeyEvent("KeyA", false)).toEqual([0xf0, 0x1c]);

    // Extended (E0 prefix).
    expect(ps2Set2BytesForKeyEvent("ArrowUp", true)).toEqual([0xe0, 0x75]);
    expect(ps2Set2BytesForKeyEvent("ArrowUp", false)).toEqual([0xe0, 0xf0, 0x75]);

    // Multi-byte sequences.
    expect(ps2Set2BytesForKeyEvent("PrintScreen", true)).toEqual([0xe0, 0x12, 0xe0, 0x7c]);
    expect(ps2Set2BytesForKeyEvent("PrintScreen", false)).toEqual([0xe0, 0xf0, 0x7c, 0xe0, 0xf0, 0x12]);

    // `Pause` has an empty break sequence in set 2 (it emits all bytes on press).
    expect(ps2Set2BytesForKeyEvent("Pause", true)).toEqual([
      0xe1, 0x14, 0x77, 0xe1, 0xf0, 0x14, 0xf0, 0x77,
    ]);
    expect(ps2Set2BytesForKeyEvent("Pause", false)).toEqual([]);
  });

  it("src/input/scancodes.ts is identical to web/src/input/scancodes.ts", async () => {
    const testDir = path.dirname(fileURLToPath(import.meta.url));
    const repoRoot = path.resolve(testDir, "../../../");

    const webScancodesPath = path.join(testDir, "scancodes.ts");
    const srcScancodesPath = path.join(repoRoot, "src/input/scancodes.ts");

    const [webBody, srcBody] = await Promise.all([
      fs.readFile(webScancodesPath, "utf8"),
      fs.readFile(srcScancodesPath, "utf8"),
    ]);

    // Both TS scancode maps are generated from the same input, so they should be byte-for-byte
    // identical. If this fails, regenerate:
    //   node tools/gen_scancodes/gen_scancodes.mjs
    expect(srcBody).toBe(webBody);
  });
});

