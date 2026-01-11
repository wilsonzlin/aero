// Minimal ESM loader used for running the TypeScript codebase with Node's
// `--experimental-strip-types` flag (no third-party runtime like `tsx`).
//
// The codebase is written using NodeNext-style `.js` import specifiers
// (e.g. `import './server.js'` from `server.ts`) so that `tsc` emits valid JS.
//
// When running the `.ts` sources directly, Node's resolver will fail to find
// those `.js` files. This loader transparently falls back to `.ts` when a
// relative `.js` specifier can't be resolved.
//
// This intentionally only remaps relative `.js` specifiers. Anything else
// (node: builtins, bare specifiers, absolute URLs) is delegated unchanged.

export async function resolve(specifier, context, nextResolve) {
  if ((specifier.startsWith('./') || specifier.startsWith('../')) && specifier.endsWith('.js')) {
    try {
      return await nextResolve(specifier, context);
    } catch (err) {
      const tsSpecifier = `${specifier.slice(0, -3)}.ts`;
      return nextResolve(tsSpecifier, context);
    }
  }

  return nextResolve(specifier, context);
}

export async function load(url, context, nextLoad) {
  // Vite uses query-string imports like `?worker&url` / `?url` to turn an asset
  // path into a URL string default export. When running the TypeScript sources
  // directly under Node (with `--experimental-strip-types`), those query strings
  // reach the native loader, which will load the underlying file as an ES module
  // (and therefore not have a default export).
  //
  // For unit tests, we only need a stable string (the URL isn't actually fetched),
  // so synthesize a minimal module that default-exports the resolved file URL.
  const u = new URL(url);
  if (u.protocol === "file:" && u.searchParams.has("url")) {
    // Some "worker module" sources provide their own `default export` (and other
    // named exports) so they can be imported directly in Node-based unit tests.
    // Don't short-circuit those module loads; only synthesize `?url` for non-module
    // assets.
    const path = u.pathname.toLowerCase();
    if (path.endsWith(".js") || path.endsWith(".ts") || path.endsWith(".mjs") || path.endsWith(".cjs")) {
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
