import { expect, test } from "@playwright/test";

test("gpu worker falls back to WebGL2 when WebGPU is disabled", async ({ page }) => {
  await page.goto("/web/blank.html");

  // Expected hash for a solid-white 64x64 RGBA8 frame (SHA-256 over raw bytes).
  const expectedWhiteHash = "0fbba07a833d4dcfc7024eaf313661a0ba8f80a05c6d29b8801c612e10e60dee";

  const result = await page.evaluate(async () => {
    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);

    const offscreen = canvas.transferControlToOffscreen();

    // Simple framebuffer_protocol (AERO) layout: 8 i32 header + RGBA bytes.
    const width = 64;
    const height = 64;
    const strideBytes = width * 4;
    const headerBytes = 8 * 4;

    const sharedFramebuffer = new SharedArrayBuffer(headerBytes + strideBytes * height);
    const header = new Int32Array(sharedFramebuffer, 0, 8);
    const pixels = new Uint8Array(sharedFramebuffer, headerBytes, strideBytes * height);

    // Header fields from `src/display/framebuffer_protocol.ts` (inlined for a
    // self-contained smoke test).
    header[0] = 0x4f524541; // FRAMEBUFFER_MAGIC ("AERO")
    header[1] = 1; // FRAMEBUFFER_VERSION
    header[2] = width;
    header[3] = height;
    header[4] = strideBytes;
    header[5] = 1; // FRAMEBUFFER_FORMAT_RGBA8888
    header[6] = 0; // frame counter
    header[7] = 1; // config counter

    const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
    const frameState = new Int32Array(sharedFrameState);
    frameState[0] = 0; // FRAME_PRESENTED
    frameState[1] = 0; // seq

    const worker = new Worker("/web/src/workers/gpu.worker.ts", { type: "module" });

    const readyMsg = await new Promise<any>((resolve, reject) => {
      worker.addEventListener("message", (ev: MessageEvent) => {
        if (ev.data?.type === "ready") resolve(ev.data);
        if (ev.data?.type === "error") reject(new Error(ev.data?.message ?? "gpu worker error"));
      });

      worker.postMessage(
        {
          type: "init",
          canvas: offscreen,
          sharedFrameState,
          sharedFramebuffer,
          sharedFramebufferOffsetBytes: 0,
          options: {
            preferWebGpu: true,
            disableWebGpu: true,
            outputWidth: width,
            outputHeight: height,
            dpr: 1,
          },
        },
        [offscreen],
      );
    });

    // Publish a frame and tick once so screenshot has content.
    pixels.fill(0xff); // solid white
    Atomics.add(header, 6, 1);
    Atomics.add(frameState, 1, 1);
    Atomics.store(frameState, 0, 1); // FRAME_DIRTY
    worker.postMessage({ type: "tick", frameTimeMs: performance.now() });

    const screenshot = await new Promise<any>((resolve, reject) => {
      worker.addEventListener("message", (ev: MessageEvent) => {
        if (ev.data?.type === "screenshot" && ev.data?.requestId === 1) resolve(ev.data);
        if (ev.data?.type === "error") reject(new Error(ev.data?.message ?? "gpu worker error"));
      });
      worker.postMessage({ type: "screenshot", requestId: 1 });
    });

    const digest = await crypto.subtle.digest("SHA-256", screenshot.rgba8);
    const hash = Array.from(new Uint8Array(digest))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");

    worker.postMessage({ type: "shutdown" });
    worker.terminate();

    return { readyMsg, screenshot: { width: screenshot.width, height: screenshot.height, hash } };
  });

  expect(result.readyMsg.backendKind).toBe("webgl2_raw");
  expect(result.readyMsg.fallback?.from).toBe("webgpu");
  expect(result.readyMsg.fallback?.to).toBe("webgl2_raw");
  expect(result.screenshot.width).toBeGreaterThan(0);
  expect(result.screenshot.height).toBeGreaterThan(0);
  expect(result.screenshot.hash).toBe(expectedWhiteHash);
});
