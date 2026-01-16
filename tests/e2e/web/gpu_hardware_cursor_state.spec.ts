import { expect, test } from "@playwright/test";

test("GPU worker: CursorState uploads cursor image from guest memory and screenshots can include/exclude it", async ({
  page,
  browserName,
}) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/blank.html");

  await page.setContent(`
    <style>
      html, body { margin: 0; padding: 0; background: #000; }
      canvas { width: 64px; height: 64px; }
    </style>
    <canvas id="c"></canvas>
    <script type="module">
      import {
        GPU_PROTOCOL_NAME,
        GPU_PROTOCOL_VERSION,
        FRAME_DIRTY,
        FRAME_PRESENTED,
        FRAME_SEQ_INDEX,
        FRAME_STATUS_INDEX,
        isGpuWorkerMessageBase,
      } from "/web/src/ipc/gpu-protocol.ts";
      import {
        FRAMEBUFFER_FORMAT_RGBA8888,
        HEADER_BYTE_LENGTH,
        HEADER_I32_COUNT,
        HEADER_INDEX_FRAME_COUNTER,
        initFramebufferHeader,
      } from "/web/src/display/framebuffer_protocol.ts";
        import { allocateHarnessSharedMemorySegments } from "/web/src/runtime/harness_shared_memory.ts";
        import { createSharedMemoryViews } from "/web/src/runtime/shared_layout.ts";
      import {
        CURSOR_FORMAT_B8G8R8A8,
        publishCursorState,
        wrapCursorState,
      } from "/web/src/ipc/cursor_state.ts";
      import { formatOneLineUtf8 } from "/web/src/text.ts";

      const W = 64;
      const H = 64;
      const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION };

      const MAX_ERROR_BYTES = 512;

      function formatOneLineError(err) {
        const msg = err instanceof Error ? err.message : err;
        return formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error";
      }

      const canvas = /** @type {HTMLCanvasElement} */ (document.getElementById("c"));
      canvas.width = W;
      canvas.height = H;

      function samplePixel(rgba8, width, x, y) {
        const i = (y * width + x) * 4;
        return [rgba8[i + 0], rgba8[i + 1], rgba8[i + 2], rgba8[i + 3]];
      }

      (async () => {
        try {
           const segments = allocateHarnessSharedMemorySegments({
             guestRamBytes: 64 * 1024,
             sharedFramebuffer: new SharedArrayBuffer(8),
             sharedFramebufferOffsetBytes: 0,
             ioIpcBytes: 0,
             vramBytes: 0,
           });
           const views = createSharedMemoryViews(segments);

          // Frame pacing SAB.
          const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
          const frameState = new Int32Array(sharedFrameState);
          Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
          Atomics.store(frameState, FRAME_SEQ_INDEX, 0);

          // Small RGBA8888 framebuffer_protocol surface (AERO header + pixels).
          const strideBytes = W * 4;
          const framebufferSab = new SharedArrayBuffer(HEADER_BYTE_LENGTH + strideBytes * H);
          const header = new Int32Array(framebufferSab, 0, HEADER_I32_COUNT);
          initFramebufferHeader(header, { width: W, height: H, strideBytes, format: FRAMEBUFFER_FORMAT_RGBA8888 });
          const pixels = new Uint8Array(framebufferSab, HEADER_BYTE_LENGTH, strideBytes * H);
          // Solid black background.
          for (let i = 0; i < pixels.length; i += 4) {
            pixels[i + 0] = 0;
            pixels[i + 1] = 0;
            pixels[i + 2] = 0;
            pixels[i + 3] = 255;
          }

          const worker = new Worker("/web/src/workers/gpu.worker.ts", { type: "module" });

          let readyResolve;
          let readyReject;
          const ready = new Promise((resolve, reject) => {
            readyResolve = resolve;
            readyReject = reject;
          });

          let nextRequestId = 1;
          const pendingScreenshots = new Map();

          const failAll = (err) => {
            readyReject(err);
            for (const [, p] of pendingScreenshots) p.reject(err);
            pendingScreenshots.clear();
          };

          worker.addEventListener("message", (event) => {
            const msg = event.data;
            if (!isGpuWorkerMessageBase(msg) || typeof msg.type !== "string") return;
            if (msg.type === "ready") {
              readyResolve(msg);
              return;
            }
            if (msg.type === "error") {
              failAll(new Error("gpu worker error: " + (msg.message ?? "unknown")));
              return;
            }
            if (msg.type === "screenshot") {
              const pending = pendingScreenshots.get(msg.requestId);
              if (!pending) return;
              pendingScreenshots.delete(msg.requestId);
              pending.resolve(msg);
              return;
            }
          });

          worker.addEventListener("error", (event) => {
            failAll((event && event.error) || event);
          });

          // Worker-side shared memory init (provides guest RAM + CursorState descriptor).
          worker.postMessage({
            kind: "init",
            role: "gpu",
            controlSab: segments.control,
            guestMemory: segments.guestMemory,
            scanoutState: segments.scanoutState,
            scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
            cursorState: segments.cursorState,
            cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
            ioIpcSab: segments.ioIpc,
            sharedFramebuffer: segments.sharedFramebuffer,
            sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
            frameStateSab: sharedFrameState,
          });

          const offscreen = canvas.transferControlToOffscreen();
          worker.postMessage(
            {
              ...GPU_MESSAGE_BASE,
              type: "init",
              canvas: offscreen,
              sharedFrameState,
              sharedFramebuffer: framebufferSab,
              sharedFramebufferOffsetBytes: 0,
              options: {
                // Force WebGL2 so the test does not depend on WebGPU availability.
                forceBackend: "webgl2_raw",
                outputWidth: W,
                outputHeight: H,
                dpr: 1,
              },
            },
            [offscreen],
          );

          await ready;

          // Present the background once so the cursor can be overlaid on top of it.
          const nextSeq = (Atomics.load(frameState, FRAME_SEQ_INDEX) + 1) | 0;
          Atomics.store(header, HEADER_INDEX_FRAME_COUNTER, nextSeq);
          Atomics.store(frameState, FRAME_SEQ_INDEX, nextSeq);
          Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });

          // Program a 1x1 BGRA cursor surface in guest RAM.
          const cursorPaddr = 0x1000;
          // BGRA for an opaque red pixel (B=0, G=0, R=255, A=255).
          views.guestU8.set([0, 0, 255, 255], cursorPaddr);

          // Publish cursor state pointing at the guest RAM cursor surface.
          const cursorWords =
            segments.cursorState instanceof SharedArrayBuffer ? wrapCursorState(segments.cursorState, segments.cursorStateOffsetBytes ?? 0) : null;
          if (!cursorWords) throw new Error("cursorState SAB missing");

          const cursorX = 8;
          const cursorY = 8;
          publishCursorState(cursorWords, {
            enable: 1,
            x: cursorX,
            y: cursorY,
            hotX: 0,
            hotY: 0,
            width: 1,
            height: 1,
            pitchBytes: 4,
            format: CURSOR_FORMAT_B8G8R8A8,
            basePaddrLo: cursorPaddr >>> 0,
            basePaddrHi: 0,
          });

          // Tick the worker so it can snapshot CursorState + upload the cursor image.
          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });

          const requestScreenshot = (includeCursor) => {
            const requestId = nextRequestId++;
            const promise = new Promise((resolve, reject) => pendingScreenshots.set(requestId, { resolve, reject }));
            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId, includeCursor: !!includeCursor });
            return promise;
          };

          const shotNoCursor = await requestScreenshot(false);
          const shotWithCursor = await requestScreenshot(true);

          const rgbaNoCursor = new Uint8Array(shotNoCursor.rgba8);
          const rgbaWithCursor = new Uint8Array(shotWithCursor.rgba8);

          const noCursorPx = samplePixel(rgbaNoCursor, shotNoCursor.width, cursorX, cursorY);
          const withCursorPx = samplePixel(rgbaWithCursor, shotWithCursor.width, cursorX, cursorY);

          window.__AERO_CURSOR_STATE_RESULT__ = {
            pass: noCursorPx[0] === 0 && noCursorPx[1] === 0 && noCursorPx[2] === 0 && noCursorPx[3] === 255 &&
              withCursorPx[0] === 255 && withCursorPx[1] === 0 && withCursorPx[2] === 0 && withCursorPx[3] === 255,
            noCursorPx,
            withCursorPx,
            backend: "webgl2_raw",
          };

          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
          worker.terminate();
        } catch (err) {
          window.__AERO_CURSOR_STATE_RESULT__ = { pass: false, error: formatOneLineError(err) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_CURSOR_STATE_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_CURSOR_STATE_RESULT__);
  expect(result.error ?? null).toBeNull();
  expect(result.pass).toBe(true);
  expect(result.noCursorPx).toEqual([0, 0, 0, 255]);
  expect(result.withCursorPx).toEqual([255, 0, 0, 255]);
});
