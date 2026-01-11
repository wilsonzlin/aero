import { perf } from "../perf/perf";
import type { WasmVariant } from "./wasm_loader";

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

export async function precompileWasm(variant: WasmVariant): Promise<PrecompiledWasm> {
  const existing = precompilePromises[variant];
  if (existing) return existing;

  const promise = (async () => {
    const url = await resolveWasmBinaryUrl(variant);
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`Failed to fetch WASM binary (${variant}): ${response.status} ${response.statusText}`);
    }

    const module = await perf.spanAsync("wasm:compile", async () => {
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

