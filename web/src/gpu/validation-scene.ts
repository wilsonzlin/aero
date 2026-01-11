import { RawWebGL2Presenter } from "./raw-webgl2-presenter";
import { WebGpuPresenter } from "./webgpu-presenter";
import { createGpuColorTestCardRgba8Linear } from "./test-card";

/**
 * @typedef {"webgpu" | "webgl2"} Backend
 * @typedef {"linear" | "srgb"} ColorSpace
 * @typedef {"opaque" | "premultiplied"} AlphaMode
 */

/**
 * @typedef {object} GpuColorValidationOptions
 * @property {Backend} backend
 * @property {number=} width
 * @property {number=} height
 * @property {ColorSpace=} framebufferColorSpace
 * @property {ColorSpace=} outputColorSpace
 * @property {AlphaMode=} alphaMode
 * @property {boolean=} flipY
 */

function flipRgba8VerticallyInPlace(rgba: Uint8Array, width: number, height: number) {
  const rowBytes = width * 4;
  const tmp = new Uint8Array(rowBytes);
  for (let y = 0; y < Math.floor(height / 2); y++) {
    const top = y * rowBytes;
    const bottom = (height - 1 - y) * rowBytes;
    tmp.set(rgba.subarray(top, top + rowBytes));
    rgba.copyWithin(top, bottom, bottom + rowBytes);
    rgba.set(tmp, bottom);
  }
}

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as unknown as BufferSource);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Render the GPU color validation test card and return a stable hash of the final presented pixels.
 *
 * The hash is computed on **RGBA8, top-left origin** to avoid backend-specific conventions.
 *
 * @param {HTMLCanvasElement} canvas
 * @param {GpuColorValidationOptions} opts
 */
export async function renderGpuColorTestCardAndHash(canvas: HTMLCanvasElement, opts: any): Promise<string> {
  const width = opts.width ?? 256;
  const height = opts.height ?? 256;
  canvas.width = width;
  canvas.height = height;

  const card = createGpuColorTestCardRgba8Linear(width, height);

  const common = {
    framebufferColorSpace: opts.framebufferColorSpace ?? "linear",
    outputColorSpace: opts.outputColorSpace ?? "srgb",
    alphaMode: opts.alphaMode ?? "opaque",
    flipY: opts.flipY ?? false,
  };

  if (opts.backend === "webgl2") {
    const presenter = new RawWebGL2Presenter(canvas, common);
    presenter.setSourceRgba8(card, width, height);
    presenter.present();

    // Read back. WebGL's origin is bottom-left, so flip to match our canonical convention.
    const gl = presenter.gl;
    const out = new Uint8Array(width * height * 4);
    gl.readPixels(0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, out);
    flipRgba8VerticallyInPlace(out, width, height);

    return await sha256Hex(out);
  }

  if (opts.backend === "webgpu") {
    const presenter = await WebGpuPresenter.create(canvas, common);
    presenter.setSourceRgba8(card, width, height);
    const out = await presenter.presentAndReadbackRgba8();
    return await sha256Hex(out);
  }

  throw new Error(`unknown backend: ${opts.backend}`);
}
