import { expect, test } from '@playwright/test';

test('GPU worker: submit_aerogpu round-trips and presents deterministic triangle', async ({ page }) => {
  await page.goto('/blank.html');

  await page.setContent(`
    <style>
      html, body { margin: 0; padding: 0; background: #000; }
      canvas { width: 64px; height: 64px; }
    </style>
    <canvas id="c"></canvas>
    <script type="module">
      import { fnv1a32Hex } from "/src/utils/fnv1a.ts";

      const canvas = /** @type {HTMLCanvasElement} */ (document.getElementById("c"));

      const W = 64;
      const H = 64;

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
        const MAGIC = 0x444d4341; // "ACMD"
        const ABI_VERSION = 0x00010000; // 1.0

        const OP_CREATE_TEXTURE2D = 0x101;
        const OP_UPLOAD_RESOURCE = 0x104;
        const OP_SET_RENDER_TARGETS = 0x400;
        const OP_PRESENT = 0x700;

        const AEROGPU_FORMAT_R8G8B8A8_UNORM = 3;
        const AEROGPU_RESOURCE_USAGE_TEXTURE = (1 << 3);
        const AEROGPU_RESOURCE_USAGE_RENDER_TARGET = (1 << 4);
        const AEROGPU_RESOURCE_USAGE_SCANOUT = (1 << 6);

        const texHandle = 1;
        const uploadBytes = textureRgba.byteLength >>> 0;
        const uploadPacketBytes = 32 + uploadBytes;
        const totalBytes = 24 + 56 + 48 + uploadPacketBytes + 16;

        const buf = new ArrayBuffer(totalBytes);
        const dv = new DataView(buf);
        let off = 0;

        // Stream header.
        dv.setUint32(off + 0, MAGIC, true);
        dv.setUint32(off + 4, ABI_VERSION, true);
        dv.setUint32(off + 8, totalBytes, true);
        dv.setUint32(off + 12, 0, true);
        dv.setUint32(off + 16, 0, true);
        dv.setUint32(off + 20, 0, true);
        off += 24;

        // CREATE_TEXTURE2D.
        dv.setUint32(off + 0, OP_CREATE_TEXTURE2D, true);
        dv.setUint32(off + 4, 56, true);
        dv.setUint32(off + 8, texHandle, true);
        dv.setUint32(
          off + 12,
          AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_SCANOUT,
          true,
        );
        dv.setUint32(off + 16, AEROGPU_FORMAT_R8G8B8A8_UNORM, true);
        dv.setUint32(off + 20, w >>> 0, true);
        dv.setUint32(off + 24, h >>> 0, true);
        dv.setUint32(off + 28, 1, true); // mip_levels
        dv.setUint32(off + 32, 1, true); // array_layers
        dv.setUint32(off + 36, (w * 4) >>> 0, true); // row_pitch_bytes
        dv.setUint32(off + 40, 0, true); // backing_alloc_id
        dv.setUint32(off + 44, 0, true); // backing_offset_bytes
        dv.setBigUint64(off + 48, 0n, true); // reserved0
        off += 56;

        // SET_RENDER_TARGETS (color0 = texHandle).
        dv.setUint32(off + 0, OP_SET_RENDER_TARGETS, true);
        dv.setUint32(off + 4, 48, true);
        dv.setUint32(off + 8, 1, true); // color_count
        dv.setUint32(off + 12, 0, true); // depth_stencil
        dv.setUint32(off + 16, texHandle, true);
        for (let i = 1; i < 8; i++) dv.setUint32(off + 16 + i * 4, 0, true);
        off += 48;

        // UPLOAD_RESOURCE (payload = raw RGBA bytes).
        dv.setUint32(off + 0, OP_UPLOAD_RESOURCE, true);
        dv.setUint32(off + 4, uploadPacketBytes, true);
        dv.setUint32(off + 8, texHandle, true);
        dv.setUint32(off + 12, 0, true);
        dv.setBigUint64(off + 16, 0n, true); // offset_bytes
        dv.setBigUint64(off + 24, BigInt(uploadBytes), true); // size_bytes
        new Uint8Array(buf, off + 32, uploadBytes).set(textureRgba);
        off += uploadPacketBytes;

        // PRESENT.
        dv.setUint32(off + 0, OP_PRESENT, true);
        dv.setUint32(off + 4, 16, true);
        dv.setUint32(off + 8, 0, true); // scanout_id
        dv.setUint32(off + 12, 0, true); // flags
        off += 16;

        if (off !== totalBytes) throw new Error("cmd stream size mismatch");
        return buf;
      }

      (async () => {
        try {
          const worker = new Worker("/src/workers/gpu.worker.ts", { type: "module" });

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
            if (!msg || typeof msg !== "object" || typeof msg.type !== "string") return;
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
              type: "submit_aerogpu",
              requestId: submitRequestId,
              signalFence: 1n,
              cmdStream,
            },
            [cmdStream],
          );
          const submit = await submitPromise;

          const screenshotRequestId = nextRequestId++;
          const screenshotPromise = new Promise((resolve, reject) => pending.set(screenshotRequestId, { resolve, reject }));
          worker.postMessage({ type: "screenshot", requestId: screenshotRequestId });
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

          worker.postMessage({ type: "shutdown" });
          worker.terminate();
        } catch (e) {
          window.__AERO_SUBMIT_RESULT__ = { pass: false, error: String(e) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_SUBMIT_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_SUBMIT_RESULT__);
  expect(result.error ?? null).toBeNull();
  expect(result.pass).toBe(true);
});
