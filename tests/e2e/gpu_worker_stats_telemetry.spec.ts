import { expect, test } from "@playwright/test";

test("gpu worker telemetry: emits stats (with wasm payload) on webgl2_wgpu presenter", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/blank.html");

  const result = await page.evaluate(async () => {
    const proto = await import("/web/src/ipc/gpu-protocol.ts");
    const GPU_MESSAGE_BASE = { protocol: proto.GPU_PROTOCOL_NAME, protocolVersion: proto.GPU_PROTOCOL_VERSION };

    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);
    const offscreen = canvas.transferControlToOffscreen();

    const width = 16;
    const height = 16;
    const strideBytes = width * 4;
    const headerBytes = 8 * 4;

    // Legacy framebuffer_protocol (AERO) layout: 8 i32 header + RGBA8 bytes.
    const sharedFramebuffer = new SharedArrayBuffer(headerBytes + strideBytes * height);
    const header = new Int32Array(sharedFramebuffer, 0, 8);
    const pixels = new Uint8Array(sharedFramebuffer, headerBytes, strideBytes * height);

    // Inlined header fields from `web/src/display/framebuffer_protocol.ts`.
    header[0] = 0x4f524541; // FRAMEBUFFER_MAGIC ("AERO")
    header[1] = 1; // FRAMEBUFFER_VERSION
    header[2] = width;
    header[3] = height;
    header[4] = strideBytes;
    header[5] = 1; // FRAMEBUFFER_FORMAT_RGBA8888
    header[6] = 0; // frame counter
    header[7] = 1; // config counter

    const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
    const frameState = new Int32Array(sharedFrameState);
    frameState[proto.FRAME_STATUS_INDEX] = proto.FRAME_PRESENTED;
    frameState[proto.FRAME_SEQ_INDEX] = 0;

    // Publish a single deterministic frame so the presenter path is exercised.
    for (let i = 0; i < pixels.length; i += 4) {
      pixels[i + 0] = 0xff;
      pixels[i + 1] = 0x00;
      pixels[i + 2] = 0xff;
      pixels[i + 3] = 0xff;
    }

    const worker = new Worker("/web/src/workers/gpu.worker.ts", { type: "module" });

    let readyMsg: any = null;
    let firstEventsMsg: any = null;
    let lastStatsMsg: any = null;

    const waitForMessage = <T>(
      predicate: (msg: any) => msg is T,
      timeoutMs: number,
      label: string,
    ): Promise<T> => {
      return new Promise<T>((resolve, reject) => {
        const timer = setTimeout(() => {
          reject(new Error(`Timed out waiting for ${label}`));
        }, timeoutMs);

        const onMessage = (ev: MessageEvent) => {
          const msg = ev.data;
          if (!proto.isGpuWorkerMessageBase(msg) || typeof msg.type !== "string") return;

          if (msg.type === "error") {
            clearTimeout(timer);
            worker.removeEventListener("message", onMessage);
            worker.removeEventListener("error", onError);
            reject(new Error(String(msg.message ?? "gpu worker error")));
            return;
          }

          if (msg.type === "events" && firstEventsMsg == null) {
            firstEventsMsg = msg;
          }

          if (msg.type === "stats") {
            lastStatsMsg = msg;
          }

          if (predicate(msg)) {
            clearTimeout(timer);
            worker.removeEventListener("message", onMessage);
            worker.removeEventListener("error", onError);
            resolve(msg);
          }
        };

        const onError = (event: unknown) => {
          clearTimeout(timer);
          worker.removeEventListener("message", onMessage);
          worker.removeEventListener("error", onError);
          reject((event as any)?.error ?? event);
        };

        worker.addEventListener("message", onMessage);
        worker.addEventListener("error", onError);
      });
    };

    try {
      worker.postMessage(
        {
          ...GPU_MESSAGE_BASE,
          type: "init",
          canvas: offscreen,
          sharedFrameState,
          sharedFramebuffer,
          sharedFramebufferOffsetBytes: 0,
          options: {
            forceBackend: "webgl2_wgpu",
            disableWebGpu: true,
            outputWidth: width,
            outputHeight: height,
            dpr: 1,
          },
        },
        [offscreen],
      );

      // Wait for backend init so telemetry should have access to the wasm module.
      readyMsg = await waitForMessage<any>((msg): msg is any => msg.type === "ready", 15_000, "gpu worker ready");

      // Exercise present at least once.
      Atomics.add(header, 6, 1);
      Atomics.add(frameState, proto.FRAME_SEQ_INDEX, 1);
      Atomics.store(frameState, proto.FRAME_STATUS_INDEX, proto.FRAME_DIRTY);
      worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });

      // Wait for a stats message that includes a wasm payload.
      let statsMsg: any;
      try {
        statsMsg = await waitForMessage<any>(
          (msg): msg is any => msg.type === "stats" && msg.wasm !== undefined && msg.wasm !== null,
          15_000,
          "gpu worker stats with wasm payload",
        );
      } catch (err) {
        const suffix =
          lastStatsMsg && lastStatsMsg.type === "stats"
            ? ` (last stats message had wasm=${String(lastStatsMsg.wasm !== undefined && lastStatsMsg.wasm !== null)})`
            : "";
        throw new Error(`${String(err)}${suffix}`);
      }

      return { readyMsg, statsMsg, eventsMsg: firstEventsMsg };
    } finally {
      try {
        worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
      } catch {
        // Ignore; the worker may already be terminating after an init failure.
      }
      worker.terminate();
    }
  });

  expect(result.readyMsg.backendKind).toBe("webgl2_wgpu");

  const stats = result.statsMsg;
  expect(stats.version).toBe(1);
  expect(stats.counters).toBeTruthy();

  const counters = stats.counters as Record<string, unknown>;
  for (const key of [
    "presents_attempted",
    "presents_succeeded",
    "recoveries_attempted",
    "recoveries_succeeded",
    "surface_reconfigures",
  ]) {
    expect(typeof counters[key]).toBe("number");
  }

  expect(stats.wasm).toBeTruthy();
  const wasmRaw = stats.wasm as unknown;
  const wasmStats =
    typeof wasmRaw === "string"
      ? (() => {
          try {
            return JSON.parse(wasmRaw) as unknown;
          } catch {
            return wasmRaw;
          }
        })()
      : wasmRaw;

  if (!wasmStats || typeof wasmStats !== "object") {
    throw new Error(`Unexpected wasm stats payload type: ${typeof wasmStats}`);
  }

  // Keys from `aero_gpu::stats::GpuStatsSnapshot`.
  const wasmObj = wasmStats as Record<string, unknown>;
  for (const key of [
    "presents_attempted",
    "presents_succeeded",
    "recoveries_attempted",
    "recoveries_succeeded",
    "surface_reconfigures",
  ]) {
    expect(typeof wasmObj[key]).toBe("number");
  }

  // Optional: validate wasm-reported error event shape if events were emitted.
  if (result.eventsMsg) {
    expect(result.eventsMsg.version).toBe(1);
    expect(Array.isArray(result.eventsMsg.events)).toBe(true);
    for (const ev of result.eventsMsg.events as any[]) {
      expect(typeof ev.severity).toBe("string");
      expect(typeof ev.category).toBe("string");
      expect(typeof ev.message).toBe("string");
    }
  }
});
