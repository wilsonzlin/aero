/* eslint-disable no-var */
/* eslint-disable prefer-const */

(function init(global) {
  /**
   * VBE-style linear framebuffer microtest.
   *
   * We intentionally render a pure CPU-generated pattern (color bars) so the golden is
   * independent of any GPU rasterization rules; this validates the plumbing from an
   * emulated/translated LFB into an RGBA canvas image.
   */
  const VBE_LFB_WIDTH = 320;
  const VBE_LFB_HEIGHT = 200;

  const BARS = [
    [255, 0, 0, 255], // red
    [0, 255, 0, 255], // green
    [0, 0, 255, 255], // blue
    [255, 255, 0, 255], // yellow
    [255, 0, 255, 255], // magenta
    [0, 255, 255, 255], // cyan
    [255, 255, 255, 255], // white
    [0, 0, 0, 255] // black
  ];

  function renderVbeLfbColorBarsRGBA() {
    const width = VBE_LFB_WIDTH;
    const height = VBE_LFB_HEIGHT;
    const rgba = new Uint8Array(width * height * 4);

    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const bar = Math.floor((x * BARS.length) / width);
        const c = BARS[Math.min(BARS.length - 1, bar)];
        const i = (y * width + x) * 4;
        rgba[i + 0] = c[0];
        rgba[i + 1] = c[1];
        rgba[i + 2] = c[2];
        rgba[i + 3] = c[3];
      }
    }

    return { width, height, rgba };
  }

  const api = {
    VBE_LFB_WIDTH,
    VBE_LFB_HEIGHT,
    renderVbeLfbColorBarsRGBA
  };

  global.AeroTestScenes = global.AeroTestScenes || {};
  global.AeroTestScenes.VBE_LFB_WIDTH = VBE_LFB_WIDTH;
  global.AeroTestScenes.VBE_LFB_HEIGHT = VBE_LFB_HEIGHT;
  global.AeroTestScenes.renderVbeLfbColorBarsRGBA = renderVbeLfbColorBarsRGBA;

  if (typeof module !== 'undefined' && typeof module.exports !== 'undefined') {
    module.exports = api;
  }
})(typeof globalThis !== 'undefined' ? globalThis : window);

