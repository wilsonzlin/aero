import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { IPC_MAGIC, IPC_VERSION, RECORD_ALIGN, WRAP_MARKER, ipcHeader, queueDesc, queueKind, ringCtrl } from "./layout";

function parseRustConstExpr(source: string, name: string): string {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  // Allow an optional trailing comment after the semicolon.
  const re = new RegExp(String.raw`^\s*pub const ${name}: [^=]+ = (.+?);(?:\s*//.*)?$`, "m");
  const match = source.match(re);
  if (!match) {
    throw new Error(`Failed to locate \`pub const ${name}\` in crates/aero-ipc/src/layout.rs`);
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

  // Support the small subset of const expressions used by the IPC layout.
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

function parseRustModuleBody(source: string, moduleName: string): string {
  // Keep matcher intentionally strict. Layout modules are simple and close with a `}` on its own line.
  const re = new RegExp(String.raw`pub mod ${moduleName}\s*\{([\s\S]*?)\r?\n\}`, "m");
  const match = source.match(re);
  if (!match) {
    throw new Error(`Failed to locate \`pub mod ${moduleName} { ... }\` in crates/aero-ipc/src/layout.rs`);
  }
  return match[1] ?? "";
}

describe("IPC shared-memory layout matches Rust source of truth", () => {
  it("keeps web/src/ipc/layout.ts constants in sync with crates/aero-ipc/src/layout.rs", () => {
    const rustUrl = new URL("../../../crates/aero-ipc/src/layout.rs", import.meta.url);
    const rust = readFileSync(rustUrl, "utf8");

    // Top-level constants.
    expect(IPC_MAGIC, "IPC_MAGIC mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "IPC_MAGIC"));
    expect(IPC_VERSION, "IPC_VERSION mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "IPC_VERSION"));
    expect(RECORD_ALIGN, "RECORD_ALIGN mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "RECORD_ALIGN"));
    expect(WRAP_MARKER >>> 0, "WRAP_MARKER mismatch (Rust <-> TS)").toBe(parseRustConstNumber(rust, "WRAP_MARKER"));

    // ring_ctrl module.
    const ring = parseRustModuleBody(rust, "ring_ctrl");
    expect(ringCtrl.HEAD, "ringCtrl.HEAD mismatch (Rust <-> TS)").toBe(parseRustConstNumber(ring, "HEAD"));
    expect(ringCtrl.TAIL_RESERVE, "ringCtrl.TAIL_RESERVE mismatch (Rust <-> TS)").toBe(parseRustConstNumber(ring, "TAIL_RESERVE"));
    expect(ringCtrl.TAIL_COMMIT, "ringCtrl.TAIL_COMMIT mismatch (Rust <-> TS)").toBe(parseRustConstNumber(ring, "TAIL_COMMIT"));
    expect(ringCtrl.CAPACITY, "ringCtrl.CAPACITY mismatch (Rust <-> TS)").toBe(parseRustConstNumber(ring, "CAPACITY"));

    const ringWords = parseRustConstNumber(ring, "WORDS");
    expect(ringCtrl.WORDS, "ringCtrl.WORDS mismatch (Rust <-> TS)").toBe(ringWords);

    const ringBytes = parseRustConstNumber(ring, "BYTES", { WORDS: BigInt(ringWords) });
    expect(ringCtrl.BYTES, "ringCtrl.BYTES mismatch (Rust <-> TS)").toBe(ringBytes);

    // ipc_header module.
    const hdr = parseRustModuleBody(rust, "ipc_header");
    const hdrWords = parseRustConstNumber(hdr, "WORDS");
    expect(ipcHeader.WORDS, "ipcHeader.WORDS mismatch (Rust <-> TS)").toBe(hdrWords);
    expect(ipcHeader.BYTES, "ipcHeader.BYTES mismatch (Rust <-> TS)").toBe(parseRustConstNumber(hdr, "BYTES", { WORDS: BigInt(hdrWords) }));

    expect(ipcHeader.MAGIC, "ipcHeader.MAGIC mismatch (Rust <-> TS)").toBe(parseRustConstNumber(hdr, "MAGIC"));
    expect(ipcHeader.VERSION, "ipcHeader.VERSION mismatch (Rust <-> TS)").toBe(parseRustConstNumber(hdr, "VERSION"));
    expect(ipcHeader.TOTAL_BYTES, "ipcHeader.TOTAL_BYTES mismatch (Rust <-> TS)").toBe(parseRustConstNumber(hdr, "TOTAL_BYTES"));
    expect(ipcHeader.QUEUE_COUNT, "ipcHeader.QUEUE_COUNT mismatch (Rust <-> TS)").toBe(parseRustConstNumber(hdr, "QUEUE_COUNT"));

    // queue_desc module.
    const qd = parseRustModuleBody(rust, "queue_desc");
    const qdWords = parseRustConstNumber(qd, "WORDS");
    expect(queueDesc.WORDS, "queueDesc.WORDS mismatch (Rust <-> TS)").toBe(qdWords);
    expect(queueDesc.BYTES, "queueDesc.BYTES mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qd, "BYTES", { WORDS: BigInt(qdWords) }));

    expect(queueDesc.KIND, "queueDesc.KIND mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qd, "KIND"));
    expect(queueDesc.OFFSET_BYTES, "queueDesc.OFFSET_BYTES mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qd, "OFFSET_BYTES"));
    expect(queueDesc.CAPACITY_BYTES, "queueDesc.CAPACITY_BYTES mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qd, "CAPACITY_BYTES"));
    expect(queueDesc.RESERVED, "queueDesc.RESERVED mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qd, "RESERVED"));

    // queue_kind module.
    const qk = parseRustModuleBody(rust, "queue_kind");
    expect(queueKind.CMD, "queueKind.CMD mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qk, "CMD"));
    expect(queueKind.EVT, "queueKind.EVT mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qk, "EVT"));
    expect(queueKind.NET_TX, "queueKind.NET_TX mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qk, "NET_TX"));
    expect(queueKind.NET_RX, "queueKind.NET_RX mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qk, "NET_RX"));
    expect(queueKind.HID_IN, "queueKind.HID_IN mismatch (Rust <-> TS)").toBe(parseRustConstNumber(qk, "HID_IN"));
  });
});

