const fs = require('node:fs');
const path = require('node:path');

const { PNG } = require('pngjs');

const { generateQuadrantsImageRGBA } = require('../playwright/scenes/quadrants_scene.cjs');
const {
  VGA_TEXT_MODE_WIDTH,
  VGA_TEXT_MODE_HEIGHT,
  renderVgaTextModeSceneRGBA
} = require('../playwright/scenes/vga_text_mode_scene.cjs');

const {
  VBE_LFB_WIDTH,
  VBE_LFB_HEIGHT,
  renderVbeLfbColorBarsRGBA
} = require('../playwright/scenes/vbe_lfb_scene.cjs');

function writePng(filePath, width, height, rgba) {
  const png = new PNG({ width, height });
  png.data = Buffer.from(rgba);
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, PNG.sync.write(png));
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

  const vga = renderVgaTextModeSceneRGBA();
  writePng(path.join(outDir, 'vga_text_mode.png'), VGA_TEXT_MODE_WIDTH, VGA_TEXT_MODE_HEIGHT, vga.rgba);

  const vbe = renderVbeLfbColorBarsRGBA();
  writePng(path.join(outDir, 'vbe_lfb_color_bars_320x200.png'), VBE_LFB_WIDTH, VBE_LFB_HEIGHT, vbe.rgba);

  const quad64 = generateQuadrantsImageRGBA(64, 64);
  writePng(path.join(outDir, 'webgl2_quadrants_64.png'), 64, 64, quad64);
  writePng(path.join(outDir, 'webgpu_quadrants_64.png'), 64, 64, quad64);

  const smoke64 = generateGpuSmokeQuadrantsRGBA(64, 64);
  writePng(path.join(outDir, 'gpu_smoke_quadrants_64.png'), 64, 64, smoke64);

  // Trace replay "triangle" fixture is expected to clear/present solid red.
  const traceRed64 = generateSolidColorRGBA(64, 64, 255, 0, 0, 255);
  writePng(path.join(outDir, 'gpu_trace_triangle_red_64.png'), 64, 64, traceRed64);

  // eslint-disable-next-line no-console
  console.log(`Wrote goldens to ${outDir}`);
}

main();
