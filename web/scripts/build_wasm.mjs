import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, renameSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

function usageAndExit() {
    console.error("Usage: node ./scripts/build_wasm.mjs <threaded|single> [dev|release]");
    process.exit(2);
}

function die(message) {
    console.error(message);
    process.exit(1);
}

function checkCommand(command, args, help) {
    const result = spawnSync(command, args, { stdio: "pipe", encoding: "utf8" });
    if (result.status !== 0) {
        const details = (result.stderr || result.stdout || "").trim();
        die([help, details ? `\n\nDetails:\n${details}` : ""].join(""));
    }
    return result.stdout.trim();
}

const variant = process.argv[2];
const mode = process.argv[3] ?? "release";

if (variant !== "threaded" && variant !== "single") {
    usageAndExit();
}

if (mode !== "dev" && mode !== "release") {
    usageAndExit();
}

const isThreaded = variant === "threaded";
const isRelease = mode === "release";

if (isThreaded) {
    // The shared-memory build requires nightly + rust-src (for build-std).
    checkCommand(
        "rustc",
        ["+nightly", "--version"],
        "Threaded WASM build requires the nightly Rust toolchain.\n\nRun:\n  rustup toolchain install nightly",
    );

    const installed = checkCommand(
        "rustup",
        ["component", "list", "--installed", "--toolchain", "nightly"],
        "Threaded WASM build requires rust-src on the nightly toolchain.\n\nRun:\n  rustup component add rust-src --toolchain nightly",
    );
    if (!installed.split("\n").some((line) => line.trim() === "rust-src")) {
        die(
            "Threaded WASM build requires rust-src on the nightly toolchain.\n\nRun:\n  rustup component add rust-src --toolchain nightly",
        );
    }
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, "../..");
const cratePath = path.join(repoRoot, "crates/aero-wasm");
const outDir = path.join(
    repoRoot,
    "web/src/wasm",
    variant === "threaded"
        ? isRelease
            ? "pkg-threaded"
            : "pkg-threaded-dev"
        : isRelease
            ? "pkg-single"
            : "pkg-single-dev",
);

rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

const targetFeatures = ["+bulk-memory"];
const simdSetting = (process.env.AERO_WASM_SIMD ?? "1").toLowerCase();
if (simdSetting !== "0" && simdSetting !== "false") {
    targetFeatures.push("+simd128");
}
if (isThreaded) {
    targetFeatures.push("+atomics", "+mutable-globals");
}

const existingRustflags = process.env.RUSTFLAGS?.trim() ?? "";
// Avoid accidentally inheriting target features (especially `+atomics`) from a user's environment.
const rustflagsWithoutTargetFeatures = existingRustflags
    .replace(/-C\s*target-feature=[^ ]+/g, "")
    // Keep release builds reproducible by stripping codegen knobs we explicitly control.
    .replace(/-C\s*opt-level=[^ ]+/g, "")
    .replace(/-C\s*lto(=[^ ]+)?/g, "")
    .replace(/-C\s*codegen-units=[^ ]+/g, "")
    .replace(/-C\s*embed-bitcode=[^ ]+/g, "")
    .trim();
const requiredRustflags = [];
if (targetFeatures.length !== 0) {
    requiredRustflags.push(`-C target-feature=${targetFeatures.join(",")}`);
}

if (isRelease) {
    // Release builds are tuned for runtime performance.
    // Note: `-C lto=thin` requires `-C embed-bitcode=yes` (Cargo defaults to `no`).
    requiredRustflags.push("-C opt-level=3", "-C lto=thin", "-C codegen-units=1", "-C embed-bitcode=yes");
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

// Note: wasm-pack treats args *after* the PATH as cargo args, so all wasm-pack
// options must appear before `cratePath` (especially `--no-opt`).
const args = [
    "build",
    "--target",
    "web",
    isRelease ? "--release" : "--dev",
    "--out-dir",
    outDir,
    "--out-name",
    "aero_wasm",
    "--no-opt",
    cratePath,
    "--locked",
];

if (isThreaded) {
    args.push("-Z", "build-std=std,panic_abort", "--features", "wasm-threaded");
}

const result = spawnSync("wasm-pack", args, { env, stdio: "inherit" });
if ((result.status ?? 1) !== 0) {
    process.exit(result.status ?? 1);
}

if (isRelease) {
    const wasmFile = path.join(outDir, "aero_wasm_bg.wasm");
    if (existsSync(wasmFile)) {
        const wasmOptCheck = spawnSync("wasm-opt", ["--version"], { stdio: "ignore" });
        if (wasmOptCheck.status === 0) {
            const wasmOptArgs = [
                "-O4",
                "--enable-simd",
                "--enable-bulk-memory",
                "--enable-reference-types",
                "--enable-mutable-globals",
                ...(isThreaded ? ["--enable-threads"] : []),
                "-o",
                `${wasmFile}.opt`,
                wasmFile,
            ];
            const wasmOpt = spawnSync("wasm-opt", wasmOptArgs, { stdio: "inherit" });
            if ((wasmOpt.status ?? 1) !== 0) {
                process.exit(wasmOpt.status ?? 1);
            }
            renameSync(`${wasmFile}.opt`, wasmFile);
        } else {
            console.warn(
                "[wasm] Warning: wasm-opt not found; skipping Binaryen optimizations. " +
                    "Install `wasm-opt` (Binaryen) for smaller/faster release builds.",
            );
        }
    }
}

process.exit(0);
