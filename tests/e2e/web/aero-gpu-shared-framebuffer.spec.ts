import { expect, test } from "@playwright/test";

test("gpu worker can present shared-layout framebuffer and screenshot matches", async ({ page }) => {
  await page.goto("/web/blank.html");

  const width = 64;
  const height = 64;

  // Expected hash for the RGBA pattern below (SHA-256 over raw bytes).
  const expectedHash = "0ede29c88978d2dfc76557e5b7c8d2114aaf78e2278aa5c1348da7726f8fdd1f";

  const result = await page.evaluate(
    async ({ width, height, expectedHash }) => {
      const shared = await import("/web/src/ipc/shared-layout.ts");
      const fp = await import("/web/src/shared/frameProtocol.ts");

      const tileSize = 32;
      const strideBytes = width * 4;
      const layout = shared.computeSharedFramebufferLayout(width, height, strideBytes, shared.FramebufferFormat.RGBA8, tileSize);

      const sharedFramebuffer = new SharedArrayBuffer(layout.totalBytes);
      const header = new Int32Array(sharedFramebuffer, 0, shared.SHARED_FRAMEBUFFER_HEADER_U32_LEN);

      Atomics.store(header, shared.SharedFramebufferHeaderIndex.MAGIC, shared.SHARED_FRAMEBUFFER_MAGIC);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.VERSION, shared.SHARED_FRAMEBUFFER_VERSION);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.WIDTH, width);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.HEIGHT, height);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.STRIDE_BYTES, strideBytes);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.FORMAT, shared.FramebufferFormat.RGBA8);
      // Start with slot 1 active so slot 0 is treated as the back buffer for the first publish.
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.ACTIVE_INDEX, 1);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.TILE_SIZE, tileSize);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.TILES_X, layout.tilesX);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.TILES_Y, layout.tilesY);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, layout.dirtyWordsPerBuffer);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.FLAGS, 0);

      const slot0 = new Uint8Array(sharedFramebuffer, layout.framebufferOffsets[0], layout.strideBytes * layout.height);

      const frame = new Uint8Array(width * height * 4);
      for (let y = 0; y < height; y++) {
        for (let x = 0; x < width; x++) {
          const i = (y * width + x) * 4;
          frame[i + 0] = x & 0xff;
          frame[i + 1] = y & 0xff;
          frame[i + 2] = (x ^ y) & 0xff;
          frame[i + 3] = 0xff;
        }
      }

      const frameDigest = await crypto.subtle.digest("SHA-256", frame);
      const frameHash = Array.from(new Uint8Array(frameDigest))
        .map((b) => b.toString(16).padStart(2, "0"))
        .join("");
      if (frameHash !== expectedHash) {
        throw new Error(`Unexpected test pattern hash: got ${frameHash} expected ${expectedHash}`);
      }

      slot0.set(frame);

      if (layout.dirtyWordsPerBuffer > 0) {
        const dirty0 = new Uint32Array(sharedFramebuffer, layout.dirtyOffsets[0], layout.dirtyWordsPerBuffer);
        dirty0.fill(0xffffffff);
      }

      const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
      const frameState = new Int32Array(sharedFrameState);
      Atomics.store(frameState, fp.FRAME_STATUS_INDEX, fp.FRAME_PRESENTED);
      Atomics.store(frameState, fp.FRAME_SEQ_INDEX, 0);

      const canvas = document.createElement("canvas");
      document.body.appendChild(canvas);
      const offscreen = canvas.transferControlToOffscreen();

      const worker = new Worker("/web/src/workers/gpu.worker.ts", { type: "module" });

      const ready = new Promise<void>((resolve, reject) => {
        worker.addEventListener("message", (ev: MessageEvent) => {
          if (ev.data?.type === "ready") resolve();
          if (ev.data?.type === "error") reject(new Error(ev.data?.message ?? "gpu worker error"));
        });
      });

      worker.postMessage(
        {
          type: "init",
          canvas: offscreen,
          sharedFrameState,
          sharedFramebuffer,
          sharedFramebufferOffsetBytes: 0,
          options: {
            forceBackend: "webgl2_raw",
            disableWebGpu: true,
            outputWidth: width,
            outputHeight: height,
            dpr: 1,
          },
        },
        [offscreen],
      );

      await ready;

      // Publish slot 0 as the active buffer (seq 1).
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 1);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.FRAME_SEQ, 1);
      Atomics.store(header, shared.SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
      Atomics.notify(header, shared.SharedFramebufferHeaderIndex.FRAME_SEQ);

      Atomics.store(frameState, fp.FRAME_SEQ_INDEX, 1);
      Atomics.store(frameState, fp.FRAME_STATUS_INDEX, fp.FRAME_DIRTY);
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

      const frameDirty = Atomics.load(header, shared.SharedFramebufferHeaderIndex.FRAME_DIRTY);

      worker.postMessage({ type: "shutdown" });
      worker.terminate();

      return { width: screenshot.width, height: screenshot.height, hash, frameDirty };
    },
    { width, height, expectedHash },
  );

  expect(result.width).toBe(width);
  expect(result.height).toBe(height);
  expect(result.hash).toBe(expectedHash);
  expect(result.frameDirty).toBe(0);
});
