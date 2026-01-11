import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync } from "node:fs";
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

const WASM_PAGE_BYTES = 64 * 1024;
const MAX_WASM32_PAGES = 65536;
const MAX_WASM32_BYTES = WASM_PAGE_BYTES * MAX_WASM32_PAGES;

function parseMaxMemoryBytes() {
    const raw = (process.env.AERO_WASM_MAX_MEMORY_BYTES ?? "").trim();
    // 4 GiB: wasm32 can address at most 2^32 bytes.
    const fallback = 4 * 1024 * 1024 * 1024;
    if (!raw) return fallback;

    const value = Number.parseInt(raw, 10);
    if (!Number.isFinite(value) || value <= 0) {
        die(
            `Invalid AERO_WASM_MAX_MEMORY_BYTES value: '${raw}'. Expected a positive integer number of bytes (e.g. 4294967296).`,
        );
    }
    return value;
}

const maxMemoryBytesInput = parseMaxMemoryBytes();
const maxMemoryBytes = Math.ceil(maxMemoryBytesInput / WASM_PAGE_BYTES) * WASM_PAGE_BYTES;
if (maxMemoryBytes !== maxMemoryBytesInput) {
    console.warn(
        `[wasm] Warning: AERO_WASM_MAX_MEMORY_BYTES=${maxMemoryBytesInput} is not a multiple of ${WASM_PAGE_BYTES}; rounding up to ${maxMemoryBytes}.`,
    );
}
if (maxMemoryBytes > MAX_WASM32_BYTES) {
    die(
        `AERO_WASM_MAX_MEMORY_BYTES=${maxMemoryBytes} exceeds wasm32's limit (${MAX_WASM32_BYTES} bytes / ${MAX_WASM32_PAGES} pages).`,
    );
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, "../..");
const wasmRoot = path.join(repoRoot, "web/src/wasm");

const toolchainsPath = path.join(repoRoot, "scripts/toolchains.json");

function loadPinnedNightlyToolchain() {
    try {
        const parsed = JSON.parse(readFileSync(toolchainsPath, "utf8"));
        const toolchain = parsed?.rust?.nightlyWasm;
        if (typeof toolchain !== "string" || toolchain.trim() === "") {
            die(`Missing rust.nightlyWasm in ${toolchainsPath}`);
        }
        const trimmed = toolchain.trim();
        if (!/^nightly-\d{4}-\d{2}-\d{2}$/.test(trimmed)) {
            die(`Invalid rust.nightlyWasm in ${toolchainsPath} (expected nightly-YYYY-MM-DD; got '${trimmed}')`);
        }
        return trimmed;
    } catch (err) {
        die(`Failed to read pinned toolchains from ${toolchainsPath}.\n\n${err?.message ?? String(err)}`);
    }
}

const wasmThreadedToolchain = isThreaded ? loadPinnedNightlyToolchain() : null;

if (isThreaded) {
    // The shared-memory build requires nightly + rust-src (for build-std). We pin the nightly
    // toolchain to keep threaded WASM builds reproducible (see scripts/toolchains.json).
    checkCommand(
        "rustc",
        [`+${wasmThreadedToolchain}`, "--version"],
        `Threaded WASM build requires the pinned nightly Rust toolchain (${wasmThreadedToolchain}).\n\nRun:\n  rustup toolchain install ${wasmThreadedToolchain}`,
    );

    const installedComponents = checkCommand(
        "rustup",
        ["component", "list", "--installed", "--toolchain", wasmThreadedToolchain],
        `Threaded WASM build requires rust-src on ${wasmThreadedToolchain}.\n\nRun:\n  rustup component add rust-src --toolchain ${wasmThreadedToolchain}`,
    );
    if (!installedComponents.split("\n").some((line) => line.trim() === "rust-src")) {
        die(
            `Threaded WASM build requires rust-src on ${wasmThreadedToolchain}.\n\nRun:\n  rustup component add rust-src --toolchain ${wasmThreadedToolchain}`,
        );
    }

    const installedTargets = checkCommand(
        "rustup",
        ["target", "list", "--installed", "--toolchain", wasmThreadedToolchain],
        `Threaded WASM build requires wasm32-unknown-unknown on ${wasmThreadedToolchain}.\n\nRun:\n  rustup target add wasm32-unknown-unknown --toolchain ${wasmThreadedToolchain}`,
    );
    if (!installedTargets.split("\n").some((line) => line.trim() === "wasm32-unknown-unknown")) {
        die(
            `Threaded WASM build requires wasm32-unknown-unknown on ${wasmThreadedToolchain}.\n\nRun:\n  rustup target add wasm32-unknown-unknown --toolchain ${wasmThreadedToolchain}`,
        );
    }
}

function parseKeyValueLines(output) {
    const values = {};
    for (const line of output.split(/\r?\n/u)) {
        const trimmed = line.trim();
        if (!trimmed) {
            continue;
        }
        const idx = trimmed.indexOf("=");
        if (idx === -1) {
            continue;
        }
        const key = trimmed.slice(0, idx);
        const value = trimmed.slice(idx + 1);
        values[key] = value;
    }
    return values;
}

const detect = spawnSync("node", [path.join(repoRoot, "scripts/ci/detect-wasm-crate.mjs")], {
    stdio: ["ignore", "pipe", "inherit"],
    encoding: "utf8",
});
if ((detect.status ?? 1) !== 0) {
    die(
        "Failed to resolve the Rust WASM crate.\n\nTip: set AERO_WASM_CRATE_DIR to the crate directory containing Cargo.toml.",
    );
}
const detectOutput = (detect.stdout ?? "").trim();
const detected = parseKeyValueLines(detectOutput);
if (!detected.dir) {
    die("Failed to resolve the Rust WASM crate (resolver returned empty output).");
}

const cratePath = path.isAbsolute(detected.dir) ? detected.dir : path.join(repoRoot, detected.dir);
const outDirAero = path.join(
    wasmRoot,
    variant === "threaded"
        ? isRelease
            ? "pkg-threaded"
            : "pkg-threaded-dev"
        : isRelease
            ? "pkg-single"
            : "pkg-single-dev",
);

const outDirAeroGpu = path.join(
    wasmRoot,
    variant === "threaded"
        ? isRelease
            ? "pkg-threaded-gpu"
            : "pkg-threaded-gpu-dev"
        : isRelease
            ? "pkg-single-gpu"
            : "pkg-single-gpu-dev",
);

const packages = [
    {
        cratePath,
        outDir: outDirAero,
        outName: "aero_wasm",
    },
    {
        cratePath: path.join(repoRoot, "crates/aero-gpu-wasm"),
        outDir: outDirAeroGpu,
        outName: "aero_gpu_wasm",
    },
];

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

// Both variants are built with imported+exported memory so the web runtime can
// optionally provide a `WebAssembly.Memory` (e.g. the shared guest RAM allocation
// owned by the worker coordinator).
//
// The module's `--max-memory` must be >= the maximum the runtime will ever
// allocate for `guestMemory.maximum`; otherwise instantiation fails with a
// memory type mismatch when the runtime injects its own memory.
requiredRustflags.push(
    "-C link-arg=--import-memory",
    "-C link-arg=--export-memory",
    // Place the Rust/WASM stack at low addresses so the runtime-reserved region
    // is contiguous and guest RAM can live at high addresses (`guest_base`).
    "-C link-arg=--stack-first",
    `-C link-arg=--max-memory=${maxMemoryBytes}`,
);

if (isThreaded) {
    // Produce a shared-memory + imported-memory module so it can be used in
    // crossOriginIsolated contexts (SharedArrayBuffer + Atomics).
    requiredRustflags.push(
        "-C link-arg=--shared-memory",
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
    // This uses the pinned nightly toolchain + rust-src (see scripts/toolchains.json).
    env.RUSTUP_TOOLCHAIN = wasmThreadedToolchain;
} else {
    // Keep the single-threaded build on the repo's pinned stable toolchain, regardless of any
    // user-level overrides.
    delete env.RUSTUP_TOOLCHAIN;
}

// Prefer reproducible builds when a workspace lockfile is present, but allow
// building without `Cargo.lock` (e.g. minimal checkouts or downstream forks).
const lockFile = path.join(repoRoot, "Cargo.lock");
const useLocked = existsSync(lockFile);

for (const pkg of packages) {
    rmSync(pkg.outDir, { recursive: true, force: true });
    mkdirSync(pkg.outDir, { recursive: true });

    // Note: wasm-pack treats args *after* the PATH as cargo args, so all wasm-pack
    // options must appear before `cratePath` (especially `--no-opt`).
    const args = [
        "build",
        "--target",
        "web",
        isRelease ? "--release" : "--dev",
        "--out-dir",
        pkg.outDir,
        "--out-name",
        pkg.outName,
        "--no-opt",
        pkg.cratePath,
    ];

    if (isThreaded) {
        args.push("-Z", "build-std=std,panic_abort", "--features", "wasm-threaded");
    }

    if (useLocked) {
        args.push("--locked");
    }

    const result = spawnSync("wasm-pack", args, { env, stdio: "inherit" });
    if ((result.status ?? 1) !== 0) {
        process.exit(result.status ?? 1);
    }

    if (isRelease) {
        const wasmFile = path.join(pkg.outDir, `${pkg.outName}_bg.wasm`);
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
}

process.exit(0);
