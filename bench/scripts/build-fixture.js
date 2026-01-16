import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

async function copyDir(src, dest) {
  await fs.mkdir(dest, { recursive: true });
  const entries = await fs.readdir(src, { withFileTypes: true });
  for (const ent of entries) {
    const from = path.join(src, ent.name);
    const to = path.join(dest, ent.name);
    if (ent.isDirectory()) await copyDir(from, to);
    else if (ent.isFile()) await fs.copyFile(from, to);
  }
}

async function main() {
  const projectRoot = path.resolve(__dirname, '..', '..');
  const src = path.join(projectRoot, 'bench', 'fixture');
  const dist = path.join(projectRoot, 'dist');

  await fs.rm(dist, { recursive: true, force: true });
  await copyDir(src, dist);

  // eslint-disable-next-line no-console
  console.log(`Built bench fixture to ${path.relative(projectRoot, dist)}`);
}

main().catch((err) => {
  // eslint-disable-next-line no-console
  console.error(err?.stack || err);
  process.exitCode = 1;
});
