import { expect, test } from "@playwright/test";

test("Workers panel: VGA canvas captures keyboard input and forwards batches to the CPU worker in machine runtime", async ({
  page,
}) => {
  test.setTimeout(90_000);
  page.setDefaultTimeout(90_000);

  // The repo-root Vite harness serves the legacy web UI at `/web/` (and the
  // canonical harness UI at `/`). In other deployments the legacy UI may be
  // mounted at the origin root. Start at `/` as the smoke-test entrypoint, then
  // fall back to `/web/index.html` when the Workers panel isn't present.
  await page.goto("/?vmRuntime=machine", { waitUntil: "load" });
  try {
    await page.locator("#workers-start").waitFor({ state: "attached", timeout: 2000 });
  } catch {
    await page.goto("/web/index.html?vmRuntime=machine", { waitUntil: "load" });
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
  await page.waitForFunction(
    () => {
      const text = document.querySelector("#workers-input-status")?.textContent ?? "";
      return text.includes("targetWorker=cpu:ready") && /ioBatches=\d+/.test(text);
    },
    { timeout: 60_000 },
  );

  // Machine runtime uses `machine_cpu.worker.ts`, which initializes the canonical `api.Machine`
  // asynchronously after reaching READY. Wait for WASM to be ready so the CPU worker can actually
  // process input batches (otherwise the worker will just recycle buffers without incrementing
  // `ioBatches`).
  await page.waitForFunction(
    () => {
      const start = document.querySelector("#workers-start");
      const panel = start?.closest("div.panel");
      if (!panel) return false;
      const cpuRow = Array.from(panel.querySelectorAll("li")).find((li) => (li.textContent ?? "").startsWith("cpu:"));
      const text = cpuRow?.textContent ?? "";
      return text.includes("wasm(") && !text.includes("wasm(pending)");
    },
    { timeout: 60_000 },
  );

  const getIoBatches = async (): Promise<number> => {
    const value = await page.evaluate(() => {
      const text = document.querySelector("#workers-input-status")?.textContent ?? "";
      const match = /ioBatches=(\d+)/.exec(text);
      return match ? Number.parseInt(match[1] ?? "", 10) : null;
    });
    if (typeof value !== "number" || !Number.isFinite(value)) {
      throw new Error("Failed to parse ioBatches=... from #workers-input-status.");
    }
    return value;
  };

  const pressKeyAndWaitForBatchIncrement = async (prev: number): Promise<number> => {
    // In machine runtime, the CPU worker posts WASM_READY before the `Machine` instance is fully
    // constructed. Input batches delivered in that small window are recycled but not processed,
    // so retry a few times to avoid flaking on slower machines.
    const MAX_ATTEMPTS = 20;
    let last = prev;
    for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt += 1) {
      // Click the VGA canvas to focus it (pointer lock may fail in headless CI; focus must be enough).
      await page.locator("#workers-vga-canvas").click();
      await page.keyboard.press("KeyA");

      try {
        await page.waitForFunction(
          (p) => {
            const text = document.querySelector("#workers-input-status")?.textContent ?? "";
            const match = /ioBatches=(\d+)/.exec(text);
            if (!match) return false;
            const cur = Number.parseInt(match[1] ?? "", 10);
            return Number.isFinite(cur) && cur > (p as number);
          },
          last,
          { timeout: 1500 },
        );
        return await getIoBatches();
      } catch {
        // Keep trying until the CPU worker is ready to process input batches.
        last = await getIoBatches();
        await page.waitForTimeout(200);
      }
    }
    throw new Error(`Timed out waiting for ioBatches to increment after ${MAX_ATTEMPTS} keypress attempts.`);
  };

  const initialIoBatches = await getIoBatches();
  await pressKeyAndWaitForBatchIncrement(initialIoBatches);

  // Stop and restart workers to ensure input capture is recreated for the new CPU worker instance
  // (and potentially a recreated VGA canvas if OffscreenCanvas transfer was used).
  await page.locator("#workers-stop").click();

  await page.waitForFunction(
    () => {
      const text = document.querySelector("#workers-input-status")?.textContent ?? "";
      return text.includes("targetWorker=cpu:stopped") && text.includes("ioWorker=stopped");
    },
    { timeout: 60_000 },
  );

  await page.locator("#workers-start").click();

  await page.waitForFunction(
    () => {
      const text = document.querySelector("#workers-input-status")?.textContent ?? "";
      return text.includes("targetWorker=cpu:ready") && /ioBatches=\d+/.test(text);
    },
    { timeout: 60_000 },
  );

  await page.waitForFunction(
    () => {
      const start = document.querySelector("#workers-start");
      const panel = start?.closest("div.panel");
      if (!panel) return false;
      const cpuRow = Array.from(panel.querySelectorAll("li")).find((li) => (li.textContent ?? "").startsWith("cpu:"));
      const text = cpuRow?.textContent ?? "";
      return text.includes("wasm(") && !text.includes("wasm(pending)");
    },
    { timeout: 60_000 },
  );

  const afterRestartIoBatches = await getIoBatches();

  await pressKeyAndWaitForBatchIncrement(afterRestartIoBatches);

  expect(await getIoBatches()).toBeGreaterThan(afterRestartIoBatches);
});
