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

async function waitForMachinePanelReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroMachinePanelTest?.ready === true);
}

test("canonical Machine panel: VBE mode switch (1280x720x32) updates scanout dimensions", async ({ page }) => {
  test.setTimeout(90_000);
  page.setDefaultTimeout(90_000);

  if (!HAS_WASM_BUNDLE) {
    const message = [
      "WASM package missing (required for canonical Machine panel demo).",
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

  await page.goto("/web/index.html?machineVbe=1280x720", { waitUntil: "load" });
  await waitForMachinePanelReady(page);

  const state = await page.evaluate(() => (window as any).__aeroMachinePanelTest);
  expect(state).toBeTruthy();
  if (!state || typeof state !== "object") {
    throw new Error("__aeroMachinePanelTest missing");
  }
  if ((state as any).error) {
    throw new Error(String((state as any).error));
  }
  test.skip(!(state as any).vgaSupported, "Machine VGA scanout exports unavailable in this WASM build.");

  await page.waitForFunction(() => {
    const st = (window as any).__aeroMachinePanelTest;
    return st?.framesPresented > 0 && st?.width === 1280 && st?.height === 720;
  });

  const pixel = await page.evaluate(() => {
    const canvas = document.getElementById("canonical-machine-vga-canvas") as HTMLCanvasElement | null;
    if (!canvas) throw new Error("canonical machine canvas missing");
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("canonical machine canvas context missing");
    const data = ctx.getImageData(0, 0, 1, 1).data;
    return [data[0] ?? 0, data[1] ?? 0, data[2] ?? 0, data[3] ?? 0];
  });

  // The VBE boot sector writes a single red pixel at (0,0).
  expect(pixel).toEqual([255, 0, 0, 255]);
});

