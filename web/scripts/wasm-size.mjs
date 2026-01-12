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
  { name: "single" },
  { name: "threaded" },
];

const packages = [
  {
    label: "core (aero-wasm)",
    outName: "aero_wasm",
    dirForVariant: (variant) => (variant === "threaded" ? "pkg-threaded" : "pkg-single"),
  },
  {
    label: "gpu (aero-gpu-wasm)",
    outName: "aero_gpu_wasm",
    dirForVariant: (variant) => (variant === "threaded" ? "pkg-threaded-gpu" : "pkg-single-gpu"),
  },
  {
    label: "jit (aero-jit-wasm)",
    outName: "aero_jit_wasm",
    dirForVariant: (variant) => (variant === "threaded" ? "pkg-jit-threaded" : "pkg-jit-single"),
  },
];

let foundAny = false;

console.log("WASM artifact size report");
console.log("========================");
console.log(`WASM package root: ${path.relative(repoRoot, wasmRoot)}`);

for (const variant of variants) {
  const results = [];

  for (const pkg of packages) {
    const dir = path.join(wasmRoot, pkg.dirForVariant(variant.name));
    const wasmPath = path.join(dir, `${pkg.outName}_bg.wasm`);
    if (!fs.existsSync(wasmPath)) {
      continue;
    }

    foundAny = true;
    const wasmBytes = fs.readFileSync(wasmPath);
    const wasmGz = gzipSize(wasmBytes);

    const jsFiles = listJsFiles(dir).filter((p) => !p.endsWith("_bg.wasm.js"));
    const jsBuffers = jsFiles.map((p) => fs.readFileSync(p));
    const jsRaw = jsBuffers.reduce((sum, b) => sum + b.byteLength, 0);
    const jsConcat = Buffer.concat(
      jsBuffers.flatMap((b, i) => (i === 0 ? [b] : [Buffer.from("\n"), b])),
    );
    const jsGz = gzipSize(jsConcat);

    results.push({
      pkg,
      dir,
      wasmPath,
      wasmBytes,
      wasmGz,
      jsFiles,
      jsRaw,
      jsGz,
    });
  }

  if (results.length === 0) {
    continue;
  }

  console.log("");
  console.log(`Variant: ${variant.name}`);

  for (const r of results) {
    console.log(`Package: ${r.pkg.label}`);
    console.log(`Package dir: ${path.relative(repoRoot, r.dir)}`);
    console.log(`WASM: ${path.relative(repoRoot, r.wasmPath)}`);
    console.log(`  raw:  ${formatBytes(r.wasmBytes.byteLength)}`);
    console.log(`  gzip: ${formatBytes(r.wasmGz)}`);
    console.log(`JS glue (${r.jsFiles.length} file${r.jsFiles.length === 1 ? "" : "s"})`);
    console.log(`  raw:  ${formatBytes(r.jsRaw)}`);
    console.log(`  gzip: ${formatBytes(r.jsGz)}`);
    console.log("");
  }
}

if (!foundAny) {
  console.error(
    `[wasm:size] No wasm-pack output found under ${path.relative(repoRoot, wasmRoot)}. ` +
      "Run `npm run wasm:build:release` first.",
  );
  process.exit(1);
}
