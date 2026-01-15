import { asciiLowerEquals } from "./ascii";

// Extract a single-valued header from a Node-style `rawHeaders` array.
//
// Returns:
// - `undefined` if `rawHeaders` is missing or the header is absent
// - `string` if the header is present exactly once
// - `null` if the header is repeated, malformed, or exceeds `maxLen`
export function rawHeaderSingle(
  rawHeaders: unknown,
  nameLower: string,
  maxLen: number
): string | undefined | null {
  if (!Array.isArray(rawHeaders)) return undefined;

  let value: string | undefined = undefined;
  for (let i = 0; i + 1 < rawHeaders.length; i += 2) {
    const k = rawHeaders[i];
    const v = rawHeaders[i + 1];
    if (typeof k !== "string") continue;
    if (!asciiLowerEquals(k, nameLower)) continue;

    if (typeof v !== "string") return null;
    if (value !== undefined) return null;
    if (v.length > maxLen) return null;
    value = v;
  }

  return value;
}

