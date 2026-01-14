const fs = require('node:fs');
const path = require('node:path');

const { PNG } = require('pngjs');

// This script generates the committed PNG goldens under `tests/golden/`.
//
// CI enforces determinism by running `npm run generate:goldens` and failing if
// `tests/golden/` has uncommitted changes afterwards. If you touch any of the
// scene generators below, rerun this script and commit the updated PNGs.
const { generateQuadrantsImageRGBA } = require('../e2e/playwright/scenes/quadrants_scene.cjs');
const {
  VGA_TEXT_MODE_WIDTH,
  VGA_TEXT_MODE_HEIGHT,
  renderVgaTextModeSceneRGBA
} = require('../e2e/playwright/scenes/vga_text_mode_scene.cjs');

const {
  VBE_LFB_WIDTH,
  VBE_LFB_HEIGHT,
  renderVbeLfbColorBarsRGBA
} = require('../e2e/playwright/scenes/vbe_lfb_scene.cjs');

function writePng(filePath, width, height, rgba) {
  const png = new PNG({ width, height });
  png.data = Buffer.from(rgba);
  const out = PNG.sync.write(png);

  // Avoid rewriting files unnecessarily (helps keep `npm run test:unit` from touching
  // timestamps on a clean checkout).
  try {
    const existing = fs.readFileSync(filePath);
    if (existing.equals(out)) {
      return false;
    }
  } catch (err) {
    if (err && err.code !== 'ENOENT') {
      throw err;
    }
  }

  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, out);
  return true;
}

function generateGpuSmokeQuadrantsRGBA(width, height) {
  const rgba = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      const left = x < width / 2;
      const top = y < height / 2;
      if (top && left) {
        rgba[i + 0] = 255;
        rgba[i + 1] = 0;
        rgba[i + 2] = 0;
        rgba[i + 3] = 255;
      } else if (top && !left) {
        rgba[i + 0] = 0;
        rgba[i + 1] = 255;
        rgba[i + 2] = 0;
        rgba[i + 3] = 255;
      } else if (!top && left) {
        rgba[i + 0] = 0;
        rgba[i + 1] = 0;
        rgba[i + 2] = 255;
        rgba[i + 3] = 255;
      } else {
        rgba[i + 0] = 255;
        rgba[i + 1] = 255;
        rgba[i + 2] = 255;
        rgba[i + 3] = 255;
      }
    }
  }

  return rgba;
}

function generateSolidColorRGBA(width, height, r, g, b, a) {
  const rgba = new Uint8Array(width * height * 4);
  for (let i = 0; i < rgba.length; i += 4) {
    rgba[i + 0] = r;
    rgba[i + 1] = g;
    rgba[i + 2] = b;
    rgba[i + 3] = a;
  }
  return rgba;
}

function main() {
  const outDir = __dirname;
  let wrote = 0;

  const vga = renderVgaTextModeSceneRGBA();
  if (writePng(path.join(outDir, 'vga_text_mode.png'), VGA_TEXT_MODE_WIDTH, VGA_TEXT_MODE_HEIGHT, vga.rgba)) {
    wrote++;
  }

  const vbe = renderVbeLfbColorBarsRGBA();
  if (
    writePng(path.join(outDir, 'vbe_lfb_color_bars_320x200.png'), VBE_LFB_WIDTH, VBE_LFB_HEIGHT, vbe.rgba)
  ) {
    wrote++;
  }

  const quad64 = generateQuadrantsImageRGBA(64, 64);
  if (writePng(path.join(outDir, 'webgl2_quadrants_64.png'), 64, 64, quad64)) {
    wrote++;
  }
  if (writePng(path.join(outDir, 'webgpu_quadrants_64.png'), 64, 64, quad64)) {
    wrote++;
  }

  const smoke64 = generateGpuSmokeQuadrantsRGBA(64, 64);
  if (writePng(path.join(outDir, 'gpu_smoke_quadrants_64.png'), 64, 64, smoke64)) {
    wrote++;
  }

  // Trace replay "triangle" fixture is expected to clear/present solid red.
  const traceRed64 = generateSolidColorRGBA(64, 64, 255, 0, 0, 255);
  if (writePng(path.join(outDir, 'gpu_trace_triangle_red_64.png'), 64, 64, traceRed64)) {
    wrote++;
  }
  // Trace replay fixtures using the AeroGPU A3A0 command stream ABI are also expected to render solid red.
  if (writePng(path.join(outDir, 'gpu_trace_aerogpu_cmd_triangle_64.png'), 64, 64, traceRed64)) {
    wrote++;
  }
  if (writePng(path.join(outDir, 'gpu_trace_aerogpu_a3a0_clear_red_64.png'), 64, 64, traceRed64)) {
    wrote++;
  }

  // eslint-disable-next-line no-console
  if (wrote === 0) {
    console.log(`Goldens already up to date in ${outDir}`);
  } else {
    console.log(`Wrote ${wrote} golden(s) to ${outDir}`);
  }
}

main();
