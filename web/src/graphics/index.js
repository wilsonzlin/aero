import { WebGl2Backend } from './webgl2.js';
import { WebGpuBackend } from './webgpu.js';

/**
 * @typedef {'webgpu' | 'webgl2'} GraphicsBackendKind
 *
 * @typedef {'rgba8' | 'bgra8' | 'rgb565' | 'indexed8'} FramebufferFormat
 *
 * @typedef {object} Framebuffer
 * @property {number} width
 * @property {number} height
 * @property {FramebufferFormat} format
 * @property {ArrayBufferView} data
 * @property {Uint8Array | undefined} paletteRgba8 For `indexed8`, 256 * 4 bytes (RGBA order).
 *
 * @typedef {object} Blit
 * @property {number} x Destination x in pixels (top-left origin).
 * @property {number} y Destination y in pixels (top-left origin).
 * @property {number} width
 * @property {number} height
 * @property {FramebufferFormat} format
 * @property {ArrayBufferView} data
 * @property {Uint8Array | undefined} paletteRgba8 For `indexed8`, 256 * 4 bytes (RGBA order).
 *
 * @typedef {object} GraphicsBackend
 * @property {GraphicsBackendKind} kind
 * @property {(framebuffer: Framebuffer, blits?: Blit[]) => void} present
 * @property {() => void} drawTestTriangle
 * @property {() => void} destroy
 */

/**
 * Create the best available graphics backend.
 *
 * Higher layers should treat the returned object as a single interface to avoid
 * scattering feature-detection/conditional logic throughout the codebase.
 *
 * @param {HTMLCanvasElement} canvas
 * @returns {Promise<{ backend: GraphicsBackend }>}
 */
export async function createGraphicsBackend(canvas) {
  if (typeof navigator !== 'undefined' && navigator.gpu) {
    try {
      return { backend: await WebGpuBackend.create(canvas) };
    } catch (err) {
      // Fall through to WebGL2.
      console.warn('WebGPU initialization failed; falling back to WebGL2:', err);
    }
  }

  return { backend: await WebGl2Backend.create(canvas) };
}
