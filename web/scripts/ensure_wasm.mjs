import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, "../..");

function ensureVariant(variant) {
    const outDirAero = path.join(repoRoot, "web/src/wasm", variant === "threaded" ? "pkg-threaded" : "pkg-single");
    const outDirAeroGpu = path.join(
        repoRoot,
        "web/src/wasm",
        variant === "threaded" ? "pkg-threaded-gpu" : "pkg-single-gpu",
    );

    const expectedFiles = [
        path.join(outDirAero, "aero_wasm.js"),
        path.join(outDirAero, "aero_wasm_bg.wasm"),
        path.join(outDirAeroGpu, "aero_gpu_wasm.js"),
        path.join(outDirAeroGpu, "aero_gpu_wasm_bg.wasm"),
    ];

    if (expectedFiles.every((file) => existsSync(file))) {
        return;
    }

    const result = spawnSync("node", [path.join(__dirname, "build_wasm.mjs"), variant], { stdio: "inherit" });
    if ((result.status ?? 1) !== 0) {
        process.exit(result.status ?? 1);
    }
}

ensureVariant("single");
ensureVariant("threaded");
