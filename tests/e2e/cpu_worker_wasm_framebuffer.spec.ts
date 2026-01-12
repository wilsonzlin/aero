import { expect, test, type Page } from "@playwright/test";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const THREADED_WASM_BINARY = fileURLToPath(
  new URL("../../web/src/wasm/pkg-threaded/aero_wasm_bg.wasm", import.meta.url),
);
const HAS_THREADED_WASM_BINARY = existsSync(THREADED_WASM_BINARY);

if (process.env.CI && !HAS_THREADED_WASM_BINARY) {
  throw new Error(
    [
      "Threaded WASM package missing in CI.",
      "",
      `Expected: ${THREADED_WASM_BINARY}`,
      "",
      "Build it with (from the repo root):",
      "  npm -w web run wasm:build",
    ].join("\n"),
  );
}

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("cpu worker wasm demo: publishes shared framebuffer frames from WASM", async ({ page }) => {
  test.skip(
    !HAS_THREADED_WASM_BINARY,
    "Threaded WASM package missing (required for shared-memory worker runtime). Build it with `npm -w web run wasm:build:threaded`.",
  );
  await page.goto("http://127.0.0.1:5173/web/cpu-worker-wasm-framebuffer-smoke.html", { waitUntil: "load" });

  const support = await page.evaluate(() => {
    let wasmThreads = false;
    try {
      // eslint-disable-next-line no-new
      new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      wasmThreads = true;
    } catch {
      wasmThreads = false;
    }
    return {
      crossOriginIsolated: globalThis.crossOriginIsolated === true,
      sharedArrayBuffer: typeof SharedArrayBuffer !== "undefined",
      atomics: typeof Atomics !== "undefined",
      wasmThreads,
    };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics || !support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");

  await waitForReady(page);

  const result = await page.evaluate(() => (window as any).__aeroTest);
  expect(result).toBeTruthy();
  if (!result || typeof result !== "object") {
    throw new Error("Missing __aeroTest result");
  }
  if ((result as any).error) {
    throw new Error(String((result as any).error));
  }

  expect((result as any).pass).toBe(true);
  expect((result as any).hashes?.first).not.toBe((result as any).hashes?.second);
});
