import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";
import { createRequire } from "node:module";

import { listFilesRecursive } from "./_helpers/fs_walk.js";

function parseExportedFunctionNamesFromDts(dtsSource) {
  /** @type {string[]} */
  const names = [];
  const re = /^export function\s+([A-Za-z0-9_]+)\s*\(/gm;
  for (;;) {
    const match = re.exec(dtsSource);
    if (!match) break;
    names.push(match[1]);
  }
  return names;
}

test("d.ts exports: declared functions exist at runtime (ESM/CJS)", async () => {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const root = path.resolve(here, "..");
  const srcDir = path.resolve(root, "src");

  const relFiles = await listFilesRecursive(srcDir);
  const relFileSet = new Set(relFiles);

  const cjsDts = relFiles.filter((rel) => rel.endsWith(".cjs.d.ts")).sort();
  assert.ok(cjsDts.length > 0, "Expected at least one src/**/*.cjs.d.ts stub");

  const require = createRequire(import.meta.url);

  // Dual-module stubs: require both runtime formats to match the stub surface.
  for (const cjsDtsRel of cjsDts) {
    const baseRel = cjsDtsRel.replace(/\.cjs\.d\.ts$/, "");
    const esmDtsRel = `${baseRel}.d.ts`;
    assert.ok(
      relFileSet.has(esmDtsRel),
      `Missing ESM .d.ts stub for:\n- src/${cjsDtsRel}\nExpected:\n- src/${esmDtsRel}`,
    );

    const [esmDtsSource, cjsDtsSource] = await Promise.all([
      readFile(path.resolve(srcDir, esmDtsRel), "utf8"),
      readFile(path.resolve(srcDir, cjsDtsRel), "utf8"),
    ]);

    const esmFnNames = parseExportedFunctionNamesFromDts(esmDtsSource);
    const cjsFnNames = parseExportedFunctionNamesFromDts(cjsDtsSource);

    // The parity test ensures the files match, but keep the runtime contract robust even if that
    // test is ever relaxed.
    assert.deepEqual(
      esmFnNames,
      cjsFnNames,
      `Expected function export list parity between:\n- src/${esmDtsRel}\n- src/${cjsDtsRel}`,
    );

    const esmModuleRel = `${baseRel}.js`;
    const cjsModuleRel = `${baseRel}.cjs`;
    assert.ok(
      relFileSet.has(esmModuleRel),
      `Missing ESM runtime module for:\n- src/${esmDtsRel}\nExpected:\n- src/${esmModuleRel}`,
    );
    assert.ok(
      relFileSet.has(cjsModuleRel),
      `Missing CJS runtime module for:\n- src/${cjsDtsRel}\nExpected:\n- src/${cjsModuleRel}`,
    );

    const esmMod = await import(pathToFileURL(path.resolve(srcDir, esmModuleRel)).href);
    const cjsMod = require(path.resolve(srcDir, cjsModuleRel));

    for (const fnName of esmFnNames) {
      assert.equal(
        typeof esmMod[fnName],
        "function",
        `Expected ESM module src/${esmModuleRel} to export function ${fnName} (declared in src/${esmDtsRel})`,
      );
      assert.equal(
        typeof cjsMod[fnName],
        "function",
        `Expected CJS module src/${cjsModuleRel} to export function ${fnName} (declared in src/${cjsDtsRel})`,
      );
    }
  }

  // ESM-only stubs: enforce that declared runtime functions exist in the ESM module.
  const dts = relFiles.filter((rel) => rel.endsWith(".d.ts") && !rel.endsWith(".cjs.d.ts")).sort();
  for (const dtsRel of dts) {
    const baseRel = dtsRel.replace(/\.d\.ts$/, "");
    const cjsDtsRel = `${baseRel}.cjs.d.ts`;
    if (relFileSet.has(cjsDtsRel)) continue; // already validated above

    const dtsSource = await readFile(path.resolve(srcDir, dtsRel), "utf8");
    const fnNames = parseExportedFunctionNamesFromDts(dtsSource);
    if (fnNames.length === 0) continue;

    const esmModuleRel = `${baseRel}.js`;
    assert.ok(
      relFileSet.has(esmModuleRel),
      `Missing ESM runtime module for:\n- src/${dtsRel}\nExpected:\n- src/${esmModuleRel}`,
    );

    const esmMod = await import(pathToFileURL(path.resolve(srcDir, esmModuleRel)).href);
    for (const fnName of fnNames) {
      assert.equal(
        typeof esmMod[fnName],
        "function",
        `Expected ESM module src/${esmModuleRel} to export function ${fnName} (declared in src/${dtsRel})`,
      );
    }
  }
});

