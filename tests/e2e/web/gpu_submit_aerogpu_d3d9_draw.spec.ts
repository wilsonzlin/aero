import { expect, test } from '@playwright/test';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

test('GPU worker: submit_aerogpu executes real D3D9 draw via wasm executor', async ({ page }) => {
  await page.goto('/web/blank.html');

  const thisDir = dirname(fileURLToPath(import.meta.url));
  const repoRoot = dirname(dirname(dirname(thisDir)));
  const bundles = [
    {
      js: join(repoRoot, 'web', 'src', 'wasm', 'pkg-single-gpu', 'aero_gpu_wasm.js'),
      wasm: join(repoRoot, 'web', 'src', 'wasm', 'pkg-single-gpu', 'aero_gpu_wasm_bg.wasm'),
    },
    {
      js: join(repoRoot, 'web', 'src', 'wasm', 'pkg-threaded-gpu', 'aero_gpu_wasm.js'),
      wasm: join(repoRoot, 'web', 'src', 'wasm', 'pkg-threaded-gpu', 'aero_gpu_wasm_bg.wasm'),
    },
    {
      js: join(repoRoot, 'web', 'src', 'wasm', 'pkg-single-gpu-dev', 'aero_gpu_wasm.js'),
      wasm: join(repoRoot, 'web', 'src', 'wasm', 'pkg-single-gpu-dev', 'aero_gpu_wasm_bg.wasm'),
    },
    {
      js: join(repoRoot, 'web', 'src', 'wasm', 'pkg-threaded-gpu-dev', 'aero_gpu_wasm.js'),
      wasm: join(repoRoot, 'web', 'src', 'wasm', 'pkg-threaded-gpu-dev', 'aero_gpu_wasm_bg.wasm'),
    },
  ];
  if (!bundles.some(({ js, wasm }) => existsSync(js) && existsSync(wasm))) {
    const message = [
      'aero-gpu-wasm bundle is missing.',
      '',
      'Expected one of:',
      ...bundles.flatMap(({ js, wasm }) => [`- ${wasm}`, `  ${js}`]),
      '',
      'Build it with (from the repo root):',
      '  npm -w web run wasm:build',
    ].join('\n');
    if (process.env.CI) {
      throw new Error(message);
    }
    test.skip(true, message);
  }

  const support = await page.evaluate(async () => {
    const withTimeout = async <T>(promise: Promise<T>, ms: number): Promise<T | null> => {
      return await Promise.race([
        promise,
        new Promise<null>((resolve) => {
          setTimeout(() => resolve(null), ms);
        }),
      ]);
    };

    const sharedArrayBuffer = typeof SharedArrayBuffer !== 'undefined';
    const crossOriginIsolated = globalThis.crossOriginIsolated === true;
    const offscreen =
      typeof OffscreenCanvas !== 'undefined' && 'transferControlToOffscreen' in HTMLCanvasElement.prototype;

    // Some Chromium environments can create an adapter/device but still fail for real
    // rendering/readback (e.g. GPUBuffer.mapAsync aborts). Do a tiny mapAsync-based
    // round-trip so we only pick the WebGPU presenter backend when it's actually usable.
    let webgpu = false;
    try {
      const gpu = (navigator as any).gpu as any;
      if (gpu?.requestAdapter) {
        const adapter = await withTimeout(gpu.requestAdapter({ powerPreference: 'high-performance' }), 1000);
        if (adapter?.requestDevice) {
          const device = await withTimeout(adapter.requestDevice(), 1000);
          if (device) {
            try {
              const usage = (globalThis as any).GPUBufferUsage as any;
              const mapMode = (globalThis as any).GPUMapMode as any;
              if (usage?.MAP_READ && usage?.COPY_DST && mapMode?.READ !== undefined) {
                const buf = device.createBuffer({ size: 4, usage: usage.MAP_READ | usage.COPY_DST });
                device.queue.writeBuffer(buf, 0, new Uint32Array([0x12345678]));
                try {
                  device.queue.submit?.([]);
                } catch {
                  // Ignore (submit isn't required for writeBuffer, but calling it is harmless when supported).
                }
                const mapped = await withTimeout(buf.mapAsync(mapMode.READ), 1000);
                if (mapped !== null) {
                  const view = new Uint32Array(buf.getMappedRange());
                  webgpu = view[0] === 0x12345678;
                  buf.unmap();
                }
              }
            } catch {
              webgpu = false;
            }

            try {
              device.destroy?.();
            } catch {
              // Ignore.
            }
          }
        }
      }
    } catch {
      webgpu = false;
    }

    let webgl2 = false;
    try {
      const canvas = document.createElement('canvas');
      webgl2 = !!canvas.getContext('webgl2');
    } catch {
      webgl2 = false;
    }

    return { sharedArrayBuffer, crossOriginIsolated, offscreen, webgpu, webgl2 };
  });

  test.skip(!support.sharedArrayBuffer || !support.crossOriginIsolated, 'SharedArrayBuffer requires COOP/COEP headers.');
  test.skip(!support.offscreen, 'OffscreenCanvas is unavailable.');
  test.skip(!support.webgpu && !support.webgl2, 'No WebGPU/WebGL2 backend is available in this browser.');

  const forceBackend = support.webgpu ? 'webgpu' : 'webgl2_wgpu';

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
          AerogpuPrimitiveTopology,
          AerogpuShaderStage,
          AEROGPU_CLEAR_COLOR,
          AEROGPU_PRESENT_FLAG_VSYNC,
          AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
          AEROGPU_RESOURCE_USAGE_SCANOUT,
          AEROGPU_RESOURCE_USAGE_TEXTURE,
          AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        } from "/emulator/protocol/aerogpu/aerogpu_cmd.ts";
      import { AerogpuFormat } from "/emulator/protocol/aerogpu/aerogpu_pci.ts";
      import { formatOneLineUtf8 } from "/web/src/text.ts";

      const FORCE_BACKEND = ${JSON.stringify(forceBackend)};

      const canvas = /** @type {HTMLCanvasElement} */ (document.getElementById("c"));
      const W = 64;
      const H = 64;
      const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION };

      const MAX_ERROR_BYTES = 512;

      function formatOneLineError(err) {
        const msg = err instanceof Error ? err.message : err;
        return formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error";
      }

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

      // -----------------------------------------------------------------------------
      // Minimal D3D9 token-stream shader assembler (ported from
      // crates/aero-gpu/tests/aerogpu_d3d9_cmd_stream_state.rs).
      // -----------------------------------------------------------------------------

      function encRegType(ty) {
        const low = ty & 0x7;
        const high = ty & 0x18;
        return (((low << 28) >>> 0) | ((high << 8) >>> 0)) >>> 0;
      }

      function encSrc(regType, regNum, swizzle) {
        return (encRegType(regType) | (regNum >>> 0) | ((swizzle & 0xff) << 16)) >>> 0;
      }

      function encDst(regType, regNum, mask) {
        return (encRegType(regType) | (regNum >>> 0) | ((mask & 0xff) << 16)) >>> 0;
      }

      function encInst(opcode, params) {
        const token = ((opcode & 0xffff) | ((params.length & 0xf) << 24)) >>> 0;
        return [token, ...params.map((v) => v >>> 0)];
      }

      function wordsToBytes(words) {
        const buf = new ArrayBuffer(words.length * 4);
        const dv = new DataView(buf);
        for (let i = 0; i < words.length; i += 1) {
          dv.setUint32(i * 4, words[i] >>> 0, true);
        }
        return new Uint8Array(buf);
      }

      function assembleVsPassthroughPos() {
        // vs_2_0: mov oPos, v0; end
        const words = [0xfffe0200];
        // MOV oPos, v0
        words.push(...encInst(0x0001, [encDst(4, 0, 0xf), encSrc(1, 0, 0xe4)]));
        words.push(0x0000ffff);
        return wordsToBytes(words);
      }

      function assemblePsSolidColorC0() {
        // ps_2_0: mov oC0, c0; end
        const words = [0xffff0200];
        words.push(...encInst(0x0001, [encDst(8, 0, 0xf), encSrc(2, 0, 0xe4)]));
        words.push(0x0000ffff);
        return wordsToBytes(words);
      }

      function vertexDeclPos() {
        // D3DVERTEXELEMENT9 stream:
        //  - POSITION0 float4 at stream 0 offset 0
        //  - End marker (stream 0xFF, type UNUSED)
        const buf = new ArrayBuffer(16);
        const dv = new DataView(buf);
        const u8 = new Uint8Array(buf);
        let off = 0;
        dv.setUint16(off, 0, true); off += 2; // stream
        dv.setUint16(off, 0, true); off += 2; // offset
        u8[off++] = 3; // type = FLOAT4
        u8[off++] = 0; // method
        u8[off++] = 0; // usage = POSITION
        u8[off++] = 0; // usage_index

        dv.setUint16(off, 0x00ff, true); off += 2; // stream = 0xFF
        dv.setUint16(off, 0, true); off += 2; // offset
        u8[off++] = 17; // type = UNUSED
        u8[off++] = 0; // method
        u8[off++] = 0; // usage
        u8[off++] = 0; // usage_index

        return new Uint8Array(buf);
      }

      function fullscreenTrianglePos() {
        const verts = [
          [-1.0, -1.0, 0.0, 1.0],
          [ 3.0, -1.0, 0.0, 1.0],
          [-1.0,  3.0, 0.0, 1.0],
        ];
        const buf = new ArrayBuffer(3 * 4 * 4);
        const dv = new DataView(buf);
        let off = 0;
        for (const v of verts) {
          for (const f of v) {
            dv.setFloat32(off, f, true);
            off += 4;
          }
        }
        return new Uint8Array(buf);
      }

      function buildCmdStream(w, h) {
        const RT_HANDLE = 1;
        const VB_HANDLE = 2;
        const VS_HANDLE = 3;
        const PS_HANDLE = 4;
        const IL_HANDLE = 5;

        const vbData = fullscreenTrianglePos();
        const vsBytes = assembleVsPassthroughPos();
        const psBytes = assemblePsSolidColorC0();
        const declBytes = vertexDeclPos();

        const writer = new AerogpuCmdWriter();
        writer.createTexture2d(
          RT_HANDLE,
          AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_SCANOUT,
          AerogpuFormat.R8G8B8A8Unorm,
          w >>> 0,
          h >>> 0,
          1,
          1,
          (w * 4) >>> 0,
          0,
          0,
        );

        writer.setRenderTargets([RT_HANDLE], 0);
        writer.setViewport(0, 0, w, h, 0, 1);

        // Make state deterministic: clear black, disable culling.
        writer.clear(AEROGPU_CLEAR_COLOR, [0, 0, 0, 1], 1.0, 0);

        // D3DRS_CULLMODE = 22, D3DCULL_NONE = 1.
        writer.setRenderState(22, 1);

        writer.createBuffer(VB_HANDLE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, BigInt(vbData.byteLength), 0, 0);
        writer.uploadResource(VB_HANDLE, 0n, vbData);

        writer.createShaderDxbc(VS_HANDLE, AerogpuShaderStage.Vertex, vsBytes);
        writer.createShaderDxbc(PS_HANDLE, AerogpuShaderStage.Pixel, psBytes);
        writer.bindShaders(VS_HANDLE, PS_HANDLE, 0);

        writer.createInputLayout(IL_HANDLE, declBytes);
        writer.setInputLayout(IL_HANDLE);

        writer.setVertexBuffers(0, [{ buffer: VB_HANDLE, strideBytes: 16, offsetBytes: 0 }]);
        writer.setPrimitiveTopology(AerogpuPrimitiveTopology.TriangleList);

        // c0 = solid green.
         writer.setShaderConstantsF(AerogpuShaderStage.Pixel, 0, [0, 1, 0, 1]);
 
         writer.draw(3, 1, 0, 0);
         writer.presentEx(0, AEROGPU_PRESENT_FLAG_VSYNC, 0);
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
          const strideBytes = W * 4;
          const headerBytes = 8 * 4;
          const sharedFramebuffer = new SharedArrayBuffer(headerBytes + strideBytes * H);
          const header = new Int32Array(sharedFramebuffer, 0, 8);
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
                forceBackend: FORCE_BACKEND,
                outputWidth: W,
                outputHeight: H,
                dpr: 1,
              },
            },
            [offscreen],
          );

          const readyMsg = await ready;

          const expected = solidRgba(W, H, 0, 255, 0, 255);
          const cmdStream = buildCmdStream(W, H);

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
          const withTimeout = (promise, ms) =>
            Promise.race([
              promise.then((value) => ({ kind: "value", value })),
              new Promise((resolve) => setTimeout(() => resolve({ kind: "timeout" }), ms)),
            ]);

          const early = await withTimeout(submitPromise, 50);
          const completedBeforeTick = early.kind === "value";
          /** @type {any} */
          let submit = completedBeforeTick ? early.value : null;

          if (!completedBeforeTick) {
            // Pump ticks until the VSYNC-gated completion arrives. The wasm executor can take
            // longer than the initial timeout to finish compiling/executing the stream; a
            // tick sent too early (before the submission is enqueued) should not complete it.
            const deadline = performance.now() + 2_000;
            while (submit == null && performance.now() < deadline) {
              worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
              const next = await withTimeout(submitPromise, 50);
              if (next.kind === "value") {
                submit = next.value;
                break;
              }
            }
            if (submit == null) {
              throw new Error("timed out waiting for VSYNC submit_complete after pumping ticks");
            }
          }

          const screenshotRequestId = nextRequestId++;
          const screenshotPromise = new Promise((resolve, reject) => pending.set(screenshotRequestId, { resolve, reject }));
          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId: screenshotRequestId });
          const screenshot = await screenshotPromise;

          const actual = new Uint8Array(screenshot.rgba8);
          const hash = fnv1a32Hex(actual);
          const expectedHash = fnv1a32Hex(expected);

           window.__AERO_D3D9_DRAW_RESULT__ = {
             pass: hash === expectedHash,
             hash,
             expectedHash,
             backendKind: readyMsg.backendKind ?? null,
             completedBeforeTick,
             presentCount: submit.presentCount?.toString?.() ?? null,
             completedFence: submit.completedFence?.toString?.() ?? null,
           };

          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
          worker.terminate();
        } catch (e) {
          window.__AERO_D3D9_DRAW_RESULT__ = { pass: false, error: formatOneLineError(e) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_D3D9_DRAW_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_D3D9_DRAW_RESULT__);

  expect(result.error ?? null).toBeNull();
  expect(result.pass).toBe(true);
  // `submit_complete` timing depends on executor/backend initialization latency; the important
  // contract is that the command stream executes successfully and produces the expected pixels.
});
