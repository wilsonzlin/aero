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

          const writer = new AerogpuCmdWriter();
          writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
          const cmdStream = writer.finish().buffer;

          const submitRequestId = nextRequestId++;
          const submitPromise = new Promise((resolve, reject) => pending.set(submitRequestId, { resolve, reject }));
          worker.postMessage(
            {
              ...GPU_MESSAGE_BASE,
              type: "submit_aerogpu",
              requestId: submitRequestId,
              signalFence: 1n,
              cmdStream,
            },
            [cmdStream],
          );

          const early = await Promise.race([
            submitPromise.then((submit) => ({ kind: "submit", submit })),
            sleep(50).then(() => ({ kind: "timeout" })),
          ]);

          const receivedBeforeTick = early.kind === "submit";

          if (early.kind !== "submit") {
            // Request a screenshot before ticking. This calls into the worker's internal
            // `handleTick()` to ensure the completion gate is keyed to the explicit
            // `tick` message (not internal present work).
            const screenshotRequestId = nextRequestId++;
            const screenshotPromise = new Promise((resolve, reject) =>
              pending.set(screenshotRequestId, { resolve, reject }),
            );
            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId: screenshotRequestId });
            await screenshotPromise;

            const afterScreenshot = await Promise.race([
              submitPromise.then(() => ({ kind: "submit" })),
              sleep(50).then(() => ({ kind: "timeout" })),
            ]);
            if (afterScreenshot.kind === "submit") {
              // Expose the failure mode explicitly; the Playwright assertion will fail below.
              window.__AERO_VSYNC_SUBMIT_RESULT__ = {
                receivedBeforeTick: true,
                completedFence: 1n,
                presentCount: null,
              };
              worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
              worker.terminate();
              return;
            }

            worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
          }

          const submit = early.kind === "submit" ? early.submit : await submitPromise;

          window.__AERO_VSYNC_SUBMIT_RESULT__ = {
            receivedBeforeTick,
            completedFence: submit.completedFence ?? null,
            presentCount: submit.presentCount ?? null,
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
  expect(result.completedFence).toBe(1n);
  expect(result.presentCount).toBe(1n);
});
