/* eslint-disable no-var */
/* eslint-disable prefer-const */

(function init(global) {
  // Synthetic glyphs created for this repository (not copied from VGA BIOS ROMs).
  // The scene is intentionally minimal: it validates deterministic char+attr rendering
  // without introducing font licensing concerns.
  const GLYPHS_8x8 = {
    ' ': [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    A: [0x18, 0x24, 0x42, 0x7e, 0x42, 0x42, 0x42, 0x00],
    E: [0x7e, 0x40, 0x40, 0x7c, 0x40, 0x40, 0x7e, 0x00],
    O: [0x3c, 0x42, 0x42, 0x42, 0x42, 0x42, 0x3c, 0x00],
    R: [0x7c, 0x42, 0x42, 0x7c, 0x48, 0x44, 0x42, 0x00]
  };

  // Standard-ish VGA 16-color palette in 8-bit sRGB-ish values.
  const VGA_PALETTE = [
    [0x00, 0x00, 0x00], // 0 black
    [0x00, 0x00, 0xaa], // 1 blue
    [0x00, 0xaa, 0x00], // 2 green
    [0x00, 0xaa, 0xaa], // 3 cyan
    [0xaa, 0x00, 0x00], // 4 red
    [0xaa, 0x00, 0xaa], // 5 magenta
    [0xaa, 0x55, 0x00], // 6 brown
    [0xaa, 0xaa, 0xaa], // 7 light grey
    [0x55, 0x55, 0x55], // 8 dark grey
    [0x55, 0x55, 0xff], // 9 light blue
    [0x55, 0xff, 0x55], // 10 light green
    [0x55, 0xff, 0xff], // 11 light cyan
    [0xff, 0x55, 0x55], // 12 light red
    [0xff, 0x55, 0xff], // 13 light magenta
    [0xff, 0xff, 0x55], // 14 yellow
    [0xff, 0xff, 0xff] // 15 white
  ];

  const COLS = 80;
  const ROWS = 25;
  const CELL_W = 8;
  const CELL_H = 16; // 8x8 glyph vertically doubled

  const VGA_TEXT_MODE_WIDTH = COLS * CELL_W;
  const VGA_TEXT_MODE_HEIGHT = ROWS * CELL_H;

  function renderVgaTextModeSceneRGBA() {
    const width = VGA_TEXT_MODE_WIDTH;
    const height = VGA_TEXT_MODE_HEIGHT;
    const rgba = new Uint8Array(width * height * 4);

    const chars = new Array(COLS * ROWS).fill(' ');
    const attrs = new Uint8Array(COLS * ROWS);

    // Default: light grey on blue (classic DOS-ish look).
    const defaultAttr = (1 << 4) | 7;
    attrs.fill(defaultAttr);

    // Write "AERO" with varied foreground/background attrs to validate both planes.
    // attr = (bg << 4) | fg
    chars[0 * COLS + 0] = 'A';
    attrs[0 * COLS + 0] = (1 << 4) | 15; // white on blue

    chars[0 * COLS + 1] = 'E';
    attrs[0 * COLS + 1] = (2 << 4) | 14; // yellow on green

    chars[0 * COLS + 2] = 'R';
    attrs[0 * COLS + 2] = (4 << 4) | 11; // light cyan on red

    chars[0 * COLS + 3] = 'O';
    attrs[0 * COLS + 3] = (0 << 4) | 10; // light green on black

    // Corner marker (bottom-right) to validate coordinates.
    chars[(ROWS - 1) * COLS + (COLS - 1)] = 'A';
    attrs[(ROWS - 1) * COLS + (COLS - 1)] = (15 << 4) | 0; // black on white

    for (let row = 0; row < ROWS; row++) {
      for (let col = 0; col < COLS; col++) {
        const cellIdx = row * COLS + col;
        const ch = chars[cellIdx];
        const attr = attrs[cellIdx];
        const fgIdx = attr & 0xf;
        const bgIdx = (attr >> 4) & 0xf;
        const fg = VGA_PALETTE[fgIdx];
        const bg = VGA_PALETTE[bgIdx];
        const glyph = GLYPHS_8x8[ch] || GLYPHS_8x8[' '];

        for (let y = 0; y < CELL_H; y++) {
          const glyphRow = glyph[Math.floor(y / 2)] || 0;
          const py = row * CELL_H + y;
          for (let x = 0; x < CELL_W; x++) {
            const px = col * CELL_W + x;
            const bit = (glyphRow >> (7 - x)) & 1;
            const c = bit ? fg : bg;
            const p = (py * width + px) * 4;
            rgba[p + 0] = c[0];
            rgba[p + 1] = c[1];
            rgba[p + 2] = c[2];
            rgba[p + 3] = 0xff;
          }
        }
      }
    }

    return { width, height, rgba };
  }

  const api = {
    VGA_TEXT_MODE_WIDTH,
    VGA_TEXT_MODE_HEIGHT,
    renderVgaTextModeSceneRGBA
  };

  global.AeroTestScenes = global.AeroTestScenes || {};
  global.AeroTestScenes.VGA_TEXT_MODE_WIDTH = VGA_TEXT_MODE_WIDTH;
  global.AeroTestScenes.VGA_TEXT_MODE_HEIGHT = VGA_TEXT_MODE_HEIGHT;
  global.AeroTestScenes.renderVgaTextModeSceneRGBA = renderVgaTextModeSceneRGBA;

  if (typeof module !== 'undefined' && typeof module.exports !== 'undefined') {
    module.exports = api;
  }
})(typeof globalThis !== 'undefined' ? globalThis : window);

