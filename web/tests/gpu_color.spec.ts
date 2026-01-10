import { expect, test } from "@playwright/test";

// GPU color management validation:
// - WebGPU and raw WebGL2 presenter paths must produce identical output for the same policy.
// - This catches accidental double-gamma, wrong alphaMode, and Y-flip mismatches.
//
// NOTE: This spec assumes Playwright is configured with a web server that serves `web/`
// such that `/src/...` maps to `web/src/...` (e.g. via Vite).

async function renderHash(page: any, opts: any): Promise<string> {
  await page.setContent(`
    <style>
      html, body { margin: 0; padding: 0; background: #000; }
      canvas { width: ${opts.width ?? 256}px; height: ${opts.height ?? 256}px; }
    </style>
    <canvas id="c"></canvas>
    <script type="module">
      import { renderGpuColorTestCardAndHash } from "/src/gpu/validation-scene.ts";
      const canvas = /** @type {HTMLCanvasElement} */ (document.getElementById("c"));
      window.__GPU_HASH__ = await renderGpuColorTestCardAndHash(canvas, ${JSON.stringify(opts)});
    </script>
  `);

  await page.waitForFunction(() => (window as any).__GPU_HASH__);
  return await page.evaluate(() => (window as any).__GPU_HASH__);
}

test.describe("gpu color policy", () => {
  test("webgpu and webgl2 match (sRGB + opaque)", async ({ page }) => {
    const common = { width: 128, height: 128, outputColorSpace: "srgb", alphaMode: "opaque" };
    const webgpu = await renderHash(page, { backend: "webgpu", ...common });
    const webgl2 = await renderHash(page, { backend: "webgl2", ...common });
    expect(webgpu).toBe(webgl2);
  });

  test("webgpu and webgl2 match (linear + opaque)", async ({ page }) => {
    const common = { width: 128, height: 128, outputColorSpace: "linear", alphaMode: "opaque" };
    const webgpu = await renderHash(page, { backend: "webgpu", ...common });
    const webgl2 = await renderHash(page, { backend: "webgl2", ...common });
    expect(webgpu).toBe(webgl2);
  });

  test("debug toggle: premultiplied alpha changes output (sRGB)", async ({ page }) => {
    const common = { width: 128, height: 128, outputColorSpace: "srgb" };
    const opaque = await renderHash(page, { backend: "webgl2", ...common, alphaMode: "opaque" });
    const premul = await renderHash(page, { backend: "webgl2", ...common, alphaMode: "premultiplied" });
    expect(opaque).not.toBe(premul);
  });
});

