import { perf } from "../perf/perf";
import type { WasmVariant } from "./wasm_loader";
import { registerPrecompiledWasmModule } from "./wasm_precompiled_registry";

export type PrecompiledWasm = { module: WebAssembly.Module; url: string };

// `wasm-pack` outputs into `web/src/wasm/pkg-single` and `web/src/wasm/pkg-threaded`.
// These directories are generated (see `web/scripts/build_wasm.mjs`) and are not
// necessarily present in a fresh checkout.
//
// Use `import.meta.glob` so Vite builds/tests don't fail when the generated output is missing.
const wasmBinaryImporters = import.meta.glob("../wasm/pkg-*/aero_wasm_bg.wasm", {
  // Convert to a URL string so we can `fetch()` it explicitly.
  query: "?url",
  import: "default",
});

const WASM_BINARY_PATH: Record<WasmVariant, string> = {
  single: "../wasm/pkg-single/aero_wasm_bg.wasm",
  threaded: "../wasm/pkg-threaded/aero_wasm_bg.wasm",
};

declare global {
  // eslint-disable-next-line no-var
  var __aeroWasmBinaryUrlOverride: Partial<Record<WasmVariant, string>> | undefined;
}

async function resolveWasmBinaryUrl(variant: WasmVariant): Promise<string> {
  const override = globalThis.__aeroWasmBinaryUrlOverride?.[variant];
  if (override) return override;

  const importer = wasmBinaryImporters[WASM_BINARY_PATH[variant]];
  if (!importer) {
    throw new Error(
      [
        `Missing ${variant} WASM binary.`,
        "",
        "Build it with:",
        "  cd web",
        `  npm run wasm:build:${variant}`,
        "",
        "Or build both variants:",
        "  npm run wasm:build",
      ].join("\n"),
    );
  }

  // `import.meta.glob(..., { query: '?url', import: 'default' })` yields the default export directly.
  return (await (importer as () => Promise<unknown>)()) as string;
}

const precompilePromises: Partial<Record<WasmVariant, Promise<PrecompiledWasm>>> = {};

function isNodeEnv(): boolean {
  // Avoid referencing `process` directly so this module can be imported in browser builds without polyfills.
  const p = (globalThis as unknown as { process?: unknown }).process as { versions?: { node?: unknown } } | undefined;
  return typeof p?.versions?.node === "string";
}

export async function precompileWasm(variant: WasmVariant): Promise<PrecompiledWasm> {
  const existing = precompilePromises[variant];
  if (existing) return existing;

  const promise = (async () => {
    const url = await resolveWasmBinaryUrl(variant);
    const module = await perf.spanAsync("wasm:compile", async () => {
      // In Node (Vitest), the Vite `?url` import typically yields a dev-server style path
      // (`/web/src/...`) which `fetch()` cannot resolve (no base URL). Prefer reading the
      // bytes from disk.
      //
      // Note: this intentionally bypasses `compileStreaming` in Node since we are not
      // working with a real HTTP Response.
      if (isNodeEnv() && !/^https?:/i.test(url)) {
        // Keep the dynamic imports opaque to Vite/Rollup so browser builds don't try to resolve Node builtins.
        const fsPromises = "node:fs/promises";
        const nodeUrl = "node:url";
        const { readFile } = await import(/* @vite-ignore */ fsPromises);
        const { fileURLToPath } = await import(/* @vite-ignore */ nodeUrl);

        const fileUrl = url.startsWith("file:") ? new URL(url) : new URL(WASM_BINARY_PATH[variant], import.meta.url);
        const bytes = await readFile(fileURLToPath(fileUrl));
        return await WebAssembly.compile(bytes);
      }

      const response = await fetch(url);
      if (!response.ok) {
        throw new Error(`Failed to fetch WASM binary (${variant}): ${response.status} ${response.statusText}`);
      }

      if (typeof WebAssembly.compileStreaming === "function") {
        try {
          return await WebAssembly.compileStreaming(response.clone());
        } catch (err) {
          // `compileStreaming` can fail if the server sends the wrong MIME type, or if CSP
          // blocks streaming compilation. Fall back to `arrayBuffer()` + `compile()`.
          console.warn(
            `[wasm] compileStreaming failed for ${variant}; falling back to compile(). Error: ${
              err instanceof Error ? err.message : String(err)
            }`,
          );
        }
      }

      const bytes = await response.arrayBuffer();
      return await WebAssembly.compile(bytes);
    });

    registerPrecompiledWasmModule(module, variant);
    return { module, url } satisfies PrecompiledWasm;
  })();

  precompilePromises[variant] = promise;

  try {
    return await promise;
  } catch (err) {
    // Allow retries if compilation fails (e.g. CSP disables WASM compilation, dev server hiccup).
    delete precompilePromises[variant];
    throw err;
  }
}
