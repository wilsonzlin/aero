/* eslint-disable no-var */
/* eslint-disable prefer-const */

(function init(global) {
  /**
   * Flat-color quadrant scene used for deterministic GPU output tests.
   * Colors are chosen to be representable exactly in 8-bit UNORM formats.
   */
  const QUADRANT_COLORS = {
    topLeft: [255, 0, 0, 255], // red
    topRight: [0, 255, 0, 255], // green
    bottomLeft: [0, 0, 255, 255], // blue
    bottomRight: [255, 255, 0, 255] // yellow
  };

  function generateQuadrantsImageRGBA(width, height) {
    const data = new Uint8Array(width * height * 4);
    const midX = Math.floor(width / 2);
    const midY = Math.floor(height / 2);

    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const idx = (y * width + x) * 4;
        let c;
        if (y < midY && x < midX) c = QUADRANT_COLORS.topLeft;
        else if (y < midY && x >= midX) c = QUADRANT_COLORS.topRight;
        else if (y >= midY && x < midX) c = QUADRANT_COLORS.bottomLeft;
        else c = QUADRANT_COLORS.bottomRight;

        data[idx + 0] = c[0];
        data[idx + 1] = c[1];
        data[idx + 2] = c[2];
        data[idx + 3] = c[3];
      }
    }

    return data;
  }

  const api = { QUADRANT_COLORS, generateQuadrantsImageRGBA };

  // Browser (tests) usage.
  global.AeroTestScenes = global.AeroTestScenes || {};
  global.AeroTestScenes.QUADRANT_COLORS = QUADRANT_COLORS;
  global.AeroTestScenes.generateQuadrantsImageRGBA = generateQuadrantsImageRGBA;

  // Node (golden generator) usage.
  if (typeof module !== 'undefined' && typeof module.exports !== 'undefined') {
    module.exports = api;
  }
})(typeof globalThis !== 'undefined' ? globalThis : window);

