import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("gpu worker smoke: renders pattern and returns screenshot hash", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/gpu-worker-smoke.html", { waitUntil: "load" });
  await waitForReady(page);

  const result = await page.evaluate(async () => {
    const api = (window as any).__aeroTest;
    if (!api) throw new Error("__aeroTest missing");
    if (api.error) throw new Error(api.error);
    if (api.pass !== true) {
      throw new Error(`hash mismatch: got=${api.hash ?? "none"} expected=${api.expectedHash ?? "none"}`);
    }

    const samples = api.samplePixels ? await api.samplePixels() : null;
    return {
      backend: api.backend ?? "unknown",
      hash: api.hash,
      expectedHash: api.expectedHash,
      samples,
    };
  });

  expect(result.backend === "webgpu" || result.backend === "webgl2_raw").toBe(true);
  expect(result.hash).toBe(result.expectedHash);

  expect(result.samples).not.toBeNull();
  expect(result.samples.width).toBe(64);
  expect(result.samples.height).toBe(64);

  expect(result.samples.topLeft).toEqual([255, 0, 0, 255]);
  expect(result.samples.topRight).toEqual([0, 255, 0, 255]);
  expect(result.samples.bottomLeft).toEqual([0, 0, 255, 255]);
  expect(result.samples.bottomRight).toEqual([255, 255, 255, 255]);
});

test("gpu worker smoke: disableWebGpu forces WebGL2 fallback", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/gpu-worker-smoke.html?disableWebGpu=1", { waitUntil: "load" });
  await waitForReady(page);

  // The worker should emit a structured Init warning event describing the fallback.
  await page.waitForFunction(() => {
    const text = document.getElementById("status")?.textContent ?? "";
    // The smoke page may include the backend kind in parentheses (e.g. `Init (webgl2_raw)`),
    // so avoid matching an exact `Init:` substring.
    return text.includes("gpu_event warn Init") && text.includes("GPU backend init fell back from");
  });

  const result = await page.evaluate(() => {
    const api = (window as any).__aeroTest;
    if (!api) throw new Error("__aeroTest missing");
    if (api.error) throw new Error(api.error);
    return {
      backend: api.backend ?? "unknown",
      fallback: api.fallback ?? null,
      pass: api.pass,
      events: Array.isArray(api.events) ? api.events : null,
    };
  });

  expect(result.backend).toBe("webgl2_raw");
  expect(result.fallback).not.toBeNull();
  expect(result.fallback.from).toBe("webgpu");
  expect(result.fallback.to).toBe("webgl2_raw");
  expect(result.pass).toBe(true);

  const initWarnEvent = result.events
    ? result.events.find((ev: any) => ev && ev.category === "Init" && ev.severity === "warn") ?? null
    : null;
  expect(initWarnEvent).not.toBeNull();
  expect(initWarnEvent.backend_kind).toBe("webgl2_raw");
  expect(initWarnEvent.details).toMatchObject({ from: "webgpu", to: "webgl2_raw" });
});

test("gpu worker smoke: presenter errors emit structured events", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/gpu-worker-smoke.html?triggerPresenterError=1", { waitUntil: "load" });
  await waitForReady(page);

  // The smoke page triggers the same validation error twice. The worker should:
  // - post legacy `type:"error"` messages twice (printed as `gpu_error ...`)
  // - emit a structured `events` message only once due to per-generation dedupe.
  await page.waitForFunction(() => {
    const text = document.getElementById("status")?.textContent ?? "";
    const needle = "gpu_error msg=cursor_set_image width/height must be non-zero";
    return text.split(needle).length - 1 >= 2;
  });

  await page.waitForFunction(() => {
    const text = document.getElementById("status")?.textContent ?? "";
    return (
      text.includes("gpu_event error Validation") && text.includes("cursor_set_image width/height must be non-zero")
    );
  });

  const counts = await page.evaluate(() => {
    const text = document.getElementById("status")?.textContent ?? "";
    const eventNeedle = "gpu_event error Validation";
    const errorNeedle = "gpu_error msg=cursor_set_image width/height must be non-zero";
    return {
      events: text.split(eventNeedle).length - 1,
      errors: text.split(errorNeedle).length - 1,
    };
  });
  expect(counts.errors).toBeGreaterThanOrEqual(2);
  expect(counts.events).toBe(1);
});

test("gpu worker smoke: init failure emits structured Init fatal event", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/gpu-worker-smoke.html?expectInitFailure=1&forceBackend=webgpu&disableWebGpu=1", {
    waitUntil: "load",
  });
  await waitForReady(page);

  await page.waitForFunction(() => {
    const text = document.getElementById("status")?.textContent ?? "";
    return text.includes("gpu_event fatal Init") && text.includes("WebGPU backend was disabled");
  });

  const initEvent = await page.evaluate(() => {
    const events = ((window as any).__aeroTest as any)?.events as any[] | undefined;
    if (!Array.isArray(events)) return null;
    return (
      events.find((ev) => ev && ev.category === "Init" && ev.severity === "fatal" && typeof ev.message === "string") ??
      null
    );
  });
  expect(initEvent).not.toBeNull();
  expect(initEvent.backend_kind).toBe("webgpu");
});
