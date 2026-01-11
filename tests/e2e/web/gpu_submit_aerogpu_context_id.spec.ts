import { expect, test } from "@playwright/test";

test("GPU worker: contextId isolates AeroGPU resource state across submissions", async ({ page }) => {
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

      const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION };

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
              return;
            }
            if (msg.type === "submit_complete") {
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

          const makeCmdStream = (w, h) => {
            const writer = new AerogpuCmdWriter();
            writer.createTexture2d(
              /* textureHandle */ 1,
              AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_SCANOUT,
              AerogpuFormat.R8G8B8A8Unorm,
              w >>> 0,
              h >>> 0,
              /* mipLevels */ 1,
              /* arrayLayers */ 1,
              /* rowPitchBytes */ (w * 4) >>> 0,
              /* backingAllocId */ 0,
              /* backingOffsetBytes */ 0,
            );
            writer.setRenderTargets([1], 0);
            writer.present(0, 0);
            return writer.finish().buffer;
          };

          // Same texture handle but mismatched dimensions. Without per-context state isolation,
          // the second submission would trigger a CREATE_TEXTURE2D rebind mismatch error.
          const cmdStream0 = makeCmdStream(64, 64);
          const cmdStream1 = makeCmdStream(32, 64);

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

          window.__AERO_CONTEXT_ID_RESULT__ = {
            errors,
            presentCounts: [submit0.presentCount ?? null, submit1.presentCount ?? null],
          };

          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
          worker.terminate();
        } catch (err) {
          window.__AERO_CONTEXT_ID_RESULT__ = { error: String(err) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_CONTEXT_ID_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_CONTEXT_ID_RESULT__);

  expect(result.error ?? null).toBeNull();
  expect(result.errors).toEqual([]);
  expect(result.presentCounts).toHaveLength(2);
  expect(result.presentCounts[0]).not.toBeNull();
  expect(result.presentCounts[1]).not.toBeNull();
});
