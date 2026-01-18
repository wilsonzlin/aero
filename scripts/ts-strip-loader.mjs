// Minimal ESM loader used for running the TypeScript codebase with Node's
// `--experimental-strip-types` flag (no third-party runtime like `tsx`).
//
// This loader is intended to be registered via:
//
//   node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs <entry>.ts
//
// (Node's legacy `--loader` flag is deprecated; prefer `--import` + `register()`.)
//
// The codebase is written using NodeNext-style `.js` import specifiers
// (e.g. `import './server.js'` from `server.ts`) so that `tsc` emits valid JS.
//
// When running the `.ts` sources directly, Node's resolver will fail to find
// those `.js` files. This loader transparently falls back to `.ts` when a
// relative `.js` specifier can't be resolved.
//
// This loader is intentionally conservative: it only rewrites relative specifiers
// (NodeNext-style `.js` -> `.ts`, and extensionless -> `.ts`/`index.ts`), plus a
// couple of tiny shims (`ws`, `ipaddr.js`) for offline unit tests. Everything
// else (node: builtins, bare specifiers, absolute URLs) is delegated unchanged.

import { readFile } from "node:fs/promises";

export async function resolve(specifier, context, nextResolve) {
  if (specifier === "ws") {
    try {
      return await nextResolve(specifier, context);
    } catch (err) {
      // The repo's unit tests run offline (no `node_modules/`). Prefer the real
      // `ws` package when available, but fall back to a tiny built-in shim that
      // implements the subset of the API we rely on in tests (WebSocket + Server).
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
      // Same motivation as the `ws` shim: keep Node unit tests runnable in an
      // offline environment without `node_modules/`.
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
    //
    // - ERR_MODULE_NOT_FOUND: typical missing file case (e.g. `./foo.js` when
    //   only `foo.ts` exists).
    // - ERR_UNSUPPORTED_DIR_IMPORT: extensionless directory import (e.g. `./foo`
    //   where `foo/index.ts` exists).
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
    if (pathPart.endsWith('.js')) {
      fallbackSpecifiers.push(`${pathPart.slice(0, -3)}.ts${suffix}`);
    }

    // 2) Allow extensionless relative imports (common in Vite/tsconfig "Bundler" mode)
    // by falling back to `.ts` and `index.ts`.
    const basename = pathPart.split('/').pop() ?? '';
    const hasExtension = basename.includes('.');
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
  // Vite uses query-string imports like `?worker&url` / `?url` to turn an asset
  // path into a URL string default export. When running the TypeScript sources
  // directly under Node (with `--experimental-strip-types`), those query strings
  // reach the native loader, which will load the underlying file as an ES module.
  //
  // For unit tests, we only need a stable string (the URL isn't actually fetched),
  // so synthesize a minimal module that default-exports the resolved file URL.
  //
  // Some "worker module" sources provide their own `default export` (and other
  // named exports) so they can be imported directly in Node-based unit tests.
  // Don't short-circuit those module loads; only synthesize `?url` for non-module
  // assets.
  const u = new URL(url);
  if (u.protocol === "file:" && u.searchParams.has("raw")) {
    const base = new URL(url);
    base.search = "";
    base.hash = "";
    const text = await readFile(base, "utf8");
    return {
      format: "module",
      source: `export default ${JSON.stringify(text)};\n`,
      shortCircuit: true,
    };
  }
  if (u.protocol === "file:" && u.searchParams.has("url")) {
    const path = u.pathname.toLowerCase();
    if (
      path.endsWith(".js") ||
      path.endsWith(".ts") ||
      path.endsWith(".mjs") ||
      path.endsWith(".mts") ||
      path.endsWith(".cjs")
    ) {
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

  return nextLoad(url, context);
}
