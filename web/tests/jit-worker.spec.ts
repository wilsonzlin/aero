import { expect, test } from "@playwright/test";

test("jit worker compiles wasm modules and caches by content hash", async ({ page }) => {
  await page.goto("/blank.html");

  const result = await page.evaluate(async () => {
    const worker = new Worker("/src/workers/jit.worker.ts", { type: "module" });

    const compileOnce = (id: number, wasmBytes: ArrayBuffer) =>
      new Promise<any>((resolve, reject) => {
        const onMessage = (ev: MessageEvent) => {
          const msg = ev.data as any;
          if (!msg || typeof msg !== "object") return;
          if (msg.id !== id) return;
          if (msg.type !== "jit:compiled" && msg.type !== "jit:error") return;
          clearTimeout(timeout);
          worker.removeEventListener("message", onMessage);
          resolve(msg);
        };

        const timeout = setTimeout(() => {
          worker.removeEventListener("message", onMessage);
          reject(new Error("timeout"));
        }, 5000);

        worker.addEventListener("message", onMessage);

        const req = { type: "jit:compile", id, wasmBytes };
        worker.postMessage(req, [wasmBytes]);
      });

    const first = await compileOnce(1, new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]).buffer);
    let firstInstantiated = false;
    if (first.type === "jit:compiled") {
      await WebAssembly.instantiate(first.module, {});
      firstInstantiated = true;
    }

    // Same bytes again should hit cache.
    const second = await compileOnce(2, new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]).buffer);

    worker.terminate();

    return { first, second, firstInstantiated };
  });

  expect(result.firstInstantiated).toBe(true);
  expect(result.first.type).toBe("jit:compiled");
  expect(result.first.durationMs).toBeGreaterThanOrEqual(0);

  expect(result.second.type).toBe("jit:compiled");
  expect(result.second.cached).toBe(true);
});

test("jit worker returns csp_blocked when platformFeatures.jit_dynamic_wasm=false", async ({ page }) => {
  await page.goto("/blank.html");

  const result = await page.evaluate(async () => {
    const worker = new Worker("/src/workers/jit.worker.ts", { type: "module" });

    // Simulate CSP gating via config.update (used by the WorkerCoordinator in the real app).
    worker.postMessage({
      kind: "config.update",
      version: 1,
      config: {
        guestMemoryMiB: 512,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
        logLevel: "info",
      },
      platformFeatures: { jit_dynamic_wasm: false },
    });

    const wasmBytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]).buffer;

    const msg = await new Promise<any>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("timeout")), 5000);
      worker.addEventListener("message", (ev: MessageEvent) => {
        const data = ev.data as any;
        if (!data || typeof data !== "object") return;
        if (data.type !== "jit:error" || data.id !== 1) return;
        clearTimeout(timeout);
        resolve(data);
      });
      worker.postMessage({ type: "jit:compile", id: 1, wasmBytes }, [wasmBytes]);
    });

    worker.terminate();
    return msg;
  });

  expect(result.type).toBe("jit:error");
  expect(result.code).toBe("csp_blocked");
});
