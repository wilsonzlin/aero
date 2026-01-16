import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export function repoRootFromTestUrl(importMetaUrl) {
  const filename = fileURLToPath(importMetaUrl);
  return path.resolve(path.dirname(filename), "..");
}

export function runNodeScriptFromRepoRoot(repoRoot, scriptRel, env) {
  const scriptAbs = path.join(repoRoot, scriptRel);
  const res = spawnSync(process.execPath, [scriptAbs], {
    env: { ...process.env, ...env },
    encoding: "utf8",
  });
  if (res.status !== 0) {
    const stderr = (res.stderr || "").trim();
    assert.fail(`script failed: ${scriptRel}\n${stderr}`);
  }
}

export async function readKeyValueFile(filePath) {
  const raw = await fs.readFile(filePath, "utf8");
  const out = new Map();
  for (const line of raw.split("\n")) {
    if (!line) continue;
    const idx = line.indexOf("=");
    if (idx <= 0) continue;
    out.set(line.slice(0, idx), line.slice(idx + 1));
  }
  return out;
}

