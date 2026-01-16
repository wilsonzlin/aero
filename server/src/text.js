export function sanitizeOneLine(input) {
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

export function truncateUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  const encoded = Buffer.from(String(input ?? ""), "utf8");
  if (encoded.length <= maxBytes) return String(input ?? "");
  let cut = maxBytes;
  while (cut > 0 && (encoded[cut] & 0xc0) === 0x80) cut -= 1;
  if (cut <= 0) return "";
  return encoded.subarray(0, cut).toString("utf8");
}

export function formatOneLineUtf8(input, maxBytes) {
  return truncateUtf8(sanitizeOneLine(input), maxBytes);
}
