import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  FramebufferFormat,
  SHARED_FRAMEBUFFER_ALIGNMENT,
  SHARED_FRAMEBUFFER_HEADER_BYTE_LEN,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_SLOTS,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
} from "./shared-layout";

function parseRustConstExpr(source: string, name: string): string {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  const re = new RegExp(String.raw`^\s*pub const ${name}: [^=]+ = (.+);$`, "m");
  const match = source.match(re);
  if (!match) {
    throw new Error(`Failed to locate \`pub const ${name}\` in crates/aero-shared/src/shared_framebuffer.rs`);
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

  const lit = parseRustIntLiteral(e);
  if (lit != null) return lit;

  // Support the small subset of const expressions used by SharedFramebuffer.
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

function parseRustEnumVariantDiscriminant(source: string, enumName: string, variant: string): number {
  const enumMatch = source.match(new RegExp(String.raw`pub enum ${enumName}\s*\{([\s\S]*?)\r?\n\}`, "m"));
  if (!enumMatch) {
    throw new Error(`Failed to locate \`pub enum ${enumName} { ... }\` in crates/aero-shared/src/shared_framebuffer.rs`);
  }
  const body = enumMatch[1] ?? "";
  const re = new RegExp(String.raw`^\s*${variant}\s*=\s*([^,]+),`, "m");
  const match = body.match(re);
  if (!match) {
    throw new Error(`Failed to locate ${enumName} variant ${variant} in Rust source`);
  }
  const lit = parseRustIntLiteral((match[1] ?? "").trim());
  if (lit == null) {
    throw new Error(`Unsupported Rust discriminant expression for ${enumName}::${variant}: ${(match[1] ?? "").trim()}`);
  }
  if (lit > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`Rust enum discriminant too large for JS number: ${enumName}::${variant} = ${lit.toString()}`);
  }
  return Number(lit);
}

describe("SharedFramebuffer layout matches Rust source of truth", () => {
  it("keeps shared framebuffer header constants in sync with crates/aero-shared/src/shared_framebuffer.rs", () => {
    const rustUrl = new URL("../../../crates/aero-shared/src/shared_framebuffer.rs", import.meta.url);
    const rust = readFileSync(rustUrl, "utf8");

    // Fundamental header constants.
    expect(SHARED_FRAMEBUFFER_MAGIC >>> 0, "SHARED_FRAMEBUFFER_MAGIC mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "SHARED_FRAMEBUFFER_MAGIC"),
    );
    expect(SHARED_FRAMEBUFFER_VERSION, "SHARED_FRAMEBUFFER_VERSION mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "SHARED_FRAMEBUFFER_VERSION"),
    );
    expect(SHARED_FRAMEBUFFER_SLOTS, "SHARED_FRAMEBUFFER_SLOTS mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "SHARED_FRAMEBUFFER_SLOTS"),
    );
    expect(SHARED_FRAMEBUFFER_ALIGNMENT, "SHARED_FRAMEBUFFER_ALIGNMENT mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "SHARED_FRAMEBUFFER_ALIGNMENT"),
    );

    const rustU32Len = parseRustConstNumber(rust, "SHARED_FRAMEBUFFER_HEADER_U32_LEN");
    expect(SHARED_FRAMEBUFFER_HEADER_U32_LEN, "SHARED_FRAMEBUFFER_HEADER_U32_LEN mismatch (Rust <-> TS)").toBe(rustU32Len);

    const rustByteLen = parseRustConstNumber(rust, "SHARED_FRAMEBUFFER_HEADER_BYTE_LEN", {
      SHARED_FRAMEBUFFER_HEADER_U32_LEN: BigInt(rustU32Len),
    });
    expect(SHARED_FRAMEBUFFER_HEADER_BYTE_LEN, "SHARED_FRAMEBUFFER_HEADER_BYTE_LEN mismatch (Rust <-> TS)").toBe(rustByteLen);

    // FramebufferFormat discriminants.
    expect(FramebufferFormat.RGBA8, "FramebufferFormat.RGBA8 mismatch (Rust <-> TS)").toBe(
      parseRustEnumVariantDiscriminant(rust, "FramebufferFormat", "Rgba8"),
    );

    // Header indices / layout offsets.
    expect(SharedFramebufferHeaderIndex.MAGIC, "SharedFramebufferHeaderIndex.MAGIC mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "MAGIC"),
    );
    expect(SharedFramebufferHeaderIndex.VERSION, "SharedFramebufferHeaderIndex.VERSION mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "VERSION"),
    );
    expect(SharedFramebufferHeaderIndex.WIDTH, "SharedFramebufferHeaderIndex.WIDTH mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "WIDTH"),
    );
    expect(SharedFramebufferHeaderIndex.HEIGHT, "SharedFramebufferHeaderIndex.HEIGHT mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "HEIGHT"),
    );
    expect(
      SharedFramebufferHeaderIndex.STRIDE_BYTES,
      "SharedFramebufferHeaderIndex.STRIDE_BYTES mismatch (Rust <-> TS)",
    ).toBe(parseRustConstNumber(rust, "STRIDE_BYTES"));
    expect(SharedFramebufferHeaderIndex.FORMAT, "SharedFramebufferHeaderIndex.FORMAT mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "FORMAT"),
    );
    expect(
      SharedFramebufferHeaderIndex.ACTIVE_INDEX,
      "SharedFramebufferHeaderIndex.ACTIVE_INDEX mismatch (Rust <-> TS)",
    ).toBe(parseRustConstNumber(rust, "ACTIVE_INDEX"));
    expect(SharedFramebufferHeaderIndex.FRAME_SEQ, "SharedFramebufferHeaderIndex.FRAME_SEQ mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "FRAME_SEQ"),
    );
    expect(
      SharedFramebufferHeaderIndex.FRAME_DIRTY,
      "SharedFramebufferHeaderIndex.FRAME_DIRTY mismatch (Rust <-> TS)",
    ).toBe(parseRustConstNumber(rust, "FRAME_DIRTY"));
    expect(SharedFramebufferHeaderIndex.TILE_SIZE, "SharedFramebufferHeaderIndex.TILE_SIZE mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "TILE_SIZE"),
    );
    expect(SharedFramebufferHeaderIndex.TILES_X, "SharedFramebufferHeaderIndex.TILES_X mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "TILES_X"),
    );
    expect(SharedFramebufferHeaderIndex.TILES_Y, "SharedFramebufferHeaderIndex.TILES_Y mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "TILES_Y"),
    );
    expect(
      SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER,
      "SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER mismatch (Rust <-> TS)",
    ).toBe(parseRustConstNumber(rust, "DIRTY_WORDS_PER_BUFFER"));
    expect(
      SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ,
      "SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ mismatch (Rust <-> TS)",
    ).toBe(parseRustConstNumber(rust, "BUF0_FRAME_SEQ"));
    expect(
      SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
      "SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ mismatch (Rust <-> TS)",
    ).toBe(parseRustConstNumber(rust, "BUF1_FRAME_SEQ"));
    expect(SharedFramebufferHeaderIndex.FLAGS, "SharedFramebufferHeaderIndex.FLAGS mismatch (Rust <-> TS)").toBe(
      parseRustConstNumber(rust, "FLAGS"),
    );
  });
});

