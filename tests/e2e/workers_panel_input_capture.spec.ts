import { expect, test } from "@playwright/test";

test("Workers panel: VGA canvas captures keyboard input and forwards batches to the IO worker", async ({ page }) => {
  test.setTimeout(90_000);
  page.setDefaultTimeout(90_000);

  // The repo-root Vite harness serves the legacy web UI at `/web/` (and the
  // canonical harness UI at `/`). In other deployments the legacy UI may be
  // mounted at the origin root. Start at `/` as the smoke-test entrypoint, then
  // fall back to `/web/index.html` when the Workers panel isn't present.
  await page.goto("/", { waitUntil: "load" });
  if ((await page.locator("#workers-start").count()) === 0) {
    await page.goto("/web/index.html", { waitUntil: "load" });
  }

  const support = await page.evaluate(() => {
    const crossOriginIsolated = globalThis.crossOriginIsolated === true;
    const sharedArrayBuffer = typeof SharedArrayBuffer !== "undefined";
    const atomics = typeof Atomics !== "undefined";
    const worker = typeof Worker !== "undefined";
    const wasm = typeof WebAssembly !== "undefined" && typeof WebAssembly.Memory === "function";
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
    return { crossOriginIsolated, sharedArrayBuffer, atomics, worker, wasm, wasmThreads };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics, "Atomics is unavailable in this browser configuration.");
  test.skip(!support.worker, "Web Workers are unavailable in this environment.");
  test.skip(!support.wasm, "WebAssembly.Memory is unavailable in this environment.");
  test.skip(!support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");

  await page.locator("#workers-start").click();

  // Ensure the VM workers started and the workers panel input capture is active.
  await page.waitForFunction(() => {
    const text = document.querySelector("#workers-input-status")?.textContent ?? "";
    return text.includes("ioWorker=ready") && /ioBatches=\d+/.test(text);
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
