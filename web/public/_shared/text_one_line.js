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

export function formatOneLineUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  if (maxBytes === 0) return "";

  const buf = new Uint8Array(maxBytes);
  let written = 0;
  let pendingSpace = false;
  for (const ch of coerceString(input)) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden =
      code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
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

function safeErrorMessageInput(err, nameFallbackMode) {
  if (err === null) return "null";
  const t = typeof err;
  if (t === "string") return err;
  if (t === "number" || t === "boolean" || t === "bigint" || t === "symbol" || t === "undefined") return String(err);
  if (t !== "object") return "Error";

  const allowNameFallback = nameFallbackMode === "always" || nameFallbackMode === "missing";
  const allowNameFallbackWhenMessageEmpty = nameFallbackMode === "always";

  try {
    const hasMessage = err && typeof err.message === "string";
    const msg = hasMessage ? err.message : "";
    if (hasMessage && !allowNameFallbackWhenMessageEmpty) return msg;
    if (msg) return msg;
  } catch {
    // ignore
  }

  if (allowNameFallback) {
    try {
      const name = err && typeof err.name === "string" ? err.name : "";
      if (name) return name;
    } catch {
      // ignore
    }
  }

  return "Error";
}

export function formatOneLineError(err, maxBytes, opts) {
  const includeNameFallback = opts ? opts.includeNameFallback : false;
  const nameFallbackMode = includeNameFallback === "missing" ? "missing" : includeNameFallback ? "always" : "never";
  return formatOneLineUtf8(safeErrorMessageInput(err, nameFallbackMode), maxBytes) || "Error";
}
