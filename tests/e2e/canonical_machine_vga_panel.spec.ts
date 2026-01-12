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

test("canonical Machine panel: renders VGA scanout to a canvas", async ({ page }) => {
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

  await page.goto("/web/index.html", { waitUntil: "load" });
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

  await page.waitForFunction(() => (window as any).__aeroMachinePanelTest?.framesPresented > 0);

  const vgaMeta = await page.evaluate(() => (window as any).__aeroMachinePanelTest);
  expect(vgaMeta).toBeTruthy();
  if (vgaMeta && typeof vgaMeta === "object") {
    // Transport telemetry is best-effort (older builds may not expose it), but when
    // present it should indicate a concrete render path once frames are flowing.
    const transport = (vgaMeta as any).transport;
    if (transport !== undefined) {
      expect(transport === "ptr" || transport === "copy").toBe(true);
    }
    const width = (vgaMeta as any).width;
    const height = (vgaMeta as any).height;
    const strideBytes = (vgaMeta as any).strideBytes;
    if (typeof width === "number" && typeof height === "number") {
      expect(width).toBeGreaterThan(0);
      expect(height).toBeGreaterThan(0);
    }
    if (typeof strideBytes === "number") {
      expect(strideBytes).toBeGreaterThan(0);
    }
  }

  const sample = await page.evaluate(() => {
    const canvas = document.getElementById("canonical-machine-vga-canvas") as HTMLCanvasElement | null;
    if (!canvas) throw new Error("canonical machine canvas missing");
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("canonical machine canvas context missing");

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

  // Optional shared-framebuffer mirroring: when SharedArrayBuffer is available,
  // the machine panel also publishes frames into `__aeroMachineVgaFramebuffer`
  // using the stable framebuffer protocol header.
  const shared = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const sab = (globalThis as any).__aeroMachineVgaFramebuffer as SharedArrayBuffer | undefined;
    if (typeof SharedArrayBuffer === "undefined") return null;
    if (!(sab instanceof SharedArrayBuffer)) return null;

    const HEADER_I32_COUNT = 8;
    const HEADER_BYTES = HEADER_I32_COUNT * 4;

    const header = new Int32Array(sab, 0, HEADER_I32_COUNT);
    const load = (index: number) => {
      if (typeof Atomics !== "undefined") return Atomics.load(header, index);
      return header[index];
    };

    const width = load(2);
    const height = load(3);
    const strideBytes = load(4);
    const frame = load(6);

    const pixelsLen = Math.min(Math.max(0, sab.byteLength - HEADER_BYTES), Math.max(0, strideBytes) * Math.max(0, height));
    const pixels = new Uint8Array(sab, HEADER_BYTES, pixelsLen);

    const sampleW = Math.max(0, Math.min(16, width));
    const sampleH = Math.max(0, Math.min(16, height));
    let nonBlack = false;
    for (let y = 0; y < sampleH && !nonBlack; y++) {
      for (let x = 0; x < sampleW && !nonBlack; x++) {
        const off = y * strideBytes + x * 4;
        if (off + 2 >= pixels.length) continue;
        const r = pixels[off] ?? 0;
        const g = pixels[off + 1] ?? 0;
        const b = pixels[off + 2] ?? 0;
        if (r !== 0 || g !== 0 || b !== 0) {
          nonBlack = true;
        }
      }
    }

    return { width, height, strideBytes, frame, nonBlack };
  });

  if (shared) {
    expect(shared.width).toBeGreaterThan(0);
    expect(shared.height).toBeGreaterThan(0);
    expect(shared.strideBytes).toBeGreaterThan(0);
    expect(shared.frame).toBeGreaterThan(0);
    expect(shared.nonBlack).toBe(true);
  }
});
