import { RawWebGL2Presenter } from './src/gpu/raw-webgl2-presenter';
import { WebGpuPresenter } from './src/gpu/webgpu-presenter';
import { formatOneLineError } from './src/text';

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      backend?: string;
      error?: string;
      samplePixels?: () => Promise<{
        backend: string;
        width: number;
        height: number;
        topLeft: number[];
        topRight: number[];
        bottomLeft: number[];
        bottomRight: number[];
      }>;
      captureFrameBase64?: () => Promise<{
        backend: string;
        width: number;
        height: number;
        rgbaBase64: string;
      }>;
    };
  }
}

function u8ToBase64(u8: Uint8Array): string {
  // Avoid `btoa(String.fromCharCode(...))` stack limits by chunking.
  const chunkSize = 0x8000;
  let binary = '';
  for (let i = 0; i < u8.length; i += chunkSize) {
    const chunk = u8.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary);
}

type BackendKind = 'webgpu' | 'webgl2';
type FilterMode = 'nearest' | 'linear';

function renderError(message: string) {
  const status = document.getElementById('status');
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message };
}

function getBackendParam(): 'auto' | 'webgpu' | 'webgl2' {
  const url = new URL(window.location.href);
  const backend = url.searchParams.get('backend');
  if (backend === 'webgpu' || backend === 'webgl2') return backend;
  return 'auto';
}

function getFilterParam(): FilterMode {
  const url = new URL(window.location.href);
  const filter = url.searchParams.get('filter');
  return filter === 'linear' ? 'linear' : 'nearest';
}

function getPreserveAspectRatioParam(): boolean {
  const url = new URL(window.location.href);
  const aspect = url.searchParams.get('aspect');
  return aspect !== 'stretch';
}

function generateQuadrantPattern(width: number, height: number): Uint8Array {
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      const left = x < width / 2;
      const top = y < height / 2;

      if (top && left) {
        out[i + 0] = 255;
        out[i + 1] = 0;
        out[i + 2] = 0;
        out[i + 3] = 255;
      } else if (top && !left) {
        out[i + 0] = 0;
        out[i + 1] = 255;
        out[i + 2] = 0;
        out[i + 3] = 255;
      } else if (!top && left) {
        out[i + 0] = 0;
        out[i + 1] = 0;
        out[i + 2] = 255;
        out[i + 3] = 255;
      } else {
        out[i + 0] = 255;
        out[i + 1] = 255;
        out[i + 2] = 255;
        out[i + 3] = 255;
      }
    }
  }

  return out;
}

function applyWebGl2Filter(presenter: RawWebGL2Presenter, filter: FilterMode): void {
  const gl = presenter.gl as WebGL2RenderingContext;
  gl.bindTexture(gl.TEXTURE_2D, presenter.srcTex as WebGLTexture);
  const mode = filter === 'linear' ? gl.LINEAR : gl.NEAREST;
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, mode);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, mode);
  gl.bindTexture(gl.TEXTURE_2D, null);
}

function readPixelsTopLeft(gl: WebGL2RenderingContext, width: number, height: number): Uint8Array {
  const pixels = new Uint8Array(width * height * 4);
  gl.readPixels(0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

  const rowBytes = width * 4;
  const flipped = new Uint8Array(pixels.length);
  for (let y = 0; y < height; y++) {
    const srcOff = (height - 1 - y) * rowBytes;
    const dstOff = y * rowBytes;
    flipped.set(pixels.subarray(srcOff, srcOff + rowBytes), dstOff);
  }
  return flipped;
}

async function main() {
  const canvas = document.getElementById('screen');
  if (!(canvas instanceof HTMLCanvasElement)) {
    renderError('Canvas element not found');
    return;
  }

  try {
    const requested = getBackendParam();
    const width = 64;
    const height = 64;
    const frame = generateQuadrantPattern(width, height);
    const status = document.getElementById('status');
    const preserveAspectRatio = getPreserveAspectRatioParam();
    void preserveAspectRatio; // kept for URL compatibility; presenters currently always stretch.

    const filter = getFilterParam();

    let backend: BackendKind | null = null;
    let capture: (() => Promise<Uint8Array>) | null = null;

    if (requested === 'webgpu') {
      const presenter = await WebGpuPresenter.create(canvas, {
        framebufferColorSpace: 'linear',
        outputColorSpace: 'srgb',
        alphaMode: 'opaque',
        flipY: false,
      });

      presenter.setSourceRgba8(frame, width, height);
      presenter.present();

      backend = 'webgpu';
      capture = async () => {
        presenter.setSourceRgba8(frame, width, height);
        return await presenter.presentAndReadbackRgba8();
      };
    } else if (requested === 'webgl2') {
      const presenter = new RawWebGL2Presenter(canvas, {
        framebufferColorSpace: 'linear',
        outputColorSpace: 'srgb',
        alphaMode: 'opaque',
        flipY: false,
      });
      applyWebGl2Filter(presenter, filter);
      presenter.setSourceRgba8(frame, width, height);
      presenter.present();

      backend = 'webgl2';
      capture = async () => {
        presenter.setSourceRgba8(frame, width, height);
        presenter.present();
        const gl = presenter.gl as WebGL2RenderingContext;
        gl.finish();
        return readPixelsTopLeft(gl, width, height);
      };
    } else {
      let webgpuError: string | null = null;

      if (navigator.gpu) {
        try {
          const presenter = await WebGpuPresenter.create(canvas, {
            framebufferColorSpace: 'linear',
            outputColorSpace: 'srgb',
            alphaMode: 'opaque',
            flipY: false,
          });
          presenter.setSourceRgba8(frame, width, height);
          presenter.present();

          backend = 'webgpu';
          capture = async () => {
            presenter.setSourceRgba8(frame, width, height);
            return await presenter.presentAndReadbackRgba8();
          };
        } catch (err) {
          webgpuError = formatOneLineError(err, 512);
        }
      } else {
        webgpuError = 'navigator.gpu is missing';
      }

      if (!backend) {
        try {
          const presenter = new RawWebGL2Presenter(canvas, {
            framebufferColorSpace: 'linear',
            outputColorSpace: 'srgb',
            alphaMode: 'opaque',
            flipY: false,
          });
          applyWebGl2Filter(presenter, filter);
          presenter.setSourceRgba8(frame, width, height);
          presenter.present();

          backend = 'webgl2';
          capture = async () => {
            presenter.setSourceRgba8(frame, width, height);
            presenter.present();
            const gl = presenter.gl as WebGL2RenderingContext;
            gl.finish();
            return readPixelsTopLeft(gl, width, height);
          };
        } catch (err) {
          const webgl2Error = formatOneLineError(err, 512);
          throw new Error(`No usable GPU backend (WebGPU: ${webgpuError}; WebGL2: ${webgl2Error})`);
        }
      }
    }

    if (!backend || !capture) {
      throw new Error('GPU backend init failed');
    }

    if (status) status.textContent = `backend: ${backend}`;

    window.__aeroTest = {
      ready: true,
      backend,
      samplePixels: async () => {
        const captured = await capture();

        const sample = (x: number, y: number) => {
          const i = (y * width + x) * 4;
          return [
            captured[i + 0],
            captured[i + 1],
            captured[i + 2],
            captured[i + 3],
          ];
        };

        return {
          backend,
          width,
          height,
          topLeft: sample(8, 8),
          topRight: sample(width - 9, 8),
          bottomLeft: sample(8, height - 9),
          bottomRight: sample(width - 9, height - 9),
        };
      },
      captureFrameBase64: async () => {
        const bytes = await capture();
        return {
          backend,
          width,
          height,
          rgbaBase64: u8ToBase64(bytes),
        };
      },
    };
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

void main();
