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

test("canonical Machine worker panel: grows shared framebuffer when guest switches to a larger VBE mode", async ({ page }) => {
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

  // Force the worker demo to program a larger-than-default VBE mode so the worker must grow the
  // shared framebuffer (initial allocation is sized for 1024x768).
  await page.goto("/web/index.html?machineWorkerVbe=1280x720", { waitUntil: "load" });
  await waitForMachineWorkerPanelReady(page);

  const support = await page.evaluate(() => ({
    sharedArrayBuffer: typeof SharedArrayBuffer !== "undefined",
    crossOriginIsolated: globalThis.crossOriginIsolated === true,
  }));
  test.skip(!support.sharedArrayBuffer || !support.crossOriginIsolated, "SharedArrayBuffer (COOP/COEP) required to cover shared framebuffer growth.");

  await page.click("#canonical-machine-vga-worker-start");

  // Wait for at least one frame and for the VBE mode dimensions to be observed by the UI.
  await page.waitForFunction(() => {
    const st = (window as any).__aeroMachineWorkerPanelTest;
    return st?.framesPresented > 0 && st?.width === 1280 && st?.height === 720;
  });

  const state = await page.evaluate(() => (window as any).__aeroMachineWorkerPanelTest);
  expect(state).toBeTruthy();
  if (!state || typeof state !== "object") {
    throw new Error("__aeroMachineWorkerPanelTest missing");
  }
  if ((state as any).error) {
    throw new Error(String((state as any).error));
  }

  expect((state as any).width).toBe(1280);
  expect((state as any).height).toBe(720);
  expect((state as any).transport).toBe("shared");
  expect((state as any).framesPresented).toBeGreaterThan(0);

  const sample = await page.evaluate(() => {
    const canvas = document.getElementById("canonical-machine-vga-worker-canvas") as HTMLCanvasElement | null;
    if (!canvas) throw new Error("canonical machine worker canvas missing");
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("canonical machine worker canvas context missing");

    const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
    for (let i = 0; i < data.length; i += 4) {
      if (data[i] !== 0 || data[i + 1] !== 0 || data[i + 2] !== 0) {
        return true;
      }
    }
    return false;
  });
  expect(sample).toBe(true);

  await page.click("#canonical-machine-vga-worker-stop");
});
