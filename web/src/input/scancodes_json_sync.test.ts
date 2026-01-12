import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { PS2_SET2_CODE_TO_SCANCODE, type Ps2Set2Scancode } from "./scancodes";

type ScancodesJsonEntry = {
  make: string[];
  break?: string[];
};

type ScancodesJson = {
  ps2_set2: Record<string, ScancodesJsonEntry>;
};

function parseHexByte(s: string): number {
  const trimmed = s.startsWith("0x") || s.startsWith("0X") ? s.slice(2) : s;
  const n = Number.parseInt(trimmed, 16);
  if (!Number.isFinite(n) || Number.isNaN(n) || n < 0 || n > 0xff) {
    throw new Error(`invalid u8 hex literal ${JSON.stringify(s)}`);
  }
  return n;
}

function parseHexBytes(xs: string[]): number[] {
  return xs.map(parseHexByte);
}

function expectedScancodeFromJson(entry: ScancodesJsonEntry): Ps2Set2Scancode {
  const make = parseHexBytes(entry.make);
  // The generator normalizes the common "E0 <make>" case into a simple entry with an `extended`
  // flag for compactness.
  if (make.length === 1) {
    return { kind: "simple", make: make[0]!, extended: false };
  }
  if (make.length === 2 && make[0] === 0xe0) {
    return { kind: "simple", make: make[1]!, extended: true };
  }
  return {
    kind: "sequence",
    make,
    break: parseHexBytes(entry.break ?? []),
  };
}

describe("PS/2 Set-2 scancode table", () => {
  it("matches tools/gen_scancodes/scancodes.json", () => {
    const scancodesUrl = new URL("../../../tools/gen_scancodes/scancodes.json", import.meta.url);
    const json = JSON.parse(readFileSync(scancodesUrl, "utf8")) as ScancodesJson;

    const expectedCodes = Object.keys(json.ps2_set2).sort();
    const actualCodes = Object.keys(PS2_SET2_CODE_TO_SCANCODE).sort();
    expect(actualCodes).toEqual(expectedCodes);

    for (const code of expectedCodes) {
      const expected = expectedScancodeFromJson(json.ps2_set2[code]!);
      const actual = PS2_SET2_CODE_TO_SCANCODE[code];
      expect(actual).toEqual(expected);
    }
  });
});

