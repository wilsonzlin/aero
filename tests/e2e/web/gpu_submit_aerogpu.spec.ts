import { expect, test } from '@playwright/test';

test('GPU worker: submit_aerogpu round-trips and presents deterministic triangle', async ({ page }) => {
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

       function buildCmdStream(textureRgba, w, h) {
         const texHandle = 1;
         const tightRowBytes = (w * 4) >>> 0;
         // Force a non-tight row pitch to exercise row_pitch_bytes repacking in the GPU worker.
         const rowPitchBytes = (tightRowBytes + 16) >>> 0;
        const padded = new Uint8Array(rowPitchBytes * h);
        for (let y = 0; y < h; y++) {
          padded.set(
            textureRgba.subarray(y * tightRowBytes, (y + 1) * tightRowBytes),
            y * rowPitchBytes,
          );
         }
         const writer = new AerogpuCmdWriter();
         const fmt = AerogpuFormat.R8G8B8A8UnormSrgb ?? AerogpuFormat.R8G8B8A8Unorm;
         writer.createTexture2d(
           texHandle,
           AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_SCANOUT,
           fmt,
           w >>> 0,
           h >>> 0,
           1,
           1,
          rowPitchBytes,
          0,
          0,
        );
        writer.setRenderTargets([texHandle], 0);
        writer.uploadResource(texHandle, 0n, padded);
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

          const expected = triangleRgba(W, H);
          const cmdStream = buildCmdStream(expected, W, H);

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
          const submit = await submitPromise;

           const screenshotRequestId = nextRequestId++;
           const screenshotPromise = new Promise((resolve, reject) => pending.set(screenshotRequestId, { resolve, reject }));
           worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId: screenshotRequestId });
           const screenshot = await screenshotPromise;
           const actual = new Uint8Array(screenshot.rgba8);

          const hash = fnv1a32Hex(actual);
          const expectedHash = fnv1a32Hex(expected);

          window.__AERO_SUBMIT_RESULT__ = {
            pass: hash === expectedHash,
            hash,
            expectedHash,
            presentCount: submit.presentCount?.toString?.() ?? null,
            completedFence: submit.completedFence?.toString?.() ?? null,
          };

           worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
           worker.terminate();
         } catch (e) {
           window.__AERO_SUBMIT_RESULT__ = { pass: false, error: formatOneLineError(e) };
         }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_SUBMIT_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_SUBMIT_RESULT__);
  expect(result.error ?? null).toBeNull();
  expect(result.pass).toBe(true);
});
