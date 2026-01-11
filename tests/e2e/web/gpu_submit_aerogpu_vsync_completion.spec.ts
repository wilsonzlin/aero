import { expect, test } from "@playwright/test";

test("GPU worker: submit_aerogpu with VSYNC present delays submit_complete until tick", async ({ page }) => {
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

      const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

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

          // Ensure `screenshot` triggers an internal `handleTick()` call without using
          // an explicit GPU-protocol `tick` message.
          frameState[FRAME_STATUS_INDEX] = FRAME_DIRTY;

          const writerVsync = new AerogpuCmdWriter();
          writerVsync.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
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
              signalFence: 2n,
              cmdStream: cmdStreamImmediate,
            },
            [cmdStreamImmediate],
          );

          await sleep(50);
          const receivedBeforeTick = completions.length > 0;

          if (!receivedBeforeTick) {
            // Request a screenshot before ticking. This calls into the worker's internal
            // `handleTick()` to ensure the completion gate is keyed to the explicit
            // `tick` message (not internal present work).
            const screenshotRequestId = nextRequestId++;
            const screenshotPromise = new Promise((resolve, reject) =>
              pending.set(screenshotRequestId, { resolve, reject }),
            );
            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId: screenshotRequestId });
            await screenshotPromise;

            await sleep(50);
            if (completions.length > 0) {
              // Expose the failure mode explicitly; the Playwright assertion will fail below.
              window.__AERO_VSYNC_SUBMIT_RESULT__ = {
                receivedBeforeTick: true,
                completions: completions.map((c) => ({
                  requestId: c.requestId,
                  completedFence: c.completedFence ?? null,
                  presentCount: c.presentCount ?? null,
                })),
              };
              worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
              worker.terminate();
              return;
            }

            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
          }

          await Promise.all([submitPromise1, submitPromise2]);

          window.__AERO_VSYNC_SUBMIT_RESULT__ = {
            receivedBeforeTick,
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
  expect(result.receivedBeforeTick).toBe(false);
  expect(result.completions).toHaveLength(2);
  expect(result.completions[0].completedFence).toBe(1n);
  expect(result.completions[0].presentCount).toBe(1n);
  expect(result.completions[1].completedFence).toBe(2n);
  expect(result.completions[1].presentCount).toBe(2n);
});

test("GPU worker: multiple VSYNC submit_aerogpu completions advance one-per-tick", async ({ page }) => {
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
      const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

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

          // Ensure `screenshot` triggers an internal `handleTick()` call without using
          // an explicit GPU-protocol `tick` message.
          frameState[FRAME_STATUS_INDEX] = FRAME_DIRTY;

          const writer1 = new AerogpuCmdWriter();
          writer1.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
          const cmdStream1 = writer1.finish().buffer;

          const writer2 = new AerogpuCmdWriter();
          writer2.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
          const cmdStream2 = writer2.finish().buffer;

          const submitRequestId1 = nextRequestId++;
          const submitPromise1 = new Promise((resolve, reject) => pending.set(submitRequestId1, { resolve, reject }));
          worker.postMessage(
            {
              ...GPU_MESSAGE_BASE,
              type: "submit_aerogpu",
              requestId: submitRequestId1,
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
              signalFence: 2n,
              cmdStream: cmdStream2,
            },
            [cmdStream2],
          );

          await sleep(50);
          const receivedBeforeTick = completions.length > 0;

          if (receivedBeforeTick) {
            window.__AERO_VSYNC_MULTI_RESULT__ = { receivedBeforeTick: true, completions };
            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
            worker.terminate();
            return;
          }

          const screenshotRequestId = nextRequestId++;
          const screenshotPromise = new Promise((resolve, reject) => pending.set(screenshotRequestId, { resolve, reject }));
          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId: screenshotRequestId });
          await screenshotPromise;

          await sleep(50);
          if (completions.length > 0) {
            window.__AERO_VSYNC_MULTI_RESULT__ = { receivedBeforeTick: true, completions };
            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
            worker.terminate();
            return;
          }

          // Tick once: only the first VSYNC submission should complete.
          worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
          await submitPromise1;

          const submit2AfterTick1 = await Promise.race([
            submitPromise2.then(() => ({ kind: "submit" })),
            sleep(50).then(() => ({ kind: "timeout" })),
          ]);
          const submit2CompletedOnTick1 = submit2AfterTick1.kind === "submit";

          // Tick again: now the second submission should complete.
          if (!submit2CompletedOnTick1) {
            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
          }
          await submitPromise2;

          window.__AERO_VSYNC_MULTI_RESULT__ = {
            receivedBeforeTick: false,
            submit2CompletedOnTick1,
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
  expect(result.receivedBeforeTick).toBe(false);
  expect(result.submit2CompletedOnTick1).toBe(false);
  expect(result.completions).toHaveLength(2);
  expect(result.completions[0].completedFence).toBe(1n);
  expect(result.completions[0].presentCount).toBe(1n);
  expect(result.completions[1].completedFence).toBe(2n);
  expect(result.completions[1].presentCount).toBe(2n);
});
