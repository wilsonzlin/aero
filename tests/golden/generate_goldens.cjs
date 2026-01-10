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

function main() {
  const outDir = __dirname;

  const vga = renderVgaTextModeSceneRGBA();
  writePng(path.join(outDir, 'vga_text_mode.png'), VGA_TEXT_MODE_WIDTH, VGA_TEXT_MODE_HEIGHT, vga.rgba);

  const vbe = renderVbeLfbColorBarsRGBA();
  writePng(path.join(outDir, 'vbe_lfb_color_bars_320x200.png'), VBE_LFB_WIDTH, VBE_LFB_HEIGHT, vbe.rgba);

  const quad64 = generateQuadrantsImageRGBA(64, 64);
  writePng(path.join(outDir, 'webgl2_quadrants_64.png'), 64, 64, quad64);
  writePng(path.join(outDir, 'webgpu_quadrants_64.png'), 64, 64, quad64);

  // eslint-disable-next-line no-console
  console.log(`Wrote goldens to ${outDir}`);
}

main();
