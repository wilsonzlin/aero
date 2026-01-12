import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { startDiskImageServer, type DiskImageServer } from "../fixtures/servers";

const BOOT_IMAGE_BYTES = readFileSync(
  fileURLToPath(new URL("../fixtures/boot/boot_vga_serial_8s.img", import.meta.url)),
);

const THREADED_WASM_BINARY = fileURLToPath(
  new URL("../../web/src/wasm/pkg-threaded/aero_wasm_bg.wasm", import.meta.url),
);
const HAS_THREADED_WASM_BINARY = existsSync(THREADED_WASM_BINARY);

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("vm boot: boots deterministic boot sector end-to-end (WASM VM + IO worker + GPU worker)", async ({ page, browserName }) => {
  if (!HAS_THREADED_WASM_BINARY) {
    const message = [
      "Threaded WASM package missing (required for shared-memory worker runtime).",
      "",
      `Expected: ${THREADED_WASM_BINARY}`,
      "",
      "Build it with (from the repo root):",
      "  npm -w web run wasm:build",
    ].join("\n");
    if (process.env.CI) {
      throw new Error(message);
    }
    test.skip(true, message);
  }

  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  const server: DiskImageServer = await startDiskImageServer({ data: BOOT_IMAGE_BYTES, enableCors: true });
  try {
    const url = new URL("http://127.0.0.1:5173/web/vm-boot-vga-serial-smoke.html");
    url.searchParams.set("diskUrl", server.url("/disk.img"));
    await page.goto(url.toString(), { waitUntil: "load" });

    // Only probe for SAB/WASM threads after navigating to the harness page; the
    // initial Playwright page is `about:blank` and is not cross-origin isolated.
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

    test.skip(
      !support.crossOriginIsolated || !support.sharedArrayBuffer,
      "SharedArrayBuffer requires COOP/COEP headers.",
    );
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
    expect(String((result as any).serial ?? "")).toContain("AERO!");

    const expectedVga = [
      [65, 31, 0, 255], // A
      [69, 31, 0, 255], // E
      [82, 31, 0, 255], // R
      [79, 31, 0, 255], // O
      [33, 31, 0, 255], // !
    ];
    expect((result as any).samples?.vgaPixels).toEqual(expectedVga);

    const metrics = (result as any).metrics;
    expect(metrics?.framesPresented ?? 0).toBeGreaterThan(0);
  } finally {
    await server.close();
  }
});
