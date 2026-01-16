const UTF8 = Object.freeze({ encoding: "utf-8" });
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder(UTF8.encoding);

function coerceString(input) {
  try {
    return String(input ?? "");
  } catch {
    return "";
  }
}

function sanitizeOneLine(input) {
  const parts = [];
  let hasOutput = false;
  let pendingSpace = false;
  for (const ch of coerceString(input)) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = hasOutput;
      continue;
    }
    if (pendingSpace) {
      parts.push(" ");
      pendingSpace = false;
    }
    parts.push(ch);
    hasOutput = true;
  }
  return parts.join("");
}

function truncateUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  const s = coerceString(input);
  if (maxBytes === 0) return "";
  const buf = new Uint8Array(maxBytes);
  const { read, written } = textEncoder.encodeInto(s, buf);
  if (read === s.length) return s;
  return written === 0 ? "" : textDecoder.decode(buf.subarray(0, written));
}

function formatOneLineUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  if (maxBytes === 0) return "";

  const buf = new Uint8Array(maxBytes);
  let written = 0;
  let pendingSpace = false;
  for (const ch of coerceString(input)) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = written > 0;
      continue;
    }

    if (pendingSpace) {
      const spaceRes = textEncoder.encodeInto(" ", buf.subarray(written));
      if (spaceRes.written === 0) break;
      written += spaceRes.written;
      pendingSpace = false;
      if (written >= maxBytes) break;
    }

    const res = textEncoder.encodeInto(ch, buf.subarray(written));
    if (res.written === 0) break;
    written += res.written;
    if (written >= maxBytes) break;
  }
  return written === 0 ? "" : textDecoder.decode(buf.subarray(0, written));
}

function safeErrorMessageInput(err) {
  if (err === null) return "null";

  const t = typeof err;
  if (t === "string") return err;
  if (t === "number" || t === "boolean" || t === "bigint" || t === "symbol" || t === "undefined") return String(err);

  if (t === "object") {
    try {
      const msg = err && typeof err.message === "string" ? err.message : null;
      if (msg !== null) return msg;
    } catch {
      // ignore getters throwing
    }
  }

  // Avoid calling toString() on arbitrary objects/functions (can throw / be expensive).
  return "Error";
}

function formatOneLineError(err, maxBytes, fallback = "Error") {
  const raw = safeErrorMessageInput(err);
  const safe = formatOneLineUtf8(raw, maxBytes);
  const fb = typeof fallback === "string" && fallback ? fallback : "Error";
  return safe || fb;
}

module.exports = {
  sanitizeOneLine,
  truncateUtf8,
  formatOneLineUtf8,
  formatOneLineError,
};

