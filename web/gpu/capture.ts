/**
 * Browser-side image capture helpers intended for deterministic test readback.
 *
 * These utilities are kept dependency-free so they can be used by both the
 * emulator runtime (for debug capture) and by Playwright microtests.
 */
export type CapturedFrame = {
  width: number;
  height: number;
  rgba: Uint8Array;
};

export function captureCanvas2dRGBA(canvas: HTMLCanvasElement): CapturedFrame {
  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error('2d context unavailable');
  const { width, height } = canvas;
  const img = ctx.getImageData(0, 0, width, height);
  return { width, height, rgba: new Uint8Array(img.data.buffer.slice(0)) };
}

export function captureWebGL2RGBA(gl: WebGL2RenderingContext, width: number, height: number): CapturedFrame {
  const pixels = new Uint8Array(width * height * 4);
  gl.readPixels(0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

  // WebGL origin is bottom-left; flip to top-left origin expected by image diffs.
  const flipped = new Uint8Array(width * height * 4);
  const rowSize = width * 4;
  for (let y = 0; y < height; y++) {
    const srcStart = y * rowSize;
    const dstStart = (height - 1 - y) * rowSize;
    flipped.set(pixels.subarray(srcStart, srcStart + rowSize), dstStart);
  }

  return { width, height, rgba: flipped };
}

export async function captureWebGPUCanvasRGBA(
  device: GPUDevice,
  canvasContext: GPUCanvasContext,
  width: number,
  height: number,
  format: GPUTextureFormat = ((navigator as any).gpu?.getPreferredCanvasFormat?.() as GPUTextureFormat | undefined) ??
    'bgra8unorm'
): Promise<CapturedFrame> {
  const texture = canvasContext.getCurrentTexture();
  const isBGRA = String(format).startsWith('bgra');

  const bytesPerPixel = 4;
  const unpaddedBytesPerRow = width * bytesPerPixel;
  const align = (n: number, a: number) => Math.ceil(n / a) * a;
  const bytesPerRow = align(unpaddedBytesPerRow, 256);

  const readback = device.createBuffer({
    size: bytesPerRow * height,
    usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ
  });

  const encoder = device.createCommandEncoder();
  encoder.copyTextureToBuffer(
    { texture },
    { buffer: readback, bytesPerRow },
    { width, height, depthOrArrayLayers: 1 }
  );
  device.queue.submit([encoder.finish()]);

  await readback.mapAsync(GPUMapMode.READ);
  const mapped = new Uint8Array(readback.getMappedRange());

  // Canvas preferred format is typically bgra8unorm. Convert to RGBA and strip row padding.
  const rgba = new Uint8Array(width * height * 4);
  for (let y = 0; y < height; y++) {
    const srcRow = y * bytesPerRow;
    const dstRow = y * unpaddedBytesPerRow;
    for (let x = 0; x < width; x++) {
      const si = srcRow + x * 4;
      const di = dstRow + x * 4;
      const c0 = mapped[si + 0];
      const c1 = mapped[si + 1];
      const c2 = mapped[si + 2];
      const c3 = mapped[si + 3];
      if (isBGRA) {
        rgba[di + 0] = c2;
        rgba[di + 1] = c1;
        rgba[di + 2] = c0;
        rgba[di + 3] = c3;
      } else {
        rgba[di + 0] = c0;
        rgba[di + 1] = c1;
        rgba[di + 2] = c2;
        rgba[di + 3] = c3;
      }
    }
  }

  readback.unmap();
  return { width, height, rgba };
}
