// Minimal CRC32 (IEEE) implementation for streaming checksums.
// This is intentionally dependency-free so it can run in both window and worker contexts.

/** @type {Uint32Array | null} */
let CRC32_TABLE = null;

function getTable() {
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

/**
 * @returns {number} initial CRC32 state
 */
export function crc32Init() {
  return 0xffffffff;
}

/**
 * @param {number} crc
 * @param {Uint8Array} data
 * @returns {number} updated CRC32 state
 */
export function crc32Update(crc, data) {
  const table = getTable();
  let c = crc >>> 0;
  for (let i = 0; i < data.length; i++) {
    c = table[(c ^ data[i]) & 0xff] ^ (c >>> 8);
  }
  return c >>> 0;
}

/**
 * @param {number} crc
 * @returns {number} final CRC32 value
 */
export function crc32Final(crc) {
  return (crc ^ 0xffffffff) >>> 0;
}

/**
 * @param {number} crc32
 * @returns {string}
 */
export function crc32ToHex(crc32) {
  return (crc32 >>> 0).toString(16).padStart(8, "0");
}

