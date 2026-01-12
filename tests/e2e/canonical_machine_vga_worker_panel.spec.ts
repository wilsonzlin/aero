import { expect, test, type Page } from "@playwright/test";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const SINGLE_WASM_BINARY_RELEASE = fileURLToPath(new URL("../../web/src/wasm/pkg-single/aero_wasm_bg.wasm", import.meta.url));
const SINGLE_WASM_JS_RELEASE = fileURLToPath(new URL("../../web/src/wasm/pkg-single/aero_wasm.js", import.meta.url));
const SINGLE_WASM_BINARY_DEV = fileURLToPath(new URL("../../web/src/wasm/pkg-single-dev/aero_wasm_bg.wasm", import.meta.url));
const SINGLE_WASM_JS_DEV = fileURLToPath(new URL("../../web/src/wasm/pkg-single-dev/aero_wasm.js", import.meta.url));

const THREADED_WASM_BINARY_RELEASE = fileURLToPath(
  new URL("../../web/src/wasm/pkg-threaded/aero_wasm_bg.wasm", import.meta.url),
);
const THREADED_WASM_JS_RELEASE = fileURLToPath(new URL("../../web/src/wasm/pkg-threaded/aero_wasm.js", import.meta.url));
const THREADED_WASM_BINARY_DEV = fileURLToPath(
  new URL("../../web/src/wasm/pkg-threaded-dev/aero_wasm_bg.wasm", import.meta.url),
);
const THREADED_WASM_JS_DEV = fileURLToPath(new URL("../../web/src/wasm/pkg-threaded-dev/aero_wasm.js", import.meta.url));

const HAS_WASM_BUNDLE =
  (existsSync(SINGLE_WASM_BINARY_RELEASE) && existsSync(SINGLE_WASM_JS_RELEASE)) ||
  (existsSync(SINGLE_WASM_BINARY_DEV) && existsSync(SINGLE_WASM_JS_DEV)) ||
  (existsSync(THREADED_WASM_BINARY_RELEASE) && existsSync(THREADED_WASM_JS_RELEASE)) ||
  (existsSync(THREADED_WASM_BINARY_DEV) && existsSync(THREADED_WASM_JS_DEV));

async function waitForMachineWorkerPanelReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroMachineWorkerPanelTest?.ready === true);
}

test("canonical Machine worker panel: renders VGA scanout to a canvas", async ({ page }) => {
  test.setTimeout(90_000);
  page.setDefaultTimeout(90_000);

  if (!HAS_WASM_BUNDLE) {
    const message = [
      "WASM package missing (required for canonical Machine worker VGA demo).",
      "",
      "Expected one of:",
      `- ${SINGLE_WASM_BINARY_RELEASE} (+ ${SINGLE_WASM_JS_RELEASE})`,
      `- ${SINGLE_WASM_BINARY_DEV} (+ ${SINGLE_WASM_JS_DEV})`,
      `- ${THREADED_WASM_BINARY_RELEASE} (+ ${THREADED_WASM_JS_RELEASE})`,
      `- ${THREADED_WASM_BINARY_DEV} (+ ${THREADED_WASM_JS_DEV})`,
      "",
      "Build it with (from the repo root):",
      "  npm -w web run wasm:build",
    ].join("\n");
    if (process.env.CI) {
      throw new Error(message);
    }
    test.skip(true, message);
  }

  await page.goto("/web/index.html", { waitUntil: "load" });
  await waitForMachineWorkerPanelReady(page);

  await page.click("#canonical-machine-vga-worker-start");

  await page.waitForFunction(() => (window as any).__aeroMachineWorkerPanelTest?.framesPresented > 0);

  const state = await page.evaluate(() => (window as any).__aeroMachineWorkerPanelTest);
  expect(state).toBeTruthy();
  if (!state || typeof state !== "object") {
    throw new Error("__aeroMachineWorkerPanelTest missing");
  }
  if ((state as any).error) {
    throw new Error(String((state as any).error));
  }

  const sample = await page.evaluate(() => {
    const canvas = document.getElementById("canonical-machine-vga-worker-canvas") as HTMLCanvasElement | null;
    if (!canvas) throw new Error("canonical machine worker canvas missing");
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("canonical machine worker canvas context missing");

    const w = Math.min(16, canvas.width);
    const h = Math.min(16, canvas.height);
    const data = ctx.getImageData(0, 0, w, h).data;
    let nonBlack = 0;
    for (let i = 0; i < data.length; i += 4) {
      if (data[i] !== 0 || data[i + 1] !== 0 || data[i + 2] !== 0) {
        nonBlack += 1;
        break;
      }
    }
    return { width: canvas.width, height: canvas.height, nonBlack };
  });

  expect(sample.width).toBeGreaterThan(0);
  expect(sample.height).toBeGreaterThan(0);
  expect(sample.nonBlack).toBeGreaterThan(0);

  await page.click("#canonical-machine-vga-worker-stop");
});
