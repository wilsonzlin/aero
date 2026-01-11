import { expect, test } from "@playwright/test";

// GPU color management validation:
// - WebGPU and raw WebGL2 presenter paths must produce identical output for the same policy.
// - This catches accidental double-gamma, wrong alphaMode, and Y-flip mismatches.
//
// NOTE: This spec assumes Playwright is configured with a Vite dev server rooted at the repo,
// so `/web/...` serves files from the `web/` directory.

async function renderHash(page: any, opts: any): Promise<string> {
  await page.goto("/web/blank.html");
  return await page.evaluate(async (opts: any) => {
    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);
    const mod = await import("/web/src/gpu/validation-scene.ts");
    return await mod.renderGpuColorTestCardAndHash(canvas, opts);
  }, opts);
}

function isWebGPURequired() {
  return process.env.AERO_REQUIRE_WEBGPU === "1";
}

async function webGpuIsUsable(page: any): Promise<boolean> {
  await page.goto("/web/blank.html");
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

    // Some Chromium environments can create an adapter/device but still fail for
    // real rendering/readback (e.g. `GPUBuffer.mapAsync` aborts). Do a minimal
    // render+readback to ensure WebGPU is actually usable before running
    // validation comparisons.
    const adapter = await withTimeout(navigator.gpu.requestAdapter({ powerPreference: "high-performance" }), 1000);
    if (!adapter) return false;

    const device = await withTimeout(adapter.requestDevice(), 1000);
    if (!device) return false;

    try {
      device.destroy?.();
    } catch {
      // Ignore: destroy is optional in older implementations.
    }

    try {
      const canvas = document.createElement("canvas");
      canvas.width = 8;
      canvas.height = 8;
      document.body.appendChild(canvas);
      const mod = await withTimeout(import("/web/src/gpu/validation-scene.ts"), 2000);
      if (!mod) return false;
      const fn = (mod as any).renderGpuColorTestCardAndHash;
      if (typeof fn !== "function") return false;
      const ok = await withTimeout(
        fn(canvas, { backend: "webgpu", width: 8, height: 8, outputColorSpace: "srgb", alphaMode: "opaque" }),
        2000,
      );
      return typeof ok === "string" && ok.length > 0;
    } catch {
      return false;
    }
  });
}

test.describe("gpu color policy", () => {
  test("webgpu and webgl2 match (sRGB + opaque) @webgpu", async ({ page }) => {
    const usable = await webGpuIsUsable(page);
    if (!usable) {
      const message = "WebGPU is not available/usable in this Playwright environment.";
      if (isWebGPURequired()) {
        throw new Error(message);
      }
      test.skip(true, message);
    }

    const common = { width: 128, height: 128, outputColorSpace: "srgb", alphaMode: "opaque" };
    const webgpu = await renderHash(page, { backend: "webgpu", ...common });
    const webgl2 = await renderHash(page, { backend: "webgl2", ...common });
    expect(webgpu).toBe(webgl2);
  });

  test("webgpu and webgl2 match (linear + opaque) @webgpu", async ({ page }) => {
    const usable = await webGpuIsUsable(page);
    if (!usable) {
      const message = "WebGPU is not available/usable in this Playwright environment.";
      if (isWebGPURequired()) {
        throw new Error(message);
      }
      test.skip(true, message);
    }

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
