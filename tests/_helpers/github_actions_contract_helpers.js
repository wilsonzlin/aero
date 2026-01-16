import fs from "node:fs/promises";
import path from "node:path";

export async function listCompositeActionYmlRelPaths(repoRoot) {
  const actionsRoot = path.join(repoRoot, ".github", "actions");
  const filesAbs = (await listActionYmlFiles(actionsRoot)).sort();
  return filesAbs.map((p) => path.relative(repoRoot, p).replaceAll("\\", "/"));
}

async function listActionYmlFiles(dirAbs) {
  const out = [];
  const entries = await fs.readdir(dirAbs, { withFileTypes: true });
  for (const entry of entries) {
    const full = path.join(dirAbs, entry.name);
    if (entry.isDirectory()) {
      out.push(...(await listActionYmlFiles(full)));
      continue;
    }
    if (!entry.isFile()) continue;
    if (entry.name === "action.yml") out.push(full);
  }
  return out;
}

