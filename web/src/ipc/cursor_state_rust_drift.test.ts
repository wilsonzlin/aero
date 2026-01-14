import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  CURSOR_FORMAT_B8G8R8A8,
  CURSOR_FORMAT_B8G8R8X8,
  CURSOR_FORMAT_R8G8B8A8,
  CURSOR_FORMAT_R8G8B8X8,
  CURSOR_STATE_BYTE_LEN,
  CURSOR_STATE_GENERATION_BUSY_BIT,
  CURSOR_STATE_U32_LEN,
  CursorStateIndex,
} from "./cursor_state";

let cachedAerogpuFormatValues: Readonly<Record<string, bigint>> | null = null;

function getAerogpuFormatValues(): Readonly<Record<string, bigint>> {
  if (cachedAerogpuFormatValues) return cachedAerogpuFormatValues;

  const srcUrl = new URL("../../../emulator/protocol/aerogpu/aerogpu_pci.rs", import.meta.url);
  const src = readFileSync(srcUrl, "utf8");

  const enumMatch = src.match(/pub enum AerogpuFormat\s*\{([\s\S]*?)\r?\n\}/m);
  if (!enumMatch) {
    throw new Error("Failed to locate `pub enum AerogpuFormat { ... }` in emulator/protocol/aerogpu/aerogpu_pci.rs");
  }

  const body = enumMatch[1] ?? "";
  const values: Record<string, bigint> = {};

  // Example lines:
  //   Invalid = 0,
  //   B8G8R8X8Unorm = 2,
  const variantRe = /^\s*([A-Za-z0-9_]+)\s*=\s*([^,]+),/gm;
  for (const match of body.matchAll(variantRe)) {
    const name = match[1] ?? "";
    const raw = (match[2] ?? "").trim();
    const lit = parseRustIntLiteral(raw);
    if (lit == null) {
      throw new Error(`Unsupported AerogpuFormat discriminant expression for ${name}: ${raw}`);
    }
    values[name] = lit;
  }

  cachedAerogpuFormatValues = values;
  return values;
}

function parseRustConstExpr(source: string, name: string): string {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  const re = new RegExp(String.raw`^\s*pub const ${name}: [^=]+ = (.+);$`, "m");
  const match = source.match(re);
  if (!match) {
    throw new Error(`Failed to locate \`pub const ${name}\` in crates/aero-shared/src/cursor_state.rs`);
  }
  return match[1]?.trim() ?? "";
}

function parseRustIntLiteral(token: string): bigint | null {
  // Rust allows numeric suffixes like `123u32` or `0xffusize`. We only care about the numeric part.
  const match = token.match(/^(0x[0-9a-fA-F_]+|\d[\d_]*)(?:[a-zA-Z][a-zA-Z0-9_]*)?$/);
  if (!match) return null;
  const raw = (match[1] ?? "").replaceAll("_", "");
  if (raw.length === 0) return null;
  if (raw.startsWith("0x") || raw.startsWith("0X")) {
    return BigInt(raw);
  }
  return BigInt(raw);
}

function evalRustConstExpr(expr: string, env: Readonly<Record<string, bigint>>): bigint {
  const e = expr.trim();
  if (e in env) return env[e] ?? 0n;

  // Ignore simple casts like `... as u32`.
  const cast = e.match(/^(.+?)\s+as\s+[A-Za-z0-9_]+$/);
  if (cast) return evalRustConstExpr(cast[1] ?? "", env);

  const aerogpuFormat = e.match(/^(?:[A-Za-z0-9_:]+::)?AerogpuFormat::([A-Za-z0-9_]+)$/);
  if (aerogpuFormat) {
    const variant = aerogpuFormat[1] ?? "";
    const values = getAerogpuFormatValues();
    const value = values[variant];
    if (value === undefined) {
      throw new Error(`Unknown AerogpuFormat variant: ${variant}`);
    }
    return value;
  }

  const lit = parseRustIntLiteral(e);
  if (lit != null) return lit;

  // Support the small subset of const expressions used by CursorState.
  const shift = e.match(/^(.+?)\s*<<\s*(.+)$/);
  if (shift) return evalRustConstExpr(shift[1] ?? "", env) << evalRustConstExpr(shift[2] ?? "", env);

  const mul = e.match(/^(.+?)\s*\*\s*(.+)$/);
  if (mul) return evalRustConstExpr(mul[1] ?? "", env) * evalRustConstExpr(mul[2] ?? "", env);

  throw new Error(`Unsupported Rust const expression: ${expr}`);
}

function parseRustConstNumber(source: string, name: string, env: Readonly<Record<string, bigint>> = {}): number {
  const expr = parseRustConstExpr(source, name);
  const value = evalRustConstExpr(expr, env);
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`Rust const ${name} too large to represent as a JS number: ${value.toString()}`);
  }
  return Number(value);
}

describe("CursorState layout matches Rust source of truth", () => {
  it("keeps cursor state constants in sync with crates/aero-shared/src/cursor_state.rs", () => {
    const rustUrl = new URL("../../../crates/aero-shared/src/cursor_state.rs", import.meta.url);
    const rust = readFileSync(rustUrl, "utf8");

    const rustU32Len = parseRustConstNumber(rust, "CURSOR_STATE_U32_LEN");
    expect(CURSOR_STATE_U32_LEN, "CURSOR_STATE_U32_LEN mismatch (Rust <-> TS)").toBe(rustU32Len);

    const rustByteLen = parseRustConstNumber(rust, "CURSOR_STATE_BYTE_LEN", {
      CURSOR_STATE_U32_LEN: BigInt(rustU32Len),
    });
    expect(CURSOR_STATE_BYTE_LEN, "CURSOR_STATE_BYTE_LEN mismatch (Rust <-> TS)").toBe(rustByteLen);

    const rustBusyBit = parseRustConstNumber(rust, "CURSOR_STATE_GENERATION_BUSY_BIT");
    expect(CURSOR_STATE_GENERATION_BUSY_BIT >>> 0, "CURSOR_STATE_GENERATION_BUSY_BIT mismatch (Rust <-> TS)").toBe(rustBusyBit);

    // Cursor format enum values.
    expect(CURSOR_FORMAT_B8G8R8A8, "CURSOR_FORMAT_B8G8R8A8 mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "CURSOR_FORMAT_B8G8R8A8"),
    );
    expect(CURSOR_FORMAT_B8G8R8X8, "CURSOR_FORMAT_B8G8R8X8 mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "CURSOR_FORMAT_B8G8R8X8"),
    );
    expect(CURSOR_FORMAT_R8G8B8A8, "CURSOR_FORMAT_R8G8B8A8 mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "CURSOR_FORMAT_R8G8B8A8"),
    );
    expect(CURSOR_FORMAT_R8G8B8X8, "CURSOR_FORMAT_R8G8B8X8 mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "CURSOR_FORMAT_R8G8B8X8"),
    );

    // Header indices / layout offsets.
    expect(CursorStateIndex.GENERATION, "CursorStateIndex.GENERATION mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "GENERATION"),
    );
    expect(CursorStateIndex.ENABLE, "CursorStateIndex.ENABLE mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "ENABLE"));
    expect(CursorStateIndex.X, "CursorStateIndex.X mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "X"));
    expect(CursorStateIndex.Y, "CursorStateIndex.Y mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "Y"));
    expect(CursorStateIndex.HOT_X, "CursorStateIndex.HOT_X mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "HOT_X"));
    expect(CursorStateIndex.HOT_Y, "CursorStateIndex.HOT_Y mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "HOT_Y"));
    expect(CursorStateIndex.WIDTH, "CursorStateIndex.WIDTH mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "WIDTH"));
    expect(CursorStateIndex.HEIGHT, "CursorStateIndex.HEIGHT mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "HEIGHT"));
    expect(CursorStateIndex.PITCH_BYTES, "CursorStateIndex.PITCH_BYTES mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "PITCH_BYTES"));
    expect(CursorStateIndex.FORMAT, "CursorStateIndex.FORMAT mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "FORMAT"));
    expect(CursorStateIndex.BASE_PADDR_LO, "CursorStateIndex.BASE_PADDR_LO mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "BASE_PADDR_LO"),
    );
    expect(CursorStateIndex.BASE_PADDR_HI, "CursorStateIndex.BASE_PADDR_HI mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "BASE_PADDR_HI"),
    );
  });
});

