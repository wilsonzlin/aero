/**
 * sRGB <-> linear conversion helpers for 8-bit channels.
 *
 * Notes:
 * - Alpha is treated as linear and is not modified by these helpers.
 * - These conversions are used in a few places:
 *   - GPU worker: decode `*_SRGB` scanout/cursor formats to linear RGBA8 before blending/present.
 *   - Main thread: encode linear screenshot bytes to sRGB before exporting PNG via Canvas2D.
 */

// Lookup table for converting an 8-bit sRGB channel to an 8-bit linear channel.
export const SRGB_TO_LINEAR_U8 = (() => {
  const lut = new Uint8Array(256);
  for (let i = 0; i < 256; i += 1) {
    const s = i / 255;
    const linear = s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
    lut[i] = Math.min(255, Math.max(0, Math.round(linear * 255)));
  }
  return lut;
})();

// Lookup table for converting an 8-bit linear channel to an 8-bit sRGB channel.
export const LINEAR_TO_SRGB_U8 = (() => {
  const lut = new Uint8Array(256);
  for (let i = 0; i < 256; i += 1) {
    const l = i / 255;
    const s = l <= 0.0031308 ? l * 12.92 : 1.055 * Math.pow(l, 1 / 2.4) - 0.055;
    lut[i] = Math.min(255, Math.max(0, Math.round(s * 255)));
  }
  return lut;
})();

/**
 * Decode an RGBA8 buffer in-place from sRGB to linear.
 *
 * RGB channels are decoded; alpha is preserved.
 */
export function linearizeSrgbRgba8InPlace(rgba: Uint8Array): void {
  for (let i = 0; i + 3 < rgba.byteLength; i += 4) {
    rgba[i + 0] = SRGB_TO_LINEAR_U8[rgba[i + 0]!]!;
    rgba[i + 1] = SRGB_TO_LINEAR_U8[rgba[i + 1]!]!;
    rgba[i + 2] = SRGB_TO_LINEAR_U8[rgba[i + 2]!]!;
  }
}

/**
 * Encode an RGBA8 buffer in-place from linear to sRGB.
 *
 * RGB channels are encoded; alpha is preserved.
 */
export function encodeLinearRgba8ToSrgbInPlace(rgba: Uint8Array): void {
  for (let i = 0; i + 3 < rgba.byteLength; i += 4) {
    rgba[i + 0] = LINEAR_TO_SRGB_U8[rgba[i + 0]!]!;
    rgba[i + 1] = LINEAR_TO_SRGB_U8[rgba[i + 1]!]!;
    rgba[i + 2] = LINEAR_TO_SRGB_U8[rgba[i + 2]!]!;
  }
}

