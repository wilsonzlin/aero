import type { Page } from "@playwright/test";
import { fileURLToPath } from "node:url";

const threadedWasmBundleCache = new Map<string, Promise<boolean>>();

const THREADED_WASM_BINARY_RELEASE = fileURLToPath(
  new URL("../../../web/src/wasm/pkg-threaded/aero_wasm_bg.wasm", import.meta.url),
);
const THREADED_WASM_JS_RELEASE = fileURLToPath(new URL("../../../web/src/wasm/pkg-threaded/aero_wasm.js", import.meta.url));
const THREADED_WASM_BINARY_DEV = fileURLToPath(
  new URL("../../../web/src/wasm/pkg-threaded-dev/aero_wasm_bg.wasm", import.meta.url),
);
const THREADED_WASM_JS_DEV = fileURLToPath(new URL("../../../web/src/wasm/pkg-threaded-dev/aero_wasm.js", import.meta.url));

function threadedWasmMissingMessage(): string {
  return [
    "threaded WASM bundle (pkg-threaded) is missing",
    "",
    "Expected one of:",
    `- ${THREADED_WASM_BINARY_RELEASE} (+ ${THREADED_WASM_JS_RELEASE})`,
    `- ${THREADED_WASM_BINARY_DEV} (+ ${THREADED_WASM_JS_DEV})`,
    "",
    "Build it with (from the repo root):",
    "  npm -w web run wasm:build",
  ].join("\n");
}

function resolvePageOrigin(page: Page): string {
  try {
    const url = new URL(page.url());
    if (url.origin && url.origin !== "null") return url.origin;
  } catch {
    // ignore
  }
  return process.env.AERO_PLAYWRIGHT_DEV_ORIGIN ?? "http://127.0.0.1:5173";
}

export async function hasThreadedWasmBundle(page: Page): Promise<boolean> {
  const baseUrl = resolvePageOrigin(page);
  const cached = threadedWasmBundleCache.get(baseUrl);
  if (cached) return await cached;

  const checkPromise = (async (): Promise<boolean> => {
    // `wasm_loader.ts` fetches these paths when instantiating the threaded build.
    const check = async (wasmPath: string, jsPath: string): Promise<boolean> => {
      try {
        const wasm = await page.request.get(new URL(wasmPath, baseUrl).toString());
        if (!wasm.ok()) return false;
        // Vite's dev server can return `index.html` for missing assets (200 + text/html). Treat that as missing.
        const wasmContentType = wasm.headers()["content-type"] ?? "";
        if (wasmContentType.includes("text/html")) return false;
        const js = await page.request.get(new URL(jsPath, baseUrl).toString());
        if (!js.ok()) return false;
        const jsContentType = js.headers()["content-type"] ?? "";
        if (jsContentType.includes("text/html")) return false;
        return true;
      } catch {
        return false;
      }
    };

    if (await check("/web/src/wasm/pkg-threaded/aero_wasm_bg.wasm", "/web/src/wasm/pkg-threaded/aero_wasm.js")) {
      return true;
    }
    return await check("/web/src/wasm/pkg-threaded-dev/aero_wasm_bg.wasm", "/web/src/wasm/pkg-threaded-dev/aero_wasm.js");
  })();

  // Cache success. If the bundle is missing (or the dev server isn't ready), allow later tests to retry.
  threadedWasmBundleCache.set(baseUrl, checkPromise);
  const ok = await checkPromise;
  if (!ok) threadedWasmBundleCache.delete(baseUrl);
  return ok;
}

export async function checkThreadedWasmBundle(page: Page): Promise<{ ok: boolean; message: string }> {
  const ok = await hasThreadedWasmBundle(page);
  return { ok, message: ok ? "" : threadedWasmMissingMessage() };
}
