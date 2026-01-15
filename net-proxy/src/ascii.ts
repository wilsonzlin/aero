export function asciiLowerEquals(s: string, lower: string): boolean {
  if (s.length !== lower.length) return false;
  for (let i = 0; i < lower.length; i += 1) {
    let c = s.charCodeAt(i);
    if (c >= 0x41 && c <= 0x5a) c += 0x20; // ASCII upper -> lower
    if (c !== lower.charCodeAt(i)) return false;
  }
  return true;
}

export function asciiLowerEqualsSpan(s: string, start: number, end: number, lower: string): boolean {
  if (end - start !== lower.length) return false;
  for (let i = 0; i < lower.length; i += 1) {
    let c = s.charCodeAt(start + i);
    if (c >= 0x41 && c <= 0x5a) c += 0x20; // ASCII upper -> lower
    if (c !== lower.charCodeAt(i)) return false;
  }
  return true;
}

