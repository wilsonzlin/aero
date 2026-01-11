import { expect, test } from "@playwright/test";

// GPU color management validation:
// - WebGPU and raw WebGL2 presenter paths must produce identical output for the same policy.
// - This catches accidental double-gamma, wrong alphaMode, and Y-flip mismatches.
//
// NOTE: This spec assumes Playwright is configured with a web server that serves `web/`
// such that `/src/...` maps to `web/src/...` (e.g. via Vite).

async function renderHash(page: any, opts: any): Promise<string> {
  await page.goto("/blank.html");
  return await page.evaluate(async (opts: any) => {
    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);
    const mod = await import("/src/gpu/validation-scene.ts");
    return await mod.renderGpuColorTestCardAndHash(canvas, opts);
  }, opts);
}

async function webGpuIsUsable(page: any): Promise<boolean> {
  await page.goto("/blank.html");
  return await page.evaluate(async () => {
    if (!navigator.gpu) return false;

    const withTimeout = async <T>(promise: Promise<T>, ms: number): Promise<T | null> => {
      return await Promise.race([
        promise,
        new Promise<null>((resolve) => {
          setTimeout(() => resolve(null), ms);
        }),
      ]);
    };

    const adapter = await withTimeout(navigator.gpu.requestAdapter({ powerPreference: "high-performance" }), 1000);
    if (!adapter) return false;

    const device = await withTimeout(adapter.requestDevice(), 1000);
    if (!device) return false;

    try {
      device.destroy?.();
    } catch {
      // Ignore: destroy is optional in older implementations.
    }

    return true;
  });
}

test.describe("gpu color policy", () => {
  test("webgpu and webgl2 match (sRGB + opaque)", async ({ page }) => {
    test.skip(!(await webGpuIsUsable(page)), "WebGPU is not available/usable in this Playwright environment.");

    const common = { width: 128, height: 128, outputColorSpace: "srgb", alphaMode: "opaque" };
    const webgpu = await renderHash(page, { backend: "webgpu", ...common });
    const webgl2 = await renderHash(page, { backend: "webgl2", ...common });
    expect(webgpu).toBe(webgl2);
  });

  test("webgpu and webgl2 match (linear + opaque)", async ({ page }) => {
    test.skip(!(await webGpuIsUsable(page)), "WebGPU is not available/usable in this Playwright environment.");

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
