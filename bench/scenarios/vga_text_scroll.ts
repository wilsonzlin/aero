/**
 * VGA text scroll stress.
 *
 * This is meant to approximate the "text mode" path where a lot of small glyph
 * updates and scroll operations occur. We implement it using Canvas2D, as this
 * is the closest browser primitive without depending on the emulator core.
 */

function registerScenario(scenario) {
  const g = /** @type {any} */ (globalThis);
  g.__aeroGpuBenchScenarios = g.__aeroGpuBenchScenarios ?? {};
  g.__aeroGpuBenchScenarios[scenario.id] = scenario;
}

/**
 * @param {number} frames
 * @param {(ts:number, frameIndex:number) => void} onFrame
 */
function runRafFrames(frames, onFrame) {
  return new Promise((resolve) => {
    let i = 0;
    const step = (ts) => {
      onFrame(ts, i);
      i += 1;
      if (i < frames) {
        requestAnimationFrame(step);
      } else {
        resolve();
      }
    };
    requestAnimationFrame(step);
  });
}

/**
 * Generate a pseudo "text line" without allocating a lot of intermediate
 * objects. This avoids measuring JS allocator churn instead of draw cost.
 *
 * @param {number} frameIndex
 * @param {number} cols
 */
function makeLine(frameIndex, cols) {
  const base = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
  let out = "";
  for (let i = 0; i < cols; i += 1) {
    out += base[(frameIndex + i) % base.length];
  }
  return out;
}

export const scenario = {
  id: "vga_text_scroll",
  name: "VGA text scroll stress",
  defaultParams: {
    frames: 240,
    width: 640,
    height: 400,
    cols: 80,
    fontPx: 14,
  },

  /**
   * @param {{canvas: HTMLCanvasElement, telemetry: any, params?: any}} ctx
   */
  async run(ctx) {
    const params = { ...scenario.defaultParams, ...(ctx.params ?? {}) };

    ctx.canvas.width = params.width;
    ctx.canvas.height = params.height;

    const g2d = ctx.canvas.getContext("2d", { alpha: false });
    if (!g2d) {
      return { status: "skipped", reason: "Canvas2D context unavailable", api: "2d", params };
    }

    g2d.font = `${params.fontPx}px monospace`;
    g2d.textBaseline = "top";
    g2d.fillStyle = "#000";
    g2d.fillRect(0, 0, ctx.canvas.width, ctx.canvas.height);

    const lineH = params.fontPx + 2;

    // Pre-fill so initial frames aren't dominated by first paint.
    g2d.fillStyle = "#0f0";
    for (let y = 0; y < ctx.canvas.height; y += lineH) {
      g2d.fillText(makeLine(y / lineH, params.cols), 0, y);
    }

    await runRafFrames(params.frames, (ts, frameIndex) => {
      ctx.telemetry.beginFrame(ts);

      // Scroll up by one line.
      g2d.drawImage(
        ctx.canvas,
        0,
        lineH,
        ctx.canvas.width,
        ctx.canvas.height - lineH,
        0,
        0,
        ctx.canvas.width,
        ctx.canvas.height - lineH,
      );

      // Clear last line.
      g2d.fillStyle = "#000";
      g2d.fillRect(0, ctx.canvas.height - lineH, ctx.canvas.width, lineH);

      // Draw new line.
      g2d.fillStyle = "#0f0";
      g2d.fillText(makeLine(frameIndex, params.cols), 0, ctx.canvas.height - lineH);

      ctx.telemetry.endFrame(performance.now());
    });

    return { status: "ok", api: "2d", params };
  },
};

registerScenario(scenario);

