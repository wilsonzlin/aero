import type { CapturedFrame, FilterMode, PresentationBackend } from './gpu/backend';
import { WebGL2Backend } from './gpu/webgl2_backend';
import { WebGPUBackend } from './gpu/webgpu_backend';

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
    };
  }
}

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

async function main() {
  const canvas = document.getElementById('screen');
  if (!(canvas instanceof HTMLCanvasElement)) {
    renderError('Canvas element not found');
    return;
  }

  try {
    const initOptions = {
      filter: getFilterParam(),
      preserveAspectRatio: getPreserveAspectRatioParam(),
    };

    const requested = getBackendParam();
    let backend: PresentationBackend | null = null;

    if (requested === 'webgpu') {
      backend = new WebGPUBackend();
      await backend.init(canvas, initOptions);
    } else if (requested === 'webgl2') {
      backend = new WebGL2Backend();
      await backend.init(canvas, initOptions);
    } else {
      let webgpuError: string | null = null;
      if (navigator.gpu) {
        try {
          const candidate = new WebGPUBackend();
          await candidate.init(canvas, initOptions);
          backend = candidate;
        } catch (err) {
          webgpuError = err instanceof Error ? err.message : String(err);
        }
      } else {
        webgpuError = 'navigator.gpu is missing';
      }

      if (!backend) {
        try {
          const candidate = new WebGL2Backend();
          await candidate.init(canvas, initOptions);
          backend = candidate;
        } catch (err) {
          const webgl2Error = err instanceof Error ? err.message : String(err);
          throw new Error(`No usable GPU backend (WebGPU: ${webgpuError}; WebGL2: ${webgl2Error})`);
        }
      }
    }

    const width = 64;
    const height = 64;

    const frame = generateQuadrantPattern(width, height);
    backend.uploadFrameRGBA(frame, width, height);
    backend.present();

    const status = document.getElementById('status');
    const caps = backend.getCapabilities();
    if (status) status.textContent = `backend: ${caps.kind}`;

    window.__aeroTest = {
      ready: true,
      backend: caps.kind,
      samplePixels: async () => {
        const captured: CapturedFrame = await backend.captureFrame();

        const sample = (x: number, y: number) => {
          const i = (y * captured.width + x) * 4;
          return [
            captured.data[i + 0],
            captured.data[i + 1],
            captured.data[i + 2],
            captured.data[i + 3],
          ];
        };

        return {
          backend: caps.kind,
          width: captured.width,
          height: captured.height,
          topLeft: sample(8, 8),
          topRight: sample(captured.width - 9, 8),
          bottomLeft: sample(8, captured.height - 9),
          bottomRight: sample(captured.width - 9, captured.height - 9),
        };
      },
    };
  } catch (err) {
    renderError(err instanceof Error ? err.message : String(err));
  }
}

void main();
