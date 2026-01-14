import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("wddm scanout recovery: survives WebGL2 context loss + re-presents from guest RAM base_paddr", async ({
  page,
  browserName,
}) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/wddm-scanout-smoke.html", { waitUntil: "load" });
  await waitForReady(page);

  const result = await page.evaluate(async () => {
    const api = (window as any).__aeroTest;
    if (!api) throw new Error("__aeroTest missing");
    if (api.error) throw new Error(api.error);
    if (api.pass !== true) {
      throw new Error(
        `wddm scanout mismatch: presented got=${api.hash ?? "none"} expected=${api.expectedHash ?? "none"} ` +
          `source got=${api.sourceHash ?? "none"} expected=${api.expectedSourceHash ?? "none"}`,
      );
    }
    if (typeof api.runContextLossRecovery !== "function") {
      throw new Error("wddm scanout recovery helper missing (runContextLossRecovery)");
    }

    const recovery = await api.runContextLossRecovery();
    return {
      backend: api.backend ?? "unknown",
      hash: api.hash,
      expectedHash: api.expectedHash,
      recovery,
    };
  });

  expect(result.backend).toBe("webgl2_raw");
  expect(result.hash).toBe(result.expectedHash);

  expect(result.recovery.ok).toBe(true);
  expect(result.recovery.loseOk).toBe(true);
  expect(result.recovery.restoreOk).toBe(true);
  expect(result.recovery.after?.hash).toBe(result.expectedHash);

  // Sanity-check the recovered presented output samples.
  expect(result.recovery.after?.samples?.topLeft).toEqual([255, 0, 0, 255]);
  expect(result.recovery.after?.samples?.topRight).toEqual([0, 255, 0, 255]);
  expect(result.recovery.after?.samples?.bottomLeft).toEqual([0, 0, 255, 255]);
  expect(result.recovery.after?.samples?.bottomRight).toEqual([255, 255, 255, 255]);

  // Telemetry: recovery counters should reflect a successful WDDM-scoped recovery.
  expect(result.recovery.after?.counters).toBeTruthy();
  const counters = result.recovery.after?.counters as Record<string, unknown> | undefined;
  expect(typeof counters?.recoveries_succeeded).toBe("number");
  expect(typeof counters?.recoveries_succeeded_wddm).toBe("number");
  expect((counters?.recoveries_succeeded as number) ?? 0).toBeGreaterThanOrEqual(1);
  expect((counters?.recoveries_succeeded_wddm as number) ?? 0).toBeGreaterThanOrEqual(1);

  // The worker should re-emit READY after recovery (readyCount increments).
  const readyBefore = result.recovery.before?.readyCount ?? 0;
  const readyAfter = result.recovery.after?.readyCount ?? 0;
  expect(readyAfter).toBeGreaterThanOrEqual(readyBefore + 1);
});

