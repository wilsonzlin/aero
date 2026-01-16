import { perf } from "../perf/perf";
import type { WasmVariant } from "./wasm_loader";
import { registerPrecompiledWasmModule } from "./wasm_precompiled_registry";
import { formatOneLineError } from "../text";

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

function stripQueryAndHash(url: string): string {
  const q = url.indexOf("?");
  const h = url.indexOf("#");
  const end = Math.min(q >= 0 ? q : url.length, h >= 0 ? h : url.length);
  return end === url.length ? url : url.slice(0, end);
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
      //
      // `data:` URLs are valid fetch targets (and are used by some bundlers). Avoid treating them
      // as filesystem paths.
      if (isNodeEnv() && !/^https?:/i.test(url) && !/^data:/i.test(url)) {
        // Keep the dynamic imports opaque to Vite/Rollup so browser builds don't try to resolve Node builtins.
        const fsPromises = "node:fs/promises";
        const nodeUrl = "node:url";
        const { readFile } = await import(/* @vite-ignore */ fsPromises);
        const { fileURLToPath, pathToFileURL } = await import(/* @vite-ignore */ nodeUrl);

        const candidates: URL[] = [];
        const pushCandidate = (candidate: URL): void => {
          // `fileURLToPath` rejects query strings/hashes. Defensive: strip them if present.
          candidate.search = "";
          candidate.hash = "";
          candidates.push(candidate);
        };

        if (url.startsWith("file:")) {
          pushCandidate(new URL(url));
        } else {
          const stripped = stripQueryAndHash(url);
          // Vite dev server can emit absolute filesystem URLs as `/@fs/<abs-path>`.
          // Prefer using it when available, but fall back to the build output path if it doesn't exist.
          if (stripped.startsWith("/@fs/")) {
            // Strip the `/@fs` prefix but preserve the leading `/` of the absolute path.
            pushCandidate(pathToFileURL(stripped.slice("/@fs".length)));
          } else if (stripped.startsWith("/")) {
            // Best-effort: interpret as an absolute filesystem path. This commonly fails for
            // dev-server paths like `/web/src/...`, so we keep a fallback below.
            pushCandidate(pathToFileURL(stripped));
          } else {
            // Interpret as a relative path to this module (most robust in Vitest/Node).
            pushCandidate(new URL(stripped, import.meta.url));
          }
        }

        // Fallback: load from the expected source-tree output path. This handles Vite-generated
        // dev-server paths that are not directly readable from the filesystem (e.g. `/web/src/...`).
        pushCandidate(new URL(WASM_BINARY_PATH[variant], import.meta.url));

        let lastErr: unknown;
        for (const candidate of candidates) {
          try {
            const bytes = await readFile(fileURLToPath(candidate));
            return await WebAssembly.compile(bytes);
          } catch (err) {
            lastErr = err;
          }
        }

        throw lastErr instanceof Error ? lastErr : new Error(formatOneLineError(lastErr, 512));
      }

      const response = await fetch(url);
      if (!response.ok) {
        // Avoid reflecting server-controlled HTTP reason phrases (statusText).
        throw new Error(`Failed to fetch WASM binary (${variant}) (${response.status})`);
      }

      if (typeof WebAssembly.compileStreaming === "function") {
        try {
          return await WebAssembly.compileStreaming(response.clone());
        } catch (err) {
          // `compileStreaming` can fail if the server sends the wrong MIME type, or if CSP
          // blocks streaming compilation. Fall back to `arrayBuffer()` + `compile()`.
          console.warn(
            `[wasm] compileStreaming failed for ${variant}; falling back to compile(). Error: ${
              formatOneLineError(err, 512)
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
