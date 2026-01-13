import { readFileSync } from "node:fs";
import { describe, it } from "vitest";

type ScancodesJsonEntry = {
  make: unknown;
  break?: unknown;
};

type ScancodesJson = {
  ps2_set2: unknown;
};

// Defensive limits: scancodes.json is a trusted repo file, but this is a unit test
// and should never become accidentally expensive.
const MAX_JSON_BYTES = 256 * 1024;
const MAX_CODE_ENTRIES = 512;
const MAX_SEQ_LEN = 32;

const REGEN_HINT = "Regenerate with `npm run gen:scancodes`.";

function parseHexByte(s: string): number {
  const trimmed = s.startsWith("0x") || s.startsWith("0X") ? s.slice(2) : s;
  if (trimmed.length !== 2 || !/^[0-9a-fA-F]{2}$/.test(trimmed)) {
    throw new Error(`Invalid scancode byte ${JSON.stringify(s)} in scancodes.json. ${REGEN_HINT}`);
  }
  const n = Number.parseInt(trimmed, 16);
  return n & 0xff;
}

function parseByteArray(v: unknown, code: string, field: string): number[] {
  if (!Array.isArray(v)) {
    throw new Error(`Expected scancodes.json entry ${JSON.stringify(code)} field ${JSON.stringify(field)} to be an array. ${REGEN_HINT}`);
  }
  if (v.length > MAX_SEQ_LEN) {
    throw new Error(
      `Refusing to parse scancodes.json entry ${JSON.stringify(code)} field ${JSON.stringify(field)}: sequence length ${v.length} exceeds limit ${MAX_SEQ_LEN}. ${REGEN_HINT}`,
    );
  }
  return v.map((item) => {
    if (typeof item !== "string") {
      throw new Error(
        `Expected scancodes.json entry ${JSON.stringify(code)} field ${JSON.stringify(field)} to contain only strings. ${REGEN_HINT}`,
      );
    }
    return parseHexByte(item);
  });
}

function defaultBreakFromMake(code: string, make: number[]): number[] {
  if (make.length === 1) return [0xf0, make[0]!];
  if (make.length === 2 && make[0] === 0xe0) return [0xe0, 0xf0, make[1]!];
  throw new Error(
    `scancodes.json entry ${JSON.stringify(code)} omits \`break\` but has non-simple make sequence ${JSON.stringify(make)}. ${REGEN_HINT}`,
  );
}

type UsedSet2Scancode = {
  code: number;
  extended: boolean;
  source: string;
  field: "make" | "break";
};

function extractUsedScancodes(seq: number[], source: string, field: "make" | "break"): UsedSet2Scancode[] {
  let sawE0 = false;
  let sawF0 = false;
  const out: UsedSet2Scancode[] = [];

  for (const raw of seq) {
    const b = raw & 0xff;
    switch (b) {
      case 0xe0:
        sawE0 = true;
        break;
      case 0xe1:
        // Pause/Break prefix: resets prefix state.
        sawE0 = false;
        sawF0 = false;
        break;
      case 0xf0:
        // Break prefix: do not clear E0 (E0 F0 <code> is a thing).
        sawF0 = true;
        break;
      default: {
        // "Real scancode" byte.
        out.push({ code: b, extended: sawE0, source, field });
        // Prefix bytes only apply to the next scancode.
        sawE0 = false;
        sawF0 = false;
      }
    }
  }

  // If a sequence ends with a dangling prefix, that's suspicious but doesn't affect coverage.
  void sawF0;

  return out;
}

function parseSet2ToSet1CaseKeys(i8042Source: string): Set<number> {
  const keys = new Set<number>();
  const caseRe = /case\s+\(0x([0-9a-fA-F]{2})\s*<<\s*1\)\s*\|\s*([01])\s*:/g;
  for (;;) {
    const m = caseRe.exec(i8042Source);
    if (!m) break;
    const code = Number.parseInt(m[1]!, 16) & 0xff;
    const extended = m[2] === "1";
    keys.add((code << 1) | (extended ? 1 : 0));
  }
  return keys;
}

describe("io/devices/i8042 Set-2 -> Set-1 translation table", () => {
  it("covers every Set-2 scancode referenced by tools/gen_scancodes/scancodes.json", () => {
    const i8042Url = new URL("./i8042.ts", import.meta.url);
    const i8042Source = readFileSync(i8042Url, "utf8");
    const mappedKeys = parseSet2ToSet1CaseKeys(i8042Source);

    // Sanity check: this protects against the regex silently failing if the
    // translation table is refactored.
    if (mappedKeys.size < 32) {
      throw new Error(`Failed to parse Set-2 -> Set-1 translation cases from ${i8042Url.pathname} (got ${mappedKeys.size}).`);
    }

    const scancodesUrl = new URL("../../../../tools/gen_scancodes/scancodes.json", import.meta.url);
    const jsonBytes = readFileSync(scancodesUrl);
    if (jsonBytes.byteLength > MAX_JSON_BYTES) {
      throw new Error(
        `Refusing to parse scancodes.json: size ${jsonBytes.byteLength} exceeds limit ${MAX_JSON_BYTES}. ${REGEN_HINT}`,
      );
    }

    const root = JSON.parse(jsonBytes.toString("utf8")) as ScancodesJson;
    if (typeof root !== "object" || root === null) {
      throw new Error(`Expected scancodes.json to be an object. ${REGEN_HINT}`);
    }

    const ps2Set2Unknown = (root as ScancodesJson).ps2_set2;
    if (typeof ps2Set2Unknown !== "object" || ps2Set2Unknown === null || Array.isArray(ps2Set2Unknown)) {
      throw new Error(`Expected scancodes.json to contain a top-level \`ps2_set2\` object. ${REGEN_HINT}`);
    }
    const ps2Set2 = ps2Set2Unknown as Record<string, ScancodesJsonEntry>;

    const codes = Object.keys(ps2Set2);
    if (codes.length > MAX_CODE_ENTRIES) {
      throw new Error(
        `Refusing to iterate scancodes.json: entry count ${codes.length} exceeds limit ${MAX_CODE_ENTRIES}. ${REGEN_HINT}`,
      );
    }

    const missing = new Map<number, UsedSet2Scancode[]>();

    for (const browserCode of codes) {
      const entryRaw = ps2Set2[browserCode];
      if (typeof entryRaw !== "object" || entryRaw === null || Array.isArray(entryRaw)) {
        throw new Error(`Expected scancodes.json entry ${JSON.stringify(browserCode)} to be an object. ${REGEN_HINT}`);
      }
      const entry = entryRaw as ScancodesJsonEntry;

      const make = parseByteArray(entry.make, browserCode, "make");
      const brk = entry.break !== undefined ? parseByteArray(entry.break, browserCode, "break") : defaultBreakFromMake(browserCode, make);

      const used = [
        ...extractUsedScancodes(make, browserCode, "make"),
        ...extractUsedScancodes(brk, browserCode, "break"),
      ];

      for (const sc of used) {
        const key = ((sc.code & 0xff) << 1) | (sc.extended ? 1 : 0);
        if (mappedKeys.has(key)) continue;
        const existing = missing.get(key);
        if (existing) existing.push(sc);
        else missing.set(key, [sc]);
      }
    }

    if (missing.size > 0) {
      const lines: string[] = [];
      for (const [key, uses] of missing.entries()) {
        const code = (key >>> 1) & 0xff;
        const extended = (key & 1) !== 0;
        const contexts = uses
          .slice(0, 5)
          .map((u) => `${u.source}.${u.field}`)
          .join(", ");
        const extra = uses.length > 5 ? ` (+${uses.length - 5} more)` : "";
        lines.push(`- missing mapping for set2=0x${code.toString(16).padStart(2, "0")} extended=${extended ? "true" : "false"} (used by ${contexts}${extra})`);
      }
      lines.sort();
      throw new Error(
        `i8042 Set-2 -> Set-1 translation table is missing mappings for scancodes referenced by scancodes.json:\n${lines.join("\n")}\n${REGEN_HINT}`,
      );
    }
  });
});

