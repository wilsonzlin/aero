import { encodeLinearRgba8ToSrgbInPlace } from '../utils/srgb';
import { formatOneLineError } from '../text';

const TEST_WIDTH = 64;
const TEST_HEIGHT = 64;

// Deterministic SHA-256 over a 64x64 RGBA buffer:
// - top-left quadrant:   red   (255, 0, 0, 255)
// - top-right quadrant:  green (0, 255, 0, 255)
// - bottom-left quadrant: blue  (0, 0, 255, 255)
// - bottom-right quadrant: white (255, 255, 255, 255)
const EXPECTED_TEST_PATTERN_SHA256 =
  'a42e8433ee338fcf505b803b5a52a663478c7009ef85c7652206b4a06d3b76a8';

function stringifyError(err) {
  const msg = formatOneLineError(err, 512);
  let name = '';
  try {
    name = err && typeof err === 'object' && typeof err.name === 'string' ? err.name : '';
  } catch {
    name = '';
  }
  if (name && msg && !msg.toLowerCase().startsWith(name.toLowerCase())) return `${name}: ${msg}`;
  return msg;
}

function createRpc(worker) {
  let nextId = 1;
  const pending = new Map();

  worker.addEventListener('message', (event) => {
    const msg = event.data;
    if (!msg || typeof msg.id !== 'number') return;
    const entry = pending.get(msg.id);
    if (!entry) return;
    pending.delete(msg.id);
    entry.resolve(msg);
  });

  worker.addEventListener('error', (event) => {
    for (const entry of pending.values()) entry.reject(event.error ?? event.message);
    pending.clear();
  });

  return {
    call(type, payload, transfer) {
      const id = nextId++;
      worker.postMessage({ id, type, ...(payload ?? {}) }, transfer ?? []);
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
      });
    },
  };
}

async function main() {
  const statusEl = /** @type {HTMLElement} */ (document.getElementById('status'));
  const benchBtn = /** @type {HTMLButtonElement} */ (document.getElementById('bench'));
  const benchOut = /** @type {HTMLElement} */ (document.getElementById('bench-output'));

  const canvas = /** @type {HTMLCanvasElement} */ (document.getElementById('gpu-canvas'));
  const screenshotCanvas = /** @type {HTMLCanvasElement} */ (
    document.getElementById('screenshot-canvas')
  );

  canvas.width = TEST_WIDTH;
  canvas.height = TEST_HEIGHT;
  screenshotCanvas.width = TEST_WIDTH;
  screenshotCanvas.height = TEST_HEIGHT;

  const params = new URLSearchParams(location.search);
  const backendParam = params.get('backend'); // webgpu | webgl2 | null
  /** @type {{ preferWebGpu?: boolean, forceBackend?: 'webgpu' | 'webgl2' }} */
  const options = {};
  if (backendParam === 'webgl2') options.forceBackend = 'webgl2';
  if (backendParam === 'webgpu') options.forceBackend = 'webgpu';
  if (backendParam === 'preferWebGpuFalse') options.preferWebGpu = false;

  const navigatorGpuAvailable = !!navigator.gpu;

  const worker = new Worker('../workers/gpu_smoke.worker.js', { type: 'module' });
  const rpc = createRpc(worker);

  try {
    const offscreen = canvas.transferControlToOffscreen();
    const ready = await rpc.call(
      'init',
      { canvas: offscreen, width: TEST_WIDTH, height: TEST_HEIGHT, options },
      [offscreen],
    );

    if (ready.type === 'error') throw new Error(ready.error);
    if (ready.type !== 'ready') throw new Error(`Unexpected init response: ${ready.type}`);

    const presented = await rpc.call('present_test_pattern');
    if (presented.type === 'error') throw new Error(presented.error);
    if (presented.type !== 'presented') throw new Error(`Unexpected present response: ${presented.type}`);
    const screenshot = await rpc.call('request_screenshot');
    if (screenshot.type === 'error') throw new Error(screenshot.error);
    if (screenshot.type !== 'screenshot') throw new Error(`Unexpected screenshot response: ${screenshot.type}`);

    const ok = screenshot.hash === EXPECTED_TEST_PATTERN_SHA256;

    const rgba = new Uint8ClampedArray(screenshot.rgba);
    // The worker readback is treated as linear RGBA8 bytes. Canvas2D expects sRGB-encoded bytes,
    // so encode on a copy before displaying the screenshot.
    const rgbaDisplay = new Uint8ClampedArray(rgba);
    encodeLinearRgba8ToSrgbInPlace(
      new Uint8Array(rgbaDisplay.buffer, rgbaDisplay.byteOffset, rgbaDisplay.byteLength),
    );
    const ctx2d = screenshotCanvas.getContext('2d', { alpha: false });
    if (!ctx2d) throw new Error('Failed to acquire 2D context for screenshot canvas');
    ctx2d.putImageData(new ImageData(rgbaDisplay, screenshot.width, screenshot.height), 0, 0);

    statusEl.textContent = [
      `backend: ${screenshot.backend}`,
      `hash: ${screenshot.hash}`,
      `expected: ${EXPECTED_TEST_PATTERN_SHA256}`,
      `match: ${ok}`,
      ``,
      `capabilities: ${JSON.stringify(ready.capabilities, null, 2)}`,
    ].join('\n');

    // Expose deterministic results for Playwright.
    // eslint-disable-next-line no-undef
    window.__gpuSmokeResult = {
      done: true,
      navigatorGpuAvailable,
      backend: screenshot.backend,
      hash: screenshot.hash,
      expected: EXPECTED_TEST_PATTERN_SHA256,
      ok,
      capabilities: ready.capabilities,
    };

    benchBtn.addEventListener('click', async () => {
      benchBtn.disabled = true;
      benchOut.textContent = 'Running bench…';
      try {
        const result = await rpc.call('bench_present', { frames: 120 });
        if (result.type !== 'bench_result') throw new Error(`Unexpected bench response: ${result.type}`);
        benchOut.textContent = JSON.stringify(result.report, null, 2);
      } catch (err) {
        benchOut.textContent = stringifyError(err);
      } finally {
        benchBtn.disabled = false;
      }
    });
  } catch (err) {
    const message = stringifyError(err);
    statusEl.textContent = message;
    // eslint-disable-next-line no-undef
    window.__gpuSmokeResult = { done: true, navigatorGpuAvailable, error: message };
    worker.terminate();
  }
}

main();
