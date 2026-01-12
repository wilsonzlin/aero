import { expect, test } from "@playwright/test";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

const THREADED_AERO_WASM_BINARY_RELEASE = fileURLToPath(new URL("../../web/src/wasm/pkg-threaded/aero_wasm_bg.wasm", import.meta.url));
const THREADED_AERO_WASM_JS_RELEASE = fileURLToPath(new URL("../../web/src/wasm/pkg-threaded/aero_wasm.js", import.meta.url));
const THREADED_AERO_WASM_BINARY_DEV = fileURLToPath(new URL("../../web/src/wasm/pkg-threaded-dev/aero_wasm_bg.wasm", import.meta.url));
const THREADED_AERO_WASM_JS_DEV = fileURLToPath(new URL("../../web/src/wasm/pkg-threaded-dev/aero_wasm.js", import.meta.url));
const HAS_THREADED_AERO_WASM_BINARY =
  (existsSync(THREADED_AERO_WASM_BINARY_RELEASE) && existsSync(THREADED_AERO_WASM_JS_RELEASE)) ||
  (existsSync(THREADED_AERO_WASM_BINARY_DEV) && existsSync(THREADED_AERO_WASM_JS_DEV));

if (process.env.CI && !HAS_THREADED_AERO_WASM_BINARY) {
  throw new Error(
    [
      "Threaded aero-wasm package missing in CI.",
      "",
      "Build it with (from the repo root):",
      "  npm -w web run wasm:build",
      "",
      "Expected one of:",
      `- ${THREADED_AERO_WASM_BINARY_RELEASE} (+ ${THREADED_AERO_WASM_JS_RELEASE})`,
      `- ${THREADED_AERO_WASM_BINARY_DEV} (+ ${THREADED_AERO_WASM_JS_DEV})`,
    ].join("\n"),
  );
}

test("HDA capture consumes synthetic mic ring and DMA-writes PCM into guest RAM", async ({ page }) => {
  // Bringing up the worker VM + WASM device models can take longer in CI/headless environments.
  // Keep this higher than the harness' internal timeouts to avoid flakiness.
  test.setTimeout(90_000);
  test.skip(test.info().project.name !== "chromium", "HDA capture test only runs on Chromium.");
  test.skip(!HAS_THREADED_AERO_WASM_BINARY, "Requires threaded aero-wasm package (npm -w web run wasm:build:threaded).");
  page.setDefaultTimeout(90_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-hda-capture-synthetic");

  await page.waitForFunction(() => {
    // Exposed by the repo-root Vite harness UI (`src/main.ts`).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (globalThis as any).__aeroAudioHdaCaptureSyntheticResult?.done === true;
  });

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (globalThis as any).__aeroAudioHdaCaptureSyntheticResult as Record<string, unknown> | undefined;
  });

  expect(result).toBeTruthy();
  expect(result?.ok).toBe(true);
  expect(result?.pcmNonZero).toBe(true);
  expect(result?.micReadDelta).toBeGreaterThan(0);
  expect(result?.micWriteDelta).toBeGreaterThan(0);
  // Startup can be racy in CI; allow some dropped samples but ensure it stays bounded.
  expect(result?.micDroppedDelta).toBeLessThanOrEqual(96_000);
});
