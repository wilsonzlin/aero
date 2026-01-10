import { spawnSync } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

function usageAndExit() {
    console.error("Usage: node ./scripts/build_wasm.mjs <threaded|single>");
    process.exit(2);
}

const variant = process.argv[2];
if (variant !== "threaded" && variant !== "single") {
    usageAndExit();
}

const isThreaded = variant === "threaded";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, "../..");
const cratePath = path.join(repoRoot, "crates/aero-wasm");
const outDir = path.join(repoRoot, "web/src/wasm", variant === "threaded" ? "pkg-threaded" : "pkg-single");

rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

const targetFeatures = isThreaded ? ["+atomics", "+bulk-memory", "+mutable-globals"] : [];

if (process.env.AERO_WASM_SIMD === "1" || process.env.AERO_WASM_SIMD === "true") {
    targetFeatures.push("+simd128");
}

const existingRustflags = process.env.RUSTFLAGS?.trim() ?? "";
// Avoid accidentally inheriting target features (especially `+atomics`) from a user's environment.
const rustflagsWithoutTargetFeatures = existingRustflags.replace(/-C\s*target-feature=[^ ]+/g, "").trim();
const requiredRustflags = [];
if (targetFeatures.length !== 0) {
    requiredRustflags.push(`-C target-feature=${targetFeatures.join(",")}`);
}

if (isThreaded) {
    // Produce a shared-memory + imported-memory module so it can be used in
    // crossOriginIsolated contexts (SharedArrayBuffer + Atomics).
    requiredRustflags.push(
        "-C link-arg=--shared-memory",
        "-C link-arg=--import-memory",
        "-C link-arg=--export-memory",
        // Ensure the shared memory has headroom for wasm-bindgen's stack/TLS
        // allocation in `__wbindgen_start`, even before we actually spawn
        // additional threads.
        "-C link-arg=--max-memory=268435456",
        // wasm-bindgen's threads transform expects these TLS exports.
        "-C link-arg=--export=__wasm_init_tls",
        "-C link-arg=--export=__tls_base",
        "-C link-arg=--export=__tls_size",
        "-C link-arg=--export=__tls_align",
    );
}

const rustflags = [rustflagsWithoutTargetFeatures, ...requiredRustflags].filter(Boolean).join(" ").trim();

const env = { ...process.env };
if (rustflags) {
    env.RUSTFLAGS = rustflags;
} else {
    delete env.RUSTFLAGS;
}

if (isThreaded) {
    // `--shared-memory` requires rebuilding std with atomics/bulk-memory enabled.
    // This uses nightly + rust-src (`rustup component add rust-src --toolchain nightly`).
    env.RUSTUP_TOOLCHAIN = "nightly";
}

const args = [
    "build",
    cratePath,
    "--target",
    "web",
    "--release",
    "--out-dir",
    outDir,
    "--out-name",
    "aero_wasm",
];

if (isThreaded) {
    args.push("-Z", "build-std=std,panic_abort", "--features", "wasm-threaded");
}

const result = spawnSync("wasm-pack", args, { env, stdio: "inherit" });
process.exit(result.status ?? 1);
