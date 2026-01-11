export function formatHexBytes(bytes: Uint8Array, maxBytes = 256, columns = 16): string {
  const limit = Math.max(0, maxBytes | 0);
  const cols = Math.max(1, columns | 0);
  const head = bytes.subarray(0, Math.min(bytes.byteLength, limit));
  const parts = Array.from(head, (b) => b.toString(16).padStart(2, "0"));

  let hex = "";
  for (let i = 0; i < parts.length; i += 1) {
    if (i !== 0) hex += i % cols === 0 ? "\n" : " ";
    hex += parts[i]!;
  }

  if (bytes.byteLength <= limit) return hex;
  const remaining = bytes.byteLength - limit;
  const suffix = `… (+${remaining} bytes)`;
  return hex ? `${hex}\n${suffix}` : suffix;
}

export function hex8(value: number): string {
  return `0x${(value & 0xff).toString(16).padStart(2, "0")}`;
}

export function hex16(value: number): string {
  return `0x${(value & 0xffff).toString(16).padStart(4, "0")}`;
}
