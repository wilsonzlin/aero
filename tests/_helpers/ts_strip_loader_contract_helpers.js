/**
 * Small shared helpers for TS-strip loader contract tests.
 *
 * These contracts focus on copy/paste surfaces (docs, workflows, package scripts):
 * if we run TypeScript entrypoints under Node's `--experimental-strip-types`, we
 * must also register our TS-strip loader via `--import ...register-ts-strip-loader.mjs`
 * or `.js` → `.ts` import remapping won't be active.
 */

/**
 * Join shell-style `\` newline continuations so `node ... \` invocations become a single line.
 *
 * @param {string} text
 * @returns {string}
 */
export function normalizeShellLineContinuations(text) {
  return text.replace(/\\\r?\n[ \t]*/gu, " ");
}

/**
 * Heuristic: treat a command as “running TS under strip-types” if it contains the
 * strip-types flag and references a `.ts` path/glob token somewhere.
 *
 * This stays intentionally simple; false positives are acceptable because they
 * encourage the canonical `--import` loader registration for TS execution.
 *
 * @param {string} cmd
 * @returns {boolean}
 */
export function commandRunsTsWithStripTypes(cmd) {
  return cmd.includes("--experimental-strip-types") && cmd.includes(".ts");
}

/**
 * @param {string} cmd
 * @param {{ requiredPathFragment?: string }=} opts
 * @returns {boolean}
 */
export function commandHasTsStripLoaderImport(cmd, opts = {}) {
  if (!cmd.includes("--import")) return false;
  if (typeof opts.requiredPathFragment === "string") {
    return cmd.includes(opts.requiredPathFragment);
  }
  return cmd.includes("register-ts-strip-loader.mjs");
}

