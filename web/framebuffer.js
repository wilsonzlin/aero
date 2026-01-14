/**
 * Draws an RGBA8888 framebuffer onto a canvas.
 *
 * The expected `framebuffer` format matches `aero-gpu-vga`:
 * - Uint32Array where each element is RGBA8888
 * - On little-endian hosts (all modern browsers), the underlying bytes in memory
 *   are `[R, G, B, A]`, matching Canvas `ImageData`.
 *
 * Note: Canvas2D `putImageData` expects sRGB-encoded bytes. Some producers (notably the GPU worker
 * and scanout readback paths) treat RGBA8 buffers as **linear** by default.
 *
 * Pass `{ colorSpace: "linear" }` to encode linear->sRGB before presentation.
 */

const LINEAR_TO_SRGB_U8 = (() => {
  const lut = new Uint8Array(256);
  for (let i = 0; i < 256; i += 1) {
    const l = i / 255;
    const s = l <= 0.0031308 ? l * 12.92 : 1.055 * Math.pow(l, 1 / 2.4) - 0.055;
    lut[i] = Math.min(255, Math.max(0, Math.round(s * 255)));
  }
  return lut;
})();

/**
 * @param {HTMLCanvasElement} canvas
 * @param {Uint32Array} framebuffer
 * @param {number} width
 * @param {number} height
 * @param {{ colorSpace?: "srgb" | "linear" }=} opts
 */
export function drawFramebuffer(canvas, framebuffer, width, height, opts = {}) {
  canvas.width = width;
  canvas.height = height;

  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("canvas.getContext('2d') returned null");

  const imageData = ctx.createImageData(width, height);
  const u32 = new Uint32Array(imageData.data.buffer);
  u32.set(framebuffer);

  if (opts.colorSpace === "linear") {
    const data = imageData.data;
    for (let i = 0; i + 3 < data.length; i += 4) {
      data[i + 0] = LINEAR_TO_SRGB_U8[data[i + 0]];
      data[i + 1] = LINEAR_TO_SRGB_U8[data[i + 1]];
      data[i + 2] = LINEAR_TO_SRGB_U8[data[i + 2]];
    }
  }

  ctx.putImageData(imageData, 0, 0);
}
