import { createGraphicsBackend } from './graphics/index.js';
import { formatOneLineError } from './text';

const canvas = /** @type {HTMLCanvasElement} */ (document.getElementById('frame'));
const backendEl = document.getElementById('backend');
const logEl = document.getElementById('log');
const btnFramebuffer = document.getElementById('btn-framebuffer');
const btnTriangle = document.getElementById('btn-triangle');

/** @param {string} msg */
function log(msg) {
  logEl.textContent = msg;
}

/** @type {import('./graphics/index.js').GraphicsBackend | null} */
let backend = null;

let mode = 'framebuffer';

btnFramebuffer.addEventListener('click', () => {
  mode = 'framebuffer';
});
btnTriangle.addEventListener('click', () => {
  mode = 'triangle';
});

async function main() {
  try {
    const result = await createGraphicsBackend(canvas);
    backend = result.backend;
    backendEl.textContent = result.backend.kind === 'webgpu' ? 'WebGPU' : 'WebGL2';
    log('');
  } catch (err) {
    backendEl.textContent = 'Unavailable';
    log(`Failed to initialize graphics backend:\n${formatOneLineError(err, 512)}`);
    return;
  }

  const width = 640;
  const height = 480;
  canvas.width = width;
  canvas.height = height;

  const rgba = new Uint8Array(width * height * 4);
  let t = 0;
  let firstPresent = false;

  /** @param {number} timeMs */
  function frame(timeMs) {
    if (!backend) return;

    if (mode === 'triangle') {
      backend.drawTestTriangle();
      if (!firstPresent) {
        // Used by Playwright smoke tests.
        window.__AERO_DEMO_FIRST_PRESENT = true;
        firstPresent = true;
      }
      requestAnimationFrame(frame);
      return;
    }

    const time = Math.floor(timeMs / 16);
    // Simple moving gradient; deterministic + non-black center pixel.
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const idx = (y * width + x) * 4;
        rgba[idx + 0] = (x + time) & 0xff;
        rgba[idx + 1] = (y + time * 2) & 0xff;
        rgba[idx + 2] = (128 + (t & 0x7f)) & 0xff;
        rgba[idx + 3] = 0xff;
      }
    }
    t++;

    backend.present({
      width,
      height,
      format: 'rgba8',
      data: rgba,
    });

    if (!firstPresent) {
      // Used by Playwright smoke tests.
      window.__AERO_DEMO_FIRST_PRESENT = true;
      firstPresent = true;
    }

    requestAnimationFrame(frame);
  }

  requestAnimationFrame(frame);
}

main();
