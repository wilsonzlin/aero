import { expect, test } from "@playwright/test";

test("gpu worker falls back to WebGL2 when WebGPU is disabled", async ({ page }) => {
  await page.goto("about:blank");

  const result = await page.evaluate(async () => {
    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);

    const offscreen = canvas.transferControlToOffscreen();

    // In the real app this resolves to the bundled worker module URL.
    const worker = new Worker(new URL("../src/workers/aero-gpu-worker.ts", import.meta.url), {
      type: "module",
    });

    const readyMsg = await new Promise<any>((resolve, reject) => {
      worker.addEventListener("message", (ev: MessageEvent) => {
        if (ev.data?.type === "ready") resolve(ev.data);
        if (ev.data?.type === "gpu_error" && ev.data?.fatal) reject(new Error(ev.data?.error?.message));
      });

      worker.postMessage(
        {
          type: "init",
          canvas: offscreen,
          width: 64,
          height: 64,
          devicePixelRatio: 1,
          gpuOptions: {
            preferWebGpu: true,
            disableWebGpu: true,
          },
        },
        [offscreen],
      );
    });

    worker.postMessage({ type: "present_test_pattern" });

    const screenshot = await new Promise<any>((resolve, reject) => {
      worker.addEventListener("message", (ev: MessageEvent) => {
        if (ev.data?.type === "screenshot" && ev.data?.requestId === 1) resolve(ev.data);
        if (ev.data?.type === "gpu_error" && ev.data?.fatal) reject(new Error(ev.data?.error?.message));
      });
      worker.postMessage({ type: "request_screenshot", requestId: 1 });
    });

    worker.postMessage({ type: "shutdown" });
    worker.terminate();

    return { readyMsg, screenshot };
  });

  expect(result.readyMsg.backendKind).toBe("webgl2");
  expect(result.readyMsg.fallback?.from).toBe("webgpu");
  expect(result.readyMsg.fallback?.to).toBe("webgl2");
  expect(result.screenshot.width).toBeGreaterThan(0);
  expect(result.screenshot.height).toBeGreaterThan(0);
  expect(result.screenshot.rgba8.byteLength).toBe(result.screenshot.width * result.screenshot.height * 4);
});
