import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { MAX_MESSAGE_BYTES } from "./protocol";

type TagMap = Readonly<Record<string, bigint>>;

function parseIntLiteral(token: string): bigint | null {
  // Accept TS/Rust-style integer literals with optional underscores and optional type suffixes.
  const match = token.trim().match(/^(0x[0-9a-fA-F_]+|\d[\d_]*)(?:[a-zA-Z][a-zA-Z0-9_]*)?$/);
  if (!match) return null;
  const raw = (match[1] ?? "").replaceAll("_", "");
  if (raw.length === 0) return null;
  return BigInt(raw);
}

function parseTagConstsFromRust(source: string): TagMap {
  const tags: Record<string, bigint> = {};
  const re = /^const\s+((?:CMD|EVT)_TAG_[A-Z0-9_]+):\s*u16\s*=\s*([^;]+);/gm;
  for (const match of source.matchAll(re)) {
    const name = match[1] ?? "";
    const expr = match[2] ?? "";
    const lit = parseIntLiteral(expr);
    if (lit == null) {
      throw new Error(`Unsupported Rust tag literal for ${name}: ${expr}`);
    }
    tags[name] = lit;
  }
  return tags;
}

function parseTagConstsFromTs(source: string): TagMap {
  const tags: Record<string, bigint> = {};
  const re = /^const\s+((?:CMD|EVT)_TAG_[A-Z0-9_]+)\s*=\s*([^;]+);/gm;
  for (const match of source.matchAll(re)) {
    const name = match[1] ?? "";
    const expr = match[2] ?? "";
    const lit = parseIntLiteral(expr);
    if (lit == null) {
      throw new Error(`Unsupported TS tag literal for ${name}: ${expr}`);
    }
    tags[name] = lit;
  }
  return tags;
}

function evalConstExpr(expr: string): bigint {
  const e = expr.trim();
  const lit = parseIntLiteral(e);
  if (lit != null) return lit;

  const shift = e.match(/^(.+?)\s*<<\s*(.+)$/);
  if (shift) return evalConstExpr(shift[1] ?? "") << evalConstExpr(shift[2] ?? "");

  const mul = e.match(/^(.+?)\s*\*\s*(.+)$/);
  if (mul) return evalConstExpr(mul[1] ?? "") * evalConstExpr(mul[2] ?? "");

  throw new Error(`Unsupported const expression: ${expr}`);
}

describe("IPC protocol tags match Rust source of truth", () => {
  it("keeps web/src/ipc/protocol.ts tags in sync with crates/aero-ipc/src/protocol.rs", () => {
    const rustUrl = new URL("../../../crates/aero-ipc/src/protocol.rs", import.meta.url);
    const tsUrl = new URL("./protocol.ts", import.meta.url);
    const rust = readFileSync(rustUrl, "utf8");
    const ts = readFileSync(tsUrl, "utf8");

    const maxMatch = rust.match(/^\s*pub const MAX_MESSAGE_BYTES: [^=]+ = (.+?);/m);
    if (!maxMatch) throw new Error("Failed to locate MAX_MESSAGE_BYTES in crates/aero-ipc/src/protocol.rs");
    const maxBytesRust = evalConstExpr(maxMatch[1] ?? "");
    expect(MAX_MESSAGE_BYTES, "MAX_MESSAGE_BYTES mismatch (Rust <-> TS)").toBe(Number(maxBytesRust));

    const rustTags = parseTagConstsFromRust(rust);
    const tsTags = parseTagConstsFromTs(ts);

    const rustNames = Object.keys(rustTags).sort();
    const tsNames = Object.keys(tsTags).sort();

    expect(rustNames.length, "expected to find CMD_TAG_*/EVT_TAG_* in Rust source").toBeGreaterThan(0);
    expect(tsNames.length, "expected to find CMD_TAG_*/EVT_TAG_* in TS source").toBeGreaterThan(0);

    expect(tsNames, "tag name set mismatch (Rust <-> TS)").toEqual(rustNames);

    for (const name of rustNames) {
      expect(tsTags[name], `missing TS tag const: ${name}`).toBeDefined();
      expect(tsTags[name], `tag value mismatch for ${name} (Rust <-> TS)`).toBe(rustTags[name]);
    }
  });
});
