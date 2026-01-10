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

