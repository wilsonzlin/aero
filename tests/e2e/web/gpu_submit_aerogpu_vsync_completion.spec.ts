import { expect, test } from "@playwright/test";

test("GPU worker: submit_aerogpu with VSYNC present completes without requiring a tick", async ({ page }) => {
  await page.goto("/web/blank.html");

  await page.setContent(`
     <script type="module">
       import {
        GPU_PROTOCOL_NAME,
        GPU_PROTOCOL_VERSION,
        FRAME_DIRTY,
        FRAME_STATUS_INDEX,
        isGpuWorkerMessageBase,
      } from "/web/src/ipc/gpu-protocol.ts";
      import { AerogpuCmdWriter, AEROGPU_PRESENT_FLAG_VSYNC } from "/emulator/protocol/aerogpu/aerogpu_cmd.ts";

       const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION };

       const withTimeout = (promise, ms) =>
         Promise.race([
           promise.then((value) => ({ kind: "value", value })),
           new Promise((resolve) => setTimeout(() => resolve({ kind: "timeout" }), ms)),
         ]);

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
          /** @type {any[]} */
          const completions = [];
          let nextRequestId = 1;

          const onMessage = (event) => {
            const msg = event.data;
            if (!isGpuWorkerMessageBase(msg) || typeof msg.type !== "string") return;

            if (msg.type === "ready") {
              readyResolve(msg);
              return;
            }
            if (msg.type === "error") {
              readyReject(new Error("gpu worker error: " + msg.message));
              for (const [, v] of pending) v.reject(new Error("gpu worker error: " + msg.message));
              pending.clear();
              return;
            }
            if (msg.type === "submit_complete") {
              completions.push(msg);
              const entry = pending.get(msg.requestId);
              if (!entry) return;
              pending.delete(msg.requestId);
              entry.resolve(msg);
              return;
            }
            if (msg.type === "screenshot") {
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
          const frameState = new Int32Array(sharedFrameState);
          const sharedFramebuffer = new SharedArrayBuffer(8);

          worker.postMessage({
            ...GPU_MESSAGE_BASE,
            type: "init",
            sharedFrameState,
            sharedFramebuffer,
            sharedFramebufferOffsetBytes: 0,
          });

           await ready;

           const writerVsync = new AerogpuCmdWriter();
           // Use PRESENT_EX so the test covers VSYNC flag scanning for both PRESENT and PRESENT_EX packets.
           writerVsync.presentEx(0, AEROGPU_PRESENT_FLAG_VSYNC, 0);
           const cmdStreamVsync = writerVsync.finish().buffer;

          const writerImmediate = new AerogpuCmdWriter();
          writerImmediate.present(0, 0);
          const cmdStreamImmediate = writerImmediate.finish().buffer;

          const submitRequestId1 = nextRequestId++;
          const submitPromise1 = new Promise((resolve, reject) => pending.set(submitRequestId1, { resolve, reject }));
          worker.postMessage(
            {
              ...GPU_MESSAGE_BASE,
              type: "submit_aerogpu",
              requestId: submitRequestId1,
              contextId: 0,
              signalFence: 1n,
              cmdStream: cmdStreamVsync,
            },
            [cmdStreamVsync],
          );

          const submitRequestId2 = nextRequestId++;
          const submitPromise2 = new Promise((resolve, reject) => pending.set(submitRequestId2, { resolve, reject }));
          worker.postMessage(
            {
              ...GPU_MESSAGE_BASE,
              type: "submit_aerogpu",
              requestId: submitRequestId2,
              contextId: 0,
              signalFence: 2n,
              cmdStream: cmdStreamImmediate,
             },
             [cmdStreamImmediate],
           );

           const r1 = await withTimeout(submitPromise1, 2000);
           const r2 = await withTimeout(submitPromise2, 2000);

           window.__AERO_VSYNC_SUBMIT_RESULT__ = {
             completed1: r1.kind === "value",
             completed2: r2.kind === "value",
             completions: completions.map((c) => ({
               requestId: c.requestId,
               completedFence: c.completedFence ?? null,
               presentCount: c.presentCount ?? null,
            })),
          };

           worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
           worker.terminate();
         } catch (err) {
           window.__AERO_VSYNC_SUBMIT_RESULT__ = { error: String(err) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_VSYNC_SUBMIT_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_VSYNC_SUBMIT_RESULT__);

  expect(result.error ?? null).toBeNull();
  expect(result.completed1).toBe(true);
  expect(result.completed2).toBe(true);
  expect(result.completions).toHaveLength(2);
  expect(result.completions[0].completedFence).toBe(1n);
  expect(result.completions[0].presentCount).toBe(1n);
  expect(result.completions[1].completedFence).toBe(2n);
  expect(result.completions[1].presentCount).toBe(2n);
});

test("GPU worker: multiple VSYNC submit_aerogpu completions arrive without ticks and preserve order", async ({ page }) => {
  await page.goto("/web/blank.html");

  await page.setContent(`
     <script type="module">
      import {
        GPU_PROTOCOL_NAME,
        GPU_PROTOCOL_VERSION,
        FRAME_DIRTY,
        FRAME_STATUS_INDEX,
        isGpuWorkerMessageBase,
      } from "/web/src/ipc/gpu-protocol.ts";
      import { AerogpuCmdWriter, AEROGPU_PRESENT_FLAG_VSYNC } from "/emulator/protocol/aerogpu/aerogpu_cmd.ts";

       const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION };
       const withTimeout = (promise, ms) =>
         Promise.race([
           promise.then((value) => ({ kind: "value", value })),
           new Promise((resolve) => setTimeout(() => resolve({ kind: "timeout" }), ms)),
         ]);

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
          /** @type {any[]} */
          const completions = [];
          let nextRequestId = 1;

          const onMessage = (event) => {
            const msg = event.data;
            if (!isGpuWorkerMessageBase(msg) || typeof msg.type !== "string") return;

            if (msg.type === "ready") {
              readyResolve(msg);
              return;
            }
            if (msg.type === "error") {
              readyReject(new Error("gpu worker error: " + msg.message));
              for (const [, v] of pending) v.reject(new Error("gpu worker error: " + msg.message));
              pending.clear();
              return;
            }
            if (msg.type === "submit_complete") {
              completions.push(msg);
              const entry = pending.get(msg.requestId);
              if (!entry) return;
              pending.delete(msg.requestId);
              entry.resolve(msg);
              return;
            }
            if (msg.type === "screenshot") {
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
          const frameState = new Int32Array(sharedFrameState);
          const sharedFramebuffer = new SharedArrayBuffer(8);

          worker.postMessage({
            ...GPU_MESSAGE_BASE,
            type: "init",
            sharedFrameState,
            sharedFramebuffer,
            sharedFramebufferOffsetBytes: 0,
          });

           await ready;

           const writer1 = new AerogpuCmdWriter();
           writer1.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
           const cmdStream1 = writer1.finish().buffer;

          const writer2 = new AerogpuCmdWriter();
          writer2.presentEx(0, AEROGPU_PRESENT_FLAG_VSYNC, 0);
          const cmdStream2 = writer2.finish().buffer;

          const submitRequestId1 = nextRequestId++;
          const submitPromise1 = new Promise((resolve, reject) => pending.set(submitRequestId1, { resolve, reject }));
          worker.postMessage(
            {
              ...GPU_MESSAGE_BASE,
              type: "submit_aerogpu",
              requestId: submitRequestId1,
              contextId: 0,
              signalFence: 1n,
              cmdStream: cmdStream1,
            },
            [cmdStream1],
          );

          const submitRequestId2 = nextRequestId++;
          const submitPromise2 = new Promise((resolve, reject) => pending.set(submitRequestId2, { resolve, reject }));
          worker.postMessage(
            {
              ...GPU_MESSAGE_BASE,
              type: "submit_aerogpu",
              requestId: submitRequestId2,
              contextId: 0,
              signalFence: 2n,
              cmdStream: cmdStream2,
             },
             [cmdStream2],
           );

           const r1 = await withTimeout(submitPromise1, 2000);
           const r2 = await withTimeout(submitPromise2, 2000);

           window.__AERO_VSYNC_MULTI_RESULT__ = {
             completed1: r1.kind === "value",
             completed2: r2.kind === "value",
             completions: completions.map((c) => ({
               completedFence: c.completedFence ?? null,
               presentCount: c.presentCount ?? null,
             })),
           };

          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
          worker.terminate();
        } catch (err) {
          window.__AERO_VSYNC_MULTI_RESULT__ = { error: String(err) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_VSYNC_MULTI_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_VSYNC_MULTI_RESULT__);

  expect(result.error ?? null).toBeNull();
  expect(result.completed1).toBe(true);
  expect(result.completed2).toBe(true);
  expect(result.completions).toHaveLength(2);
  expect(result.completions[0].completedFence).toBe(1n);
  expect(result.completions[0].presentCount).toBe(1n);
  expect(result.completions[1].completedFence).toBe(2n);
  expect(result.completions[1].presentCount).toBe(2n);
});
