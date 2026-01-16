import { expect, test } from "@playwright/test";

test("GPU worker: contextId isolates AeroGPU per-context state across submissions", async ({ page }) => {
  await page.goto("/web/blank.html");

  await page.setContent(`
    <script type="module">
      import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION, isGpuWorkerMessageBase } from "/web/src/ipc/gpu-protocol.ts";
      import {
        AerogpuCmdWriter,
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AEROGPU_RESOURCE_USAGE_SCANOUT,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
      } from "/emulator/protocol/aerogpu/aerogpu_cmd.ts";
      import { AerogpuFormat } from "/emulator/protocol/aerogpu/aerogpu_pci.ts";
      import { formatOneLineUtf8 } from "/web/src/text.ts";

      const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION };
      const MAX_ERROR_BYTES = 512;

      function formatOneLineError(err) {
        const msg = err instanceof Error ? err.message : err;
        return formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error";
      }
      const H = 64;
      const fmt = AerogpuFormat.R8G8B8A8UnormSrgb ?? AerogpuFormat.R8G8B8A8Unorm;

      (async () => {
        try {
          const worker = new Worker("/web/src/workers/gpu.worker.ts", { type: "module" });

          let readyResolve;
          let readyReject;
          const ready = new Promise((resolve, reject) => {
            readyResolve = resolve;
            readyReject = reject;
          });

          /** @type {Map<number, { resolve: (value: any) => void, reject: (err: any) => void }>} */
          const pending = new Map();
          /** @type {string[]} */
          const errors = [];
          let nextRequestId = 1;

          const onMessage = (event) => {
            const msg = event.data;
            if (!isGpuWorkerMessageBase(msg) || typeof msg.type !== "string") return;
            if (msg.type === "ready") {
              readyResolve(msg);
              return;
            }
            if (msg.type === "error") {
              errors.push(String(msg.message ?? ""));
              // Prevent hangs if the worker reports an error mid-flight.
              readyReject(new Error("gpu worker error: " + String(msg.message ?? "")));
              for (const [, v] of pending) v.reject(new Error("gpu worker error: " + String(msg.message ?? "")));
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

          const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
          const sharedFramebuffer = new SharedArrayBuffer(8);
          worker.postMessage({
            ...GPU_MESSAGE_BASE,
            type: "init",
            sharedFrameState,
            sharedFramebuffer,
            sharedFramebufferOffsetBytes: 0,
          });

          await ready;

          function solidRgba(w, h, r, g, b, a) {
            const out = new Uint8Array(w * h * 4);
            for (let i = 0; i < out.length; i += 4) {
              out[i + 0] = r;
              out[i + 1] = g;
              out[i + 2] = b;
              out[i + 3] = a;
            }
            return out;
          }

          const makeCreateAndPresentCmdStream = (handle, w, h, rgba) => {
            const writer = new AerogpuCmdWriter();
            writer.createTexture2d(
              /* textureHandle */ handle,
              AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_SCANOUT,
              fmt,
              w >>> 0,
              h >>> 0,
              /* mipLevels */ 1,
              /* arrayLayers */ 1,
              /* rowPitchBytes */ (w * 4) >>> 0,
              /* backingAllocId */ 0,
              /* backingOffsetBytes */ 0,
            );
            writer.setRenderTargets([handle], 0);
            writer.uploadResource(handle, 0n, rgba);
            writer.present(0, 0);
            return writer.finish().buffer;
          };

          const makePresentOnlyCmdStream = () => {
            const writer = new AerogpuCmdWriter();
            writer.present(0, 0);
            return writer.finish().buffer;
          };

          // Each context creates and binds its own render target. The second submission overwrites
          // global state if contextId isolation is broken. The final present should still render
          // the context 0 target.
          const W0 = 64;
          const W1 = 32;
          const cmdStream0 = makeCreateAndPresentCmdStream(1, W0, H, solidRgba(W0, H, 255, 0, 0, 255));
          const cmdStream1 = makeCreateAndPresentCmdStream(2, W1, H, solidRgba(W1, H, 0, 255, 0, 255));
          const cmdStream2 = makePresentOnlyCmdStream();

          const submit = (contextId, signalFence, cmdStream) => {
            const requestId = nextRequestId++;
            const p = new Promise((resolve, reject) => pending.set(requestId, { resolve, reject }));
            worker.postMessage(
              {
                ...GPU_MESSAGE_BASE,
                type: "submit_aerogpu",
                requestId,
                contextId,
                signalFence,
                cmdStream,
              },
              [cmdStream],
            );
            return p;
          };

          const submit0 = await submit(0, 1n, cmdStream0);
          const submit1 = await submit(1, 2n, cmdStream1);
          const submit2 = await submit(0, 3n, cmdStream2);

          const screenshotRequestId = nextRequestId++;
          const screenshotPromise = new Promise((resolve, reject) => pending.set(screenshotRequestId, { resolve, reject }));
          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId: screenshotRequestId });
          const screenshot = await screenshotPromise;
          const pixels = new Uint8Array(screenshot.rgba8);

          window.__AERO_CONTEXT_ID_RESULT__ = {
            errors,
            presentCounts: [submit0.presentCount ?? null, submit1.presentCount ?? null, submit2.presentCount ?? null],
            screenshot: {
              width: screenshot.width,
              height: screenshot.height,
              firstPixel: [pixels[0], pixels[1], pixels[2], pixels[3]],
            },
          };

          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
          worker.terminate();
        } catch (err) {
          window.__AERO_CONTEXT_ID_RESULT__ = { error: formatOneLineError(err) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_CONTEXT_ID_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_CONTEXT_ID_RESULT__);

  expect(result.error ?? null).toBeNull();
  expect(result.errors).toEqual([]);
  // presentCount is a monotonic counter for the runtime (not per-context).
  expect(result.presentCounts).toEqual([1n, 2n, 3n]);
  expect(result.screenshot).toEqual({ width: 64, height: 64, firstPixel: [255, 0, 0, 255] });
});
