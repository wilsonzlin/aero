// Minimal CRC32 (IEEE) implementation for streaming checksums.
// Intentionally dependency-free so it can run in both window and worker contexts.

let CRC32_TABLE: Uint32Array | null = null;

function getTable(): Uint32Array {
  if (CRC32_TABLE) return CRC32_TABLE;
  const table = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let c = i;
    for (let k = 0; k < 8; k++) {
      c = (c & 1) ? (0xedb88320 ^ (c >>> 1)) : (c >>> 1);
    }
    table[i] = c >>> 0;
  }
  CRC32_TABLE = table;
  return table;
}

export function crc32Init(): number {
  return 0xffffffff;
}

export function crc32Update(crc: number, data: Uint8Array): number {
  const table = getTable();
  let c = crc >>> 0;
  for (let i = 0; i < data.length; i++) {
    c = table[(c ^ data[i]) & 0xff] ^ (c >>> 8);
  }
  return c >>> 0;
}

export function crc32Final(crc: number): number {
  return (crc ^ 0xffffffff) >>> 0;
}

export function crc32ToHex(crc32: number): string {
  return (crc32 >>> 0).toString(16).padStart(8, "0");
}
