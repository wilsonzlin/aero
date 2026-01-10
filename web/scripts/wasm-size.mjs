import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const webDir = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(webDir, "..");
const wasmRoot = path.resolve(webDir, "src", "wasm");

function formatBytes(bytes) {
  const kib = bytes / 1024;
  const mib = kib / 1024;
  if (mib >= 1) return `${bytes.toLocaleString()} B (${mib.toFixed(2)} MiB)`;
  if (kib >= 1) return `${bytes.toLocaleString()} B (${kib.toFixed(2)} KiB)`;
  return `${bytes.toLocaleString()} B`;
}

function gzipSize(buf) {
  return zlib.gzipSync(buf, { level: 9 }).byteLength;
}

function listJsFiles(dir) {
  const out = [];
  const walk = (d) => {
    for (const entry of fs.readdirSync(d, { withFileTypes: true })) {
      const full = path.join(d, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.isFile() && entry.name.endsWith(".js")) out.push(full);
    }
  };
  walk(dir);
  return out;
}

const variants = [
  { name: "single", dir: path.join(wasmRoot, "pkg-single") },
  { name: "threaded", dir: path.join(wasmRoot, "pkg-threaded") },
];

let foundAny = false;

console.log("WASM artifact size report");
console.log("========================");
console.log(`WASM package root: ${path.relative(repoRoot, wasmRoot)}`);

for (const variant of variants) {
  const wasmPath = path.join(variant.dir, "aero_wasm_bg.wasm");
  if (!fs.existsSync(wasmPath)) {
    continue;
  }

  foundAny = true;
  const wasmBytes = fs.readFileSync(wasmPath);
  const wasmGz = gzipSize(wasmBytes);

  const jsFiles = listJsFiles(variant.dir).filter((p) => !p.endsWith("_bg.wasm.js"));
  const jsBuffers = jsFiles.map((p) => fs.readFileSync(p));
  const jsRaw = jsBuffers.reduce((sum, b) => sum + b.byteLength, 0);
  const jsConcat = Buffer.concat(
    jsBuffers.flatMap((b, i) => (i === 0 ? [b] : [Buffer.from("\n"), b])),
  );
  const jsGz = gzipSize(jsConcat);

  console.log("");
  console.log(`Variant: ${variant.name}`);
  console.log(`Package dir: ${path.relative(repoRoot, variant.dir)}`);
  console.log(`WASM: ${path.relative(repoRoot, wasmPath)}`);
  console.log(`  raw:  ${formatBytes(wasmBytes.byteLength)}`);
  console.log(`  gzip: ${formatBytes(wasmGz)}`);
  console.log(`JS glue (${jsFiles.length} file${jsFiles.length === 1 ? "" : "s"})`);
  console.log(`  raw:  ${formatBytes(jsRaw)}`);
  console.log(`  gzip: ${formatBytes(jsGz)}`);
}

if (!foundAny) {
  console.error(
    `[wasm:size] No wasm-pack output found under ${path.relative(repoRoot, wasmRoot)}. ` +
      "Run `npm run wasm:build:release` first.",
  );
  process.exit(1);
}
