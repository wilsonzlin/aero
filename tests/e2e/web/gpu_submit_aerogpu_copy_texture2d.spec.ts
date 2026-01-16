import { expect, test } from '@playwright/test';

test('GPU worker: submit_aerogpu COPY_TEXTURE2D copies sub-rects (with BGRX upload conversion)', async ({ page }) => {
  await page.goto('/web/blank.html');

  await page.setContent(`
    <style>
      html, body { margin: 0; padding: 0; background: #000; }
      canvas { width: 64px; height: 64px; }
    </style>
     <canvas id="c"></canvas>
     <script type="module">
        import { fnv1a32Hex } from "/web/src/utils/fnv1a.ts";
        import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION, isGpuWorkerMessageBase } from "/web/src/ipc/gpu-protocol.ts";
        import {
          AerogpuCmdWriter,
          AEROGPU_COPY_FLAG_NONE,
          AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
          AEROGPU_RESOURCE_USAGE_SCANOUT,
          AEROGPU_RESOURCE_USAGE_TEXTURE,
        } from "/emulator/protocol/aerogpu/aerogpu_cmd.ts";
        import { AerogpuFormat } from "/emulator/protocol/aerogpu/aerogpu_pci.ts";
        import { formatOneLineUtf8 } from "/web/src/text.ts";
  
        const canvas = /** @type {HTMLCanvasElement} */ (document.getElementById("c"));
  
        const W = 64;
        const H = 64;
        const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION };

      const MAX_ERROR_BYTES = 512;
      function formatOneLineError(err) {
        const msg = err instanceof Error ? err.message : err;
        return formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error";
      }

        function triangleRgba(w, h) {
          const out = new Uint8Array(w * h * 4);
          for (let y = 0; y < h; y++) {
            for (let x = 0; x < w; x++) {
              const i = (y * w + x) * 4;
              const inside = (x + y) < w;
              out[i + 0] = inside ? 255 : 0;
              out[i + 1] = 0;
              out[i + 2] = 0;
              out[i + 3] = 255;
            }
          }
          return out;
        }

        function rgbaToBgrx(rgba) {
          if ((rgba.length % 4) !== 0) throw new Error("rgbaToBgrx expects 4-byte aligned input");
          const out = new Uint8Array(rgba.length);
          for (let i = 0; i < rgba.length; i += 4) {
            out[i + 0] = rgba[i + 2]; // B
            out[i + 1] = rgba[i + 1]; // G
            out[i + 2] = rgba[i + 0]; // R
            out[i + 3] = 0; // X (ignored by the protocol)
          }
          return out;
        }

        function blackRgba(w, h) {
          const out = new Uint8Array(w * h * 4);
          for (let i = 3; i < out.length; i += 4) out[i] = 255;
          return out;
        }

        function copyRectRgba(dst, dstW, src, srcW, dstX, dstY, srcX, srcY, w, h) {
          const rowBytes = w * 4;
          for (let row = 0; row < h; row++) {
            const srcOff = ((srcY + row) * srcW + srcX) * 4;
            const dstOff = ((dstY + row) * dstW + dstX) * 4;
            dst.set(src.subarray(srcOff, srcOff + rowBytes), dstOff);
          }
        }

        function buildCmdStream(srcBgrx, w, h) {
          const srcHandle = 1;
          const dstHandle = 2;
          const writer = new AerogpuCmdWriter();

          // Use BGRX to exercise format conversion in UPLOAD_RESOURCE. The browser executor stores
          // textures internally as tight RGBA for presentation.
          const fmt = AerogpuFormat.B8G8R8X8UnormSrgb ?? AerogpuFormat.B8G8R8X8Unorm;
          const usage = AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_SCANOUT;

          writer.createTexture2d(srcHandle, usage, fmt, w >>> 0, h >>> 0, 1, 1, (w * 4) >>> 0, 0, 0);
          writer.createTexture2d(dstHandle, usage, fmt, w >>> 0, h >>> 0, 1, 1, (w * 4) >>> 0, 0, 0);

          writer.uploadResource(srcHandle, 0n, srcBgrx);

          const srcX = 0;
          const srcY = 0;
          const dstX = 16;
          const dstY = 16;
          const copyW = 32;
          const copyH = 32;
          writer.copyTexture2d(dstHandle, srcHandle, 0, 0, 0, 0, dstX, dstY, srcX, srcY, copyW, copyH, AEROGPU_COPY_FLAG_NONE);

          writer.setRenderTargets([dstHandle], 0);
          writer.present(0, 0);
          return writer.finish().buffer;
        }

        (async () => {
          try {
            const worker = new Worker("/web/src/workers/gpu.worker.ts", { type: "module" });

            let readyResolve;
            let readyReject;
            const ready = new Promise((resolve, reject) => {
              readyResolve = resolve;
              readyReject = reject;
            });

            let nextRequestId = 1;
            const pending = new Map();

            const onMessage = (event) => {
              const msg = event.data;
              if (!isGpuWorkerMessageBase(msg) || typeof msg.type !== "string") return;
              if (msg.type === "ready") {
                readyResolve(msg);
                return;
              }
              if (msg.type === "error") {
                // Treat any worker error as fatal for this test.
                readyReject(new Error("gpu worker error: " + msg.message));
                for (const [, v] of pending) v.reject(new Error("gpu worker error: " + msg.message));
                pending.clear();
                return;
              }
              if (msg.type === "submit_complete" || msg.type === "screenshot") {
                const entry = pending.get(msg.requestId);
                if (!entry) return;
                pending.delete(msg.requestId);
                entry.resolve(msg);
                return;
              }
            };
            worker.addEventListener("message", onMessage);
            worker.addEventListener("error", (event) => {
              readyReject((event && event.error) || event);
            });

            const offscreen = canvas.transferControlToOffscreen();
            // Simple framebuffer_protocol (AERO) layout: 8 i32 header + RGBA bytes.
            const strideBytes = W * 4;
            const headerBytes = 8 * 4;
            const sharedFramebuffer = new SharedArrayBuffer(headerBytes + strideBytes * H);
            const header = new Int32Array(sharedFramebuffer, 0, 8);
            // Header fields from src/display/framebuffer_protocol.ts (inlined).
            header[0] = 0x4f524541; // FRAMEBUFFER_MAGIC ("AERO")
            header[1] = 1; // FRAMEBUFFER_VERSION
            header[2] = W;
            header[3] = H;
            header[4] = strideBytes;
            header[5] = 1; // FRAMEBUFFER_FORMAT_RGBA8888
            header[6] = 0; // frame counter
            header[7] = 1; // config counter

            const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
            const frameState = new Int32Array(sharedFrameState);
            frameState[0] = 0; // FRAME_PRESENTED
            frameState[1] = 0; // seq

            worker.postMessage(
              {
                ...GPU_MESSAGE_BASE,
                type: "init",
                canvas: offscreen,
                sharedFrameState,
                sharedFramebuffer,
                sharedFramebufferOffsetBytes: 0,
                options: {
                  forceBackend: "webgl2_raw",
                  outputWidth: W,
                  outputHeight: H,
                  dpr: 1,
                },
              },
              [offscreen],
            );

            await ready;

            const srcRgba = triangleRgba(W, H);
            const srcBgrx = rgbaToBgrx(srcRgba);

            const expected = blackRgba(W, H);
            copyRectRgba(expected, W, srcRgba, W, 16, 16, 0, 0, 32, 32);

            const cmdStream = buildCmdStream(srcBgrx, W, H);

            const submitRequestId = nextRequestId++;
            const submitPromise = new Promise((resolve, reject) => pending.set(submitRequestId, { resolve, reject }));
            worker.postMessage(
              {
                ...GPU_MESSAGE_BASE,
                type: "submit_aerogpu",
                requestId: submitRequestId,
                contextId: 0,
                signalFence: 1n,
                cmdStream,
              },
              [cmdStream],
            );
            await submitPromise;

            const screenshotRequestId = nextRequestId++;
            const screenshotPromise = new Promise((resolve, reject) => pending.set(screenshotRequestId, { resolve, reject }));
            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId: screenshotRequestId });
            const screenshot = await screenshotPromise;
            const actual = new Uint8Array(screenshot.rgba8);

            const hash = fnv1a32Hex(actual);
            const expectedHash = fnv1a32Hex(expected);

            window.__AERO_COPY_RESULT__ = {
              pass: hash === expectedHash,
              hash,
              expectedHash,
            };

            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
            worker.terminate();
          } catch (e) {
            window.__AERO_COPY_RESULT__ = { pass: false, error: formatOneLineError(e) };
          }
        })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_COPY_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_COPY_RESULT__);
  expect(result.error ?? null).toBeNull();
  expect(result.pass).toBe(true);
});
