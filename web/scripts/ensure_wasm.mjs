import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { formatOneLineError } from "../../src/text.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, "../..");

class EnsureWasmError extends Error {
    constructor(message, status = 1) {
        super(message);
        this.name = "EnsureWasmError";
        this.status = status;
    }
}

export function ensureVariant(variant) {
    const outDirAero = path.join(repoRoot, "web/src/wasm", variant === "threaded" ? "pkg-threaded" : "pkg-single");
    const outDirAeroGpu = path.join(
        repoRoot,
        "web/src/wasm",
        variant === "threaded" ? "pkg-threaded-gpu" : "pkg-single-gpu",
    );
    const outDirAeroD3d11 = path.join(
        repoRoot,
        "web/src/wasm",
        variant === "threaded" ? "pkg-threaded-d3d11" : "pkg-single-d3d11",
    );
    const outDirAeroJit = path.join(
        repoRoot,
        "web/src/wasm",
        variant === "threaded" ? "pkg-jit-threaded" : "pkg-jit-single",
    );

    const packages = [
        {
            id: "core",
            files: [path.join(outDirAero, "aero_wasm.js"), path.join(outDirAero, "aero_wasm_bg.wasm")],
        },
        {
            id: "gpu",
            files: [path.join(outDirAeroGpu, "aero_gpu_wasm.js"), path.join(outDirAeroGpu, "aero_gpu_wasm_bg.wasm")],
        },
        {
            id: "d3d11",
            files: [
                path.join(outDirAeroD3d11, "aero_d3d11_wasm.js"),
                path.join(outDirAeroD3d11, "aero_d3d11_wasm_bg.wasm"),
            ],
        },
    ];

    // Optional: the Tier-1 JIT compiler wasm-pack package. Only require it when the
    // crate exists (e.g. downstream forks may not include it).
    const jitCratePath = path.join(repoRoot, "crates/aero-jit-wasm", "Cargo.toml");
    if (existsSync(jitCratePath)) {
        packages.push({
            id: "jit",
            files: [path.join(outDirAeroJit, "aero_jit_wasm.js"), path.join(outDirAeroJit, "aero_jit_wasm_bg.wasm")],
        });
    }

    const expectedFiles = packages.flatMap((pkg) => pkg.files);
    const missingPackages = packages.filter((pkg) => pkg.files.some((file) => !existsSync(file))).map((pkg) => pkg.id);

    if (missingPackages.length === 0) {
        return;
    }

    const result = spawnSync(
        "node",
        [path.join(__dirname, "build_wasm.mjs"), variant, "--packages", missingPackages.join(",")],
        { stdio: "inherit" },
    );
    if (result.error) {
        throw new EnsureWasmError(
            `[wasm] Failed to execute build_wasm.mjs for variant '${variant}': ${formatOneLineError(result.error, 512)}`,
            1,
        );
    }
    if ((result.status ?? 1) !== 0) {
        // build_wasm.mjs already printed details; preserve its exit code.
        throw new EnsureWasmError(`[wasm] build_wasm.mjs failed for variant '${variant}'.`, result.status ?? 1);
    }

    // Defensive: verify the build produced the required artifacts so callers can
    // rely on `wasm:ensure` guaranteeing the outputs exist.
    const missing = expectedFiles.filter((file) => !existsSync(file));
    if (missing.length !== 0) {
        throw new EnsureWasmError(
            `[wasm] Build succeeded but some expected wasm-pack outputs are still missing (${variant}):\n` +
                missing.map((p) => `- ${path.relative(repoRoot, p)}`).join("\n"),
            1,
        );
    }
}

export function ensureAll() {
    ensureVariant("single");
    ensureVariant("threaded");
}

function isMainModule() {
    const argv1 = process.argv[1];
    if (!argv1) return false;
    return path.resolve(argv1) === __filename;
}

if (isMainModule()) {
    try {
        ensureAll();
    } catch (err) {
        const status = err instanceof EnsureWasmError ? err.status : 1;
        const message = formatOneLineError(err, 512);
        if (message) {
            console.error(message);
        }
        process.exit(status);
    }
}
