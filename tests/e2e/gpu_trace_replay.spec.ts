import { test, expect } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";

test("gpu trace replays deterministically (triangle)", async ({ page }) => {
  const toolPath = path.resolve(__dirname, "../../web/tools/gpu_trace_replay.ts");
  const tracePath = path.resolve(__dirname, "../fixtures/triangle.aerogputrace");

  const traceBytes = fs.readFileSync(tracePath);
  const traceB64 = traceBytes.toString("base64");

  await page.setContent(`<canvas id="c" width="64" height="64"></canvas>`);
  await page.addScriptTag({ path: toolPath });

  const analysis = await page.evaluate(async (b64) => {
    const raw = atob(b64);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);

    const canvas = document.getElementById("c");
    if (!canvas) throw new Error("missing canvas");

    const gl = canvas.getContext("webgl2");
    if (!gl) throw new Error("WebGL2 unavailable in test environment");

    const replayer = await window.AeroGpuTraceReplay.load(bytes, canvas, { backend: "webgl2" });
    await replayer.replayFrame(0);

    const pixels = new Uint8Array(canvas.width * canvas.height * 4);
    gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

    let nonRed = 0;
    for (let i = 0; i < pixels.length; i += 4) {
      const r = pixels[i + 0];
      const g = pixels[i + 1];
      const b = pixels[i + 2];
      const a = pixels[i + 3];
      // Allow tiny error. (In practice, this should be exact.)
      if (r < 250 || g > 5 || b > 5 || a < 250) nonRed++;
    }

    return { nonRed, totalPixels: pixels.length / 4 };
  }, traceB64);

  expect(analysis.nonRed).toBe(0);
});

