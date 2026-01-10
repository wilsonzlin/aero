/**
 * Draws an RGBA8888 framebuffer onto a canvas.
 *
 * The expected `framebuffer` format matches `aero-gpu-vga`:
 * - Uint32Array where each element is RGBA8888
 * - On little-endian hosts (all modern browsers), the underlying bytes in memory
 *   are `[R, G, B, A]`, matching Canvas `ImageData`.
 */
export function drawFramebuffer(canvas, framebuffer, width, height) {
  canvas.width = width;
  canvas.height = height;

  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("canvas.getContext('2d') returned null");

  const imageData = ctx.createImageData(width, height);
  const u32 = new Uint32Array(imageData.data.buffer);
  u32.set(framebuffer);
  ctx.putImageData(imageData, 0, 0);
}

