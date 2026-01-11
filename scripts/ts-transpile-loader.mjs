// ESM loader used for executing this repo's TypeScript sources directly under
// Node.js without relying on `--experimental-strip-types`.
//
// Unlike `scripts/ts-strip-loader.mjs`, this loader *transpiles* `.ts` modules
// to JavaScript using the TypeScript compiler. This supports TypeScript syntax
// that requires emit (e.g. `enum`, parameter properties) which Node's strip-only
// mode intentionally does not handle.
//
// This exists primarily for worker_threads tests that want to execute production
// worker entrypoints (which are authored in TypeScript and include enums) inside
// Node.

import fs from "node:fs/promises";
import { fileURLToPath } from "node:url";

import * as ts from "typescript";

const COMPILER_OPTIONS = {
  module: ts.ModuleKind.ESNext,
  target: ts.ScriptTarget.ES2022,
  // Keep source maps inline to preserve useful stack traces in unit tests.
  inlineSourceMap: true,
  inlineSources: true,
};

export async function resolve(specifier, context, nextResolve) {
  if (specifier === "ws") {
    try {
      return await nextResolve(specifier, context);
    } catch (err) {
      // The repo's unit tests can run in an offline environment without
      // `node_modules/`. Prefer the real `ws` package when available, but fall
      // back to a tiny built-in shim that implements the subset of the API we
      // rely on in tests (WebSocket + Server).
      if (err && typeof err === "object" && "code" in err && err.code !== "ERR_MODULE_NOT_FOUND") {
        throw err;
      }
      return nextResolve(new URL("./ws-shim.mjs", import.meta.url).href, context);
    }
  }

  if (specifier === "ipaddr.js") {
    try {
      return await nextResolve(specifier, context);
    } catch (err) {
      if (err && typeof err === "object" && "code" in err && err.code !== "ERR_MODULE_NOT_FOUND") {
        throw err;
      }
      return nextResolve(new URL("./ipaddr-shim.mjs", import.meta.url).href, context);
    }
  }

  const isRelative = specifier.startsWith("./") || specifier.startsWith("../");
  if (!isRelative) {
    return nextResolve(specifier, context);
  }

  try {
    return await nextResolve(specifier, context);
  } catch (err) {
    // Only try fallbacks when the specifier fails to resolve because it does
    // not exist as-written (common when running TS sources directly).
    if (err && typeof err === "object" && "code" in err) {
      const code = err.code;
      if (code !== "ERR_MODULE_NOT_FOUND" && code !== "ERR_UNSUPPORTED_DIR_IMPORT") {
        throw err;
      }
    }

    // Preserve `?query` / `#hash` suffixes when rewriting specifiers.
    const queryIdx = specifier.indexOf("?");
    const hashIdx = specifier.indexOf("#");
    const cut = Math.min(
      queryIdx === -1 ? Number.POSITIVE_INFINITY : queryIdx,
      hashIdx === -1 ? Number.POSITIVE_INFINITY : hashIdx,
    );
    const pathPart = specifier.slice(0, cut === Number.POSITIVE_INFINITY ? specifier.length : cut);
    const suffix = specifier.slice(pathPart.length);

    const fallbackSpecifiers = [];

    // 1) Remap NodeNext-style `.js` specifiers to `.ts` sources.
    if (pathPart.endsWith(".js")) {
      fallbackSpecifiers.push(`${pathPart.slice(0, -3)}.ts${suffix}`);
    }

    // 2) Allow extensionless relative imports by falling back to `.ts` and `index.ts`.
    const basename = pathPart.split("/").pop() ?? "";
    const hasExtension = basename.includes(".");
    if (!hasExtension) {
      fallbackSpecifiers.push(`${pathPart}.ts${suffix}`);
      fallbackSpecifiers.push(`${pathPart}/index.ts${suffix}`);
    }

    for (const fallback of fallbackSpecifiers) {
      try {
        return await nextResolve(fallback, context);
      } catch {
        // continue
      }
    }

    throw err;
  }
}

export async function load(url, context, nextLoad) {
  // Preserve the `?url` behavior used by Vite in unit tests. See the comment in
  // `scripts/ts-strip-loader.mjs` for rationale.
  const u = new URL(url);
  if (u.protocol === "file:" && u.searchParams.has("url")) {
    const path = u.pathname.toLowerCase();
    if (path.endsWith(".js") || path.endsWith(".ts") || path.endsWith(".mjs") || path.endsWith(".mts") || path.endsWith(".cjs")) {
      return nextLoad(url, context);
    }
    const base = new URL(url);
    base.search = "";
    base.hash = "";
    return {
      format: "module",
      source: `export default ${JSON.stringify(base.href)};\n`,
      shortCircuit: true,
    };
  }

  if (u.protocol === "file:" && u.pathname.endsWith(".ts")) {
    const filename = fileURLToPath(u);
    const sourceText = await fs.readFile(filename, "utf8");
    const transpiled = ts.transpileModule(sourceText, {
      compilerOptions: COMPILER_OPTIONS,
      fileName: filename,
      reportDiagnostics: false,
    });
    return {
      format: "module",
      source: transpiled.outputText,
      shortCircuit: true,
    };
  }

  return nextLoad(url, context);
}
