const UTF8 = Object.freeze({ encoding: "utf-8" as const });

export function sanitizeOneLine(input: string): string {
  let out = "";
  let pendingSpace = false;
  for (const ch of String(input ?? "")) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden =
      code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = out.length > 0;
      continue;
    }
    if (pendingSpace) {
      out += " ";
      pendingSpace = false;
    }
    out += ch;
  }
  return out.trim();
}

export function truncateUtf8(input: string, maxBytes: number): string {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  const s = String(input ?? "");
  const enc = new TextEncoder();
  const bytes = enc.encode(s);
  if (bytes.byteLength <= maxBytes) return s;

  let cut = maxBytes;
  // Back up to the start of a UTF-8 code point.
  while (cut > 0 && (bytes[cut] & 0xc0) === 0x80) cut -= 1;
  if (cut <= 0) return "";
  const dec = new TextDecoder(UTF8.encoding);
  return dec.decode(bytes.subarray(0, cut));
}

export function formatOneLineUtf8(input: string, maxBytes: number): string {
  return truncateUtf8(sanitizeOneLine(input), maxBytes);
}
