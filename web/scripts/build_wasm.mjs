import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

import { formatOneLineError } from "../../src/text.js";

function usageAndExit() {
    console.error(
        [
            "Usage: node ./scripts/build_wasm.mjs <threaded|single> [dev|release] [options]",
            "",
            "Options:",
            "  --packages <list>   Comma-separated package list to build (default: all).",
            "                     Known packages: core,gpu,d3d11,jit",
            "",
            "Environment:",
            "  AERO_WASM_PACKAGES  Comma-separated package list to build when --packages is not passed.",
        ].join("\n"),
    );
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

function parseArgs(argv) {
    if (argv.length === 0 || argv.includes("-h") || argv.includes("--help")) {
        usageAndExit();
    }

    const variant = argv[0];
    if (variant !== "threaded" && variant !== "single") {
        usageAndExit();
    }

    let mode = "release";
    let packages = null;

    let i = 1;
    const maybeMode = argv[i];
    if (maybeMode === "dev" || maybeMode === "release") {
        mode = maybeMode;
        i++;
    }

    for (; i < argv.length; i++) {
        const arg = argv[i];
        if (arg === "--packages") {
            const next = argv[i + 1];
            if (!next) {
                die("--packages requires a value");
            }
            packages = next;
            i++;
            continue;
        }
        if (arg?.startsWith("--packages=")) {
            packages = arg.split("=", 2)[1] ?? "";
            if (!packages) {
                die("--packages requires a value");
            }
            continue;
        }
        die(`Unknown argument: ${arg}`);
    }

    if (packages === null) {
        const envValue = (process.env.AERO_WASM_PACKAGES ?? "").trim();
        if (envValue) {
            packages = envValue;
        }
    }

    const packagesList =
        packages === null
            ? null
            : packages
                  .split(",")
                  .map((p) => p.trim())
                  .filter(Boolean);
    return { variant, mode, packages: packagesList };
}

const parsed = parseArgs(process.argv.slice(2));
const variant = parsed.variant;
const mode = parsed.mode;
const requestedPackages = parsed.packages;

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
        die(`Failed to read pinned toolchains from ${toolchainsPath}.\n\n${formatOneLineError(err, 512)}`);
    }
}

let wasmThreadedToolchain = null;
let wasmThreadedToolchainBinDir = null;

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

const outDirAeroJit = path.join(
    wasmRoot,
    variant === "threaded"
        ? isRelease
            ? "pkg-jit-threaded"
            : "pkg-jit-threaded-dev"
        : isRelease
            ? "pkg-jit-single"
            : "pkg-jit-single-dev",
);

const outDirAeroD3d11 = path.join(
    wasmRoot,
    variant === "threaded"
        ? isRelease
            ? "pkg-threaded-d3d11"
            : "pkg-threaded-d3d11-dev"
        : isRelease
            ? "pkg-single-d3d11"
            : "pkg-single-d3d11-dev",
);

const packages = [
    {
        id: "core",
        cratePath,
        outDir: outDirAero,
        outName: "aero_wasm",
        // The main runtime module imports memory so the JS runtime can optionally inject its own
        // `WebAssembly.Memory` (e.g. the shared guest RAM allocation owned by the worker
        // coordinator).
        importMemory: true,
        threaded: true,
    },
    {
        id: "gpu",
        cratePath: path.join(repoRoot, "crates/aero-gpu-wasm"),
        outDir: outDirAeroGpu,
        outName: "aero_gpu_wasm",
        // The GPU worker module follows the same imported-memory contract as the main runtime.
        importMemory: true,
        threaded: true,
    },
    {
        id: "d3d11",
        cratePath: path.join(repoRoot, "crates/aero-d3d11-wasm"),
        outDir: outDirAeroD3d11,
        outName: "aero_d3d11_wasm",
        // The D3D11 shader-cache demo module is self-contained; it does not share the main
        // runtime's imported memory.
        importMemory: false,
        threaded: false,
    },
];

const aeroJitWasmCratePath = path.join(repoRoot, "crates/aero-jit-wasm");
if (existsSync(path.join(aeroJitWasmCratePath, "Cargo.toml"))) {
    packages.push({
        id: "jit",
        cratePath: aeroJitWasmCratePath,
        outDir: outDirAeroJit,
        outName: "aero_jit_wasm",
        // IMPORTANT: the Tier-1 JIT compiler must have its *own* private linear memory so it
        // does not alias the emulator/runtime's `WebAssembly.Memory` (multiple Rust runtimes
        // sharing one linear memory is undefined behaviour).
        //
        // Note: wasm-bindgen's "threads" transform currently *requires imported memory*
        // (it asserts `mem.import.is_some()`). Since we intentionally keep the JIT module's
        // memory private (non-imported), we also opt it out of the threaded/shared-memory
        // build path here.
        importMemory: false,
        threaded: false,
    });
} else {
    console.warn(
        `[wasm] Warning: ${path.relative(repoRoot, aeroJitWasmCratePath)} not found; skipping aero-jit-wasm build.`,
    );
}

// Sanity-check package config. wasm-bindgen's threads transform requires imported memory, so
// any package that opts into the threaded build must also opt into imported memory.
for (const pkg of packages) {
    if (pkg.threaded && !pkg.importMemory) {
        die(
            `[wasm] Internal build config error: package '${pkg.outName}' is marked as threaded, but does not import memory.\n\n` +
                "wasm-bindgen's threads transform currently requires imported memory (and will panic otherwise). " +
                "Either set `importMemory: true` for this package or set `threaded: false`.",
        );
    }
}

const allPackageIds = new Set(packages.map((pkg) => pkg.id));
if (requestedPackages && requestedPackages.length !== 0) {
    const unknown = requestedPackages.filter((id) => !allPackageIds.has(id));
    if (unknown.length !== 0) {
        die(
            `Unknown wasm package id(s): ${unknown.join(", ")}. ` +
                `Known packages: ${Array.from(allPackageIds).join(", ")}`,
        );
    }
}

const packagesToBuild =
    requestedPackages && requestedPackages.length !== 0
        ? packages.filter((pkg) => requestedPackages.includes(pkg.id))
        : packages;

if (packagesToBuild.length === 0) {
    die("No wasm packages selected to build.");
}

const needsThreadedToolchain = isThreaded && packagesToBuild.some((pkg) => pkg.threaded);
if (needsThreadedToolchain) {
    wasmThreadedToolchain = loadPinnedNightlyToolchain();
    // The shared-memory build requires nightly + rust-src (for build-std). We pin the nightly
    // toolchain to keep threaded WASM builds reproducible (see scripts/toolchains.json).
    checkCommand(
        "rustup",
        ["run", wasmThreadedToolchain, "rustc", "--version"],
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

    // When this script is launched via `cargo xtask`, rustup commonly prepends the *stable*
    // toolchain bin dir to $PATH. wasm-pack then resolves `cargo` from that directory, bypassing
    // rustup's toolchain selection logic (and breaking `-Z build-std`).
    //
    // Resolve the pinned nightly toolchain's bin dir so we can force it to the front of PATH for
    // threaded builds.
    const nightlyCargoPath = checkCommand(
        "rustup",
        ["which", "cargo", "--toolchain", wasmThreadedToolchain],
        `Threaded WASM build requires the pinned nightly Rust toolchain (${wasmThreadedToolchain}).\n\nRun:\n  rustup toolchain install ${wasmThreadedToolchain}`,
    );
    wasmThreadedToolchainBinDir = path.dirname(nightlyCargoPath);
}

const baseTargetFeatures = ["+bulk-memory"];
const simdSetting = (process.env.AERO_WASM_SIMD ?? "1").toLowerCase();
if (simdSetting !== "0" && simdSetting !== "false") {
    baseTargetFeatures.push("+simd128");
}
const threadedTargetFeatures = [...baseTargetFeatures, "+atomics", "+mutable-globals"];

const existingRustflags = process.env.RUSTFLAGS?.trim() ?? "";
// Avoid accidentally inheriting target features (especially `+atomics`) from a user's environment.
const rustflagsWithoutTargetFeatures = existingRustflags
    .replace(/-C\s*target-feature=[^ ]+/g, "")
    // Some developer environments inject an lld threads flag via `-Wl,--threads=...` (works for native
    // targets where rustc links via `cc`, but breaks wasm because rustc invokes `rust-lld -flavor wasm`
    // directly). Translate it to the wasm-compatible form so threaded wasm builds remain robust even
    // when running in an agent shell that sourced scripts like `scripts/agent-env.sh`.
    .replace(/-C\s*link-arg=-Wl,--threads=/g, "-C link-arg=--threads=")
    // Avoid inheriting wasm memory import/export knobs; the build script controls those per-package.
    .replace(/-C\s*link-arg=--import-memory\b/g, "")
    .replace(/-C\s*link-arg=--export-memory\b/g, "")
    .replace(/-C\s*link-arg=--shared-memory\b/g, "")
    .replace(/-C\s*link-arg=--stack-first\b/g, "")
    .replace(/-C\s*link-arg=--max-memory(=[^ ]+)?/g, "")
    .replace(/-C\s*link-arg=--export=__wasm_init_tls\b/g, "")
    .replace(/-C\s*link-arg=--export=__tls_base\b/g, "")
    .replace(/-C\s*link-arg=--export=__tls_size\b/g, "")
    .replace(/-C\s*link-arg=--export=__tls_align\b/g, "")
    // Keep release builds reproducible by stripping codegen knobs we explicitly control.
    .replace(/-C\s*opt-level=[^ ]+/g, "")
    .replace(/-C\s*lto(=[^ ]+)?/g, "")
    .replace(/-C\s*codegen-units=[^ ]+/g, "")
    .replace(/-C\s*embed-bitcode=[^ ]+/g, "")
    .trim();
const commonRustflags = [];
if (isRelease) {
    // Release builds are tuned for runtime performance.
    //
    // Important: do NOT pass `-C lto` via `RUSTFLAGS`.
    //
    // wasm-bindgen crates commonly emit both `cdylib` (for the final .wasm) and `rlib`
    // (so the crate can be used as a Rust dependency / in tests). Rust rejects `-C lto`
    // for those multi-output builds with:
    //   "lto can only be run for executables, cdylibs and static library outputs"
    //
    // We rely on `wasm-opt -O4` for post-link optimization instead.
    commonRustflags.push("-C opt-level=3", "-C codegen-units=1");
}

const importedMemoryLinkArgs = [
    "-C link-arg=--import-memory",
    "-C link-arg=--export-memory",
    // Place the Rust/WASM stack at low addresses so the runtime-reserved region
    // is contiguous and guest RAM can live at high addresses (`guest_base`).
    "-C link-arg=--stack-first",
    `-C link-arg=--max-memory=${maxMemoryBytes}`,
];

const sharedMemoryLinkArgs = [
    // Produce a shared-memory + imported-memory module so it can be used in
    // crossOriginIsolated contexts (SharedArrayBuffer + Atomics).
    "-C link-arg=--shared-memory",
    // wasm-bindgen's threads transform expects these TLS exports.
    "-C link-arg=--export=__wasm_init_tls",
    "-C link-arg=--export=__tls_base",
    "-C link-arg=--export=__tls_size",
    "-C link-arg=--export=__tls_align",
];

function rustflagsForPkg(pkg, pkgUsesThreads) {
    const targetFeatures = pkgUsesThreads ? threadedTargetFeatures : baseTargetFeatures;
    const out = [];
    if (rustflagsWithoutTargetFeatures) out.push(rustflagsWithoutTargetFeatures);
    if (targetFeatures.length !== 0) {
        out.push(`-C target-feature=${targetFeatures.join(",")}`);
    }
    out.push(...commonRustflags);
    if (pkg.importMemory) {
        // The module's `--max-memory` must be >= the maximum the runtime will ever allocate for
        // `guestMemory.maximum`; otherwise instantiation fails with a memory type mismatch when the
        // runtime injects its own memory.
        out.push(...importedMemoryLinkArgs);
        if (pkgUsesThreads) {
            out.push(...sharedMemoryLinkArgs);
        }
    }
    return out.filter(Boolean).join(" ").trim();
}

const baseEnv = { ...process.env };
// Keep all builds on the repo's pinned stable toolchain by default (regardless of any
// user-level overrides). Individual packages that need nightly (threaded/shared-memory
// builds) opt in below via `pkgUsesThreads`.
delete baseEnv.RUSTUP_TOOLCHAIN;

// Prefer reproducible builds when a workspace lockfile is present, but allow
// building without `Cargo.lock` (e.g. minimal checkouts or downstream forks).
const lockFile = path.join(repoRoot, "Cargo.lock");
const useLocked = existsSync(lockFile);

for (const pkg of packagesToBuild) {
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

    const pkgUsesThreads = isThreaded && pkg.threaded;
    if (pkgUsesThreads) {
        args.push("-Z", "build-std=std,panic_abort", "--features", "wasm-threaded");
    }

    if (useLocked) {
        args.push("--locked");
    }

    const env = { ...baseEnv };
    if (pkgUsesThreads) {
        // Threaded/shared-memory builds use the pinned nightly toolchain so `-Z build-std`
        // is reproducible (see scripts/toolchains.json).
        env.RUSTUP_TOOLCHAIN = wasmThreadedToolchain;
        if (wasmThreadedToolchainBinDir) {
            env.PATH = `${wasmThreadedToolchainBinDir}${path.delimiter}${baseEnv.PATH ?? ""}`;
        }
    }
    const rustflags = rustflagsForPkg(pkg, pkgUsesThreads);
    if (rustflags) {
        env.RUSTFLAGS = rustflags;
    } else {
        delete env.RUSTFLAGS;
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
                    ...(pkgUsesThreads ? ["--enable-threads"] : []),
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
