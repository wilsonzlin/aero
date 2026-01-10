import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, "../..");

function ensureVariant(variant) {
    const outDir = path.join(repoRoot, "web/src/wasm", variant === "threaded" ? "pkg-threaded" : "pkg-single");
    const jsEntry = path.join(outDir, "aero_wasm.js");
    const wasmBinary = path.join(outDir, "aero_wasm_bg.wasm");

    if (existsSync(jsEntry) && existsSync(wasmBinary)) {
        return;
    }

    const result = spawnSync("node", [path.join(__dirname, "build_wasm.mjs"), variant], { stdio: "inherit" });
    if ((result.status ?? 1) !== 0) {
        process.exit(result.status ?? 1);
    }
}

ensureVariant("single");
ensureVariant("threaded");
