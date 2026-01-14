import { expect, test } from "@playwright/test";

test("Workers panel: VGA canvas captures keyboard input and forwards batches to the IO worker", async ({ page }) => {
  test.setTimeout(90_000);
  page.setDefaultTimeout(90_000);

  // The repo-root Vite harness serves the legacy web UI at `/web/` (and the
  // canonical harness UI at `/`). In other deployments the legacy UI may be
  // mounted at the origin root. Start at `/` as the smoke-test entrypoint, then
  // fall back to `/web/index.html` when the Workers panel isn't present.
  // Explicitly force legacy runtime so this test continues to validate the IO-worker input path
  // even if the default runtime flips to `machine` in the future.
  await page.goto("/?vmRuntime=legacy", { waitUntil: "load" });
  try {
    await page.locator("#workers-start").waitFor({ state: "attached", timeout: 2000 });
  } catch {
    await page.goto("/web/index.html?vmRuntime=legacy", { waitUntil: "load" });
  }

  const support = await page.evaluate(() => {
    const crossOriginIsolated = globalThis.crossOriginIsolated === true;
    const sharedArrayBuffer = typeof SharedArrayBuffer !== "undefined";
    const atomics = typeof Atomics !== "undefined";
    const worker = typeof Worker !== "undefined";
    const wasm = typeof WebAssembly !== "undefined" && typeof WebAssembly.Memory === "function";
    const webgpu = typeof (globalThis as any).navigator?.gpu !== "undefined";
    // Match `web/src/platform/features.ts`: treat WebGL2 as available only if OffscreenCanvas can
    // create a WebGL2 context (the worker-driven renderer relies on this).
    let webgl2 = false;
    try {
      if (typeof OffscreenCanvas !== "undefined") {
        const canvas = new OffscreenCanvas(1, 1);
        webgl2 = !!canvas.getContext("webgl2");
      } else {
        const canvas = document.createElement("canvas");
        webgl2 = !!canvas.getContext("webgl2");
      }
    } catch {
      webgl2 = false;
    }
    let wasmThreads = false;
    if (wasm) {
      try {
        // eslint-disable-next-line no-new
        new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
        wasmThreads = true;
      } catch {
        wasmThreads = false;
      }
    }
    return { crossOriginIsolated, sharedArrayBuffer, atomics, worker, wasm, wasmThreads, webgl2, webgpu };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics, "Atomics is unavailable in this browser configuration.");
  test.skip(!support.worker, "Web Workers are unavailable in this environment.");
  test.skip(!support.wasm, "WebAssembly.Memory is unavailable in this environment.");
  test.skip(!support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");
  test.skip(!support.webgl2 && !support.webgpu, "Workers panel demo requires WebGL2 or WebGPU.");

  await page.locator("#workers-start").click();

  // Ensure the VM workers started and the workers panel input capture is active.
  await page.waitForFunction(() => {
    const text = document.querySelector("#workers-input-status")?.textContent ?? "";
    return text.includes("targetWorker=io:ready") && text.includes("ioWorker=ready") && /ioBatches=\d+/.test(text);
  });

  const initialBatches = await page.evaluate(() => {
    const text = document.querySelector("#workers-input-status")?.textContent ?? "";
    const match = /ioBatches=(\d+)/.exec(text);
    return match ? Number.parseInt(match[1] ?? "", 10) : null;
  });
  expect(initialBatches).toBe(0);

  // Click the VGA canvas to focus it (pointer lock may fail in headless CI; focus must be enough).
  await page.locator("#workers-vga-canvas").click();
  await page.keyboard.down("KeyA");
  await page.keyboard.up("KeyA");

  await page.waitForFunction(
    (prev) => {
      const text = document.querySelector("#workers-input-status")?.textContent ?? "";
      const match = /ioBatches=(\d+)/.exec(text);
      if (!match) return false;
      const cur = Number.parseInt(match[1] ?? "", 10);
      return Number.isFinite(cur) && cur > (prev as number);
    },
    initialBatches,
    { timeout: 10_000 },
  );
});
