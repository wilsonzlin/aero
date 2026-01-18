import path from "node:path";
import { readdir } from "node:fs/promises";

/**
 * Recursively list file paths under a directory.
 *
 * - Returned paths are relative to `absDir`
 * - Uses POSIX-style `/` separators for stable comparisons
 *
 * @param {string} absDir
 * @returns {Promise<string[]>}
 */
export async function listFilesRecursive(absDir) {
  /** @type {string[]} */
  const out = [];

  /** @param {string} curAbs @param {string} relPrefix */
  async function visit(curAbs, relPrefix) {
    const entries = await readdir(curAbs, { withFileTypes: true });
    // Ensure stable traversal order across filesystems/environments.
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const ent of entries) {
      const abs = path.join(curAbs, ent.name);
      const rel = relPrefix ? `${relPrefix}/${ent.name}` : ent.name;
      if (ent.isDirectory()) {
        await visit(abs, rel);
        continue;
      }
      if (!ent.isFile()) continue;
      out.push(rel);
    }
  }

  await visit(absDir, "");
  return out;
}

