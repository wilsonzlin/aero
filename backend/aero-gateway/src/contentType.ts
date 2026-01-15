import { asciiLowerEqualsSpan } from "./ascii.js";

function isAsciiWhitespace(code: number): boolean {
  // Treat all ASCII control chars + space as “trim”.
  return code <= 0x20;
}

function contentTypeHasMimeType(raw: string, lowerMimeType: string, maxLen: number): boolean {
  if (raw.length > maxLen) return false;

  let start = 0;
  while (start < raw.length && isAsciiWhitespace(raw.charCodeAt(start))) start += 1;

  let end = raw.length;
  for (let i = start; i < raw.length; i += 1) {
    if (raw.charCodeAt(i) === 0x3b /* ';' */) {
      end = i;
      break;
    }
  }

  while (end > start && isAsciiWhitespace(raw.charCodeAt(end - 1))) end -= 1;
  return asciiLowerEqualsSpan(raw, start, end, lowerMimeType);
}

export function headerHasMimeType(value: unknown, lowerMimeType: string, maxLen: number): boolean {
  if (typeof value === "string") return contentTypeHasMimeType(value, lowerMimeType, maxLen);
  if (Array.isArray(value)) {
    if (value.length !== 1) return false;
    if (typeof value[0] !== "string") return false;
    return contentTypeHasMimeType(value[0], lowerMimeType, maxLen);
  }
  return false;
}
