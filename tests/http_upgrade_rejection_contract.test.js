import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

async function* walkFiles(dir) {
  const entries = await fs.readdir(dir, { withFileTypes: true });
  for (const ent of entries) {
    if (ent.name.startsWith(".")) continue;
    const p = path.join(dir, ent.name);
    if (ent.isDirectory()) {
      yield* walkFiles(p);
      continue;
    }
    if (ent.isFile()) yield p;
  }
}

function isCodeFile(p) {
  return p.endsWith(".js") || p.endsWith(".mjs") || p.endsWith(".cjs") || p.endsWith(".ts") || p.endsWith(".tsx");
}

test("contract: upgrade rejections must not embed raw HTTP/1.1 4xx/5xx status lines", async () => {
  // If we need a raw HTTP response, use `src/http_text_response.js` (+ `endThenDestroyQuietly`)
  // so headers and Content-Length can't drift.
  const roots = [
    path.join(REPO_ROOT, "server", "src"),
    path.join(REPO_ROOT, "net-proxy", "src"),
    path.join(REPO_ROOT, "backend", "aero-gateway", "src"),
    path.join(REPO_ROOT, "tools"),
    path.join(REPO_ROOT, "scripts"),
  ];

  const allowlist = new Set([
    // net-proxy is a CJS workspace; importing the repo-root ESM encoder is unsafe under Node 22.
    // This module has its own dedicated unit tests that lock down headers/formatting.
    "net-proxy/src/wsUpgradeHttp.ts",
  ]);

  const forbidden = [
    // Literal rejection status lines (avoid hand-written responses).
    /HTTP\/1\.1\s+[45]\d\d\b/u,
    // Templated status lines are also forbidden; they typically indicate a hand-written response
    // like `HTTP/1.1 ${status} ...` instead of the shared encoder.
    /HTTP\/1\.1\s+\$\{/u,
  ];
  const hits = [];

  for (const root of roots) {
    for await (const file of walkFiles(root)) {
      if (!isCodeFile(file)) continue;
      const rel = path.relative(REPO_ROOT, file);
      // Tests are out of scope for this contract.
      if (rel.includes(`${path.sep}test${path.sep}`) || rel.includes(`${path.sep}tests${path.sep}`)) continue;
      if (allowlist.has(rel.replaceAll(path.sep, "/"))) continue;

      const text = await fs.readFile(file, "utf8");
      for (const re of forbidden) {
        const m = re.exec(text);
        if (!m) continue;
        const snippet = text
          .slice(Math.max(0, m.index - 40), Math.min(text.length, m.index + 80))
          .replaceAll("\n", "\\n");
        hits.push({ rel, snippet });
        break;
      }
    }
  }

  assert.deepEqual(
    hits,
    [],
    `Found raw HTTP status lines (use encodeHttpTextResponse):\n${hits
      .map((h) => `- ${h.rel}: …${h.snippet}…`)
      .join("\n")}`,
  );
});
