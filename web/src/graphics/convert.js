/**
 * @param {Uint8Array} src
 * @returns {Uint8Array}
 */
export function bgra8ToRgba8(src) {
  const out = new Uint8Array(src.length);
  for (let i = 0; i < src.length; i += 4) {
    out[i + 0] = src[i + 2];
    out[i + 1] = src[i + 1];
    out[i + 2] = src[i + 0];
    out[i + 3] = src[i + 3];
  }
  return out;
}

/**
 * @param {Uint16Array} src
 * @returns {Uint8Array}
 */
export function rgb565ToRgba8(src) {
  const out = new Uint8Array(src.length * 4);
  for (let i = 0; i < src.length; i++) {
    const v = src[i];
    const r5 = (v >> 11) & 0x1f;
    const g6 = (v >> 5) & 0x3f;
    const b5 = v & 0x1f;

    const r = (r5 << 3) | (r5 >> 2);
    const g = (g6 << 2) | (g6 >> 4);
    const b = (b5 << 3) | (b5 >> 2);

    const o = i * 4;
    out[o + 0] = r;
    out[o + 1] = g;
    out[o + 2] = b;
    out[o + 3] = 0xff;
  }
  return out;
}

/**
 * @param {Uint8Array} indices
 * @param {Uint8Array} paletteRgba8 256 * 4 bytes (RGBA).
 * @returns {Uint8Array}
 */
export function indexed8ToRgba8(indices, paletteRgba8) {
  const out = new Uint8Array(indices.length * 4);
  for (let i = 0; i < indices.length; i++) {
    const idx = indices[i] * 4;
    const o = i * 4;
    out[o + 0] = paletteRgba8[idx + 0];
    out[o + 1] = paletteRgba8[idx + 1];
    out[o + 2] = paletteRgba8[idx + 2];
    out[o + 3] = paletteRgba8[idx + 3];
  }
  return out;
}

