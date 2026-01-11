import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

function fail(message) {
    console.error(`toolchain check failed: ${message}`);
    process.exit(1);
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../..");

const rustToolchainTomlPath = path.join(repoRoot, "rust-toolchain.toml");
const toolchainsJsonPath = path.join(repoRoot, "scripts/toolchains.json");
const wasmBuildScriptPath = path.join(repoRoot, "web/scripts/build_wasm.mjs");
const justfilePath = path.join(repoRoot, "justfile");

const rustToolchainToml = readFileSync(rustToolchainTomlPath, "utf8");
const channelMatch = rustToolchainToml.match(/^\s*channel\s*=\s*"([^"]+)"\s*$/m);
if (!channelMatch) {
    fail(`Unable to find [toolchain].channel in ${rustToolchainTomlPath}`);
}

const stableChannel = channelMatch[1].trim();
if (!/^\d+\.\d+\.\d+$/.test(stableChannel)) {
    fail(
        `rust-toolchain.toml must pin stable to an explicit version (expected 1.xx.y; got '${stableChannel}'). ` +
            "See docs/adr/0009-rust-toolchain-policy.md.",
    );
}

let toolchains;
try {
    toolchains = JSON.parse(readFileSync(toolchainsJsonPath, "utf8"));
} catch (err) {
    fail(`Failed to parse ${toolchainsJsonPath}: ${err?.message ?? String(err)}`);
}

const wasmNightly = toolchains?.rust?.nightlyWasm;
if (typeof wasmNightly !== "string" || wasmNightly.trim() === "") {
    fail(`scripts/toolchains.json must define rust.nightlyWasm (string)`);
}
if (!/^nightly-\d{4}-\d{2}-\d{2}$/.test(wasmNightly.trim())) {
    fail(`rust.nightlyWasm must be pinned to a nightly date (nightly-YYYY-MM-DD); got '${wasmNightly}'`);
}

const wasmBuildScript = readFileSync(wasmBuildScriptPath, "utf8");
if (!wasmBuildScript.includes("scripts/toolchains.json") || !wasmBuildScript.includes("nightlyWasm")) {
    fail(
        `web/scripts/build_wasm.mjs must load the pinned nightly toolchain from scripts/toolchains.json (rust.nightlyWasm).`,
    );
}

if (/\+nightly(?!-)/.test(wasmBuildScript)) {
    fail("web/scripts/build_wasm.mjs uses unpinned '+nightly' (must use a pinned nightly-YYYY-MM-DD toolchain).");
}
if (/env\.RUSTUP_TOOLCHAIN\s*=\s*["']/.test(wasmBuildScript)) {
    fail("web/scripts/build_wasm.mjs sets RUSTUP_TOOLCHAIN to a string literal (must come from scripts/toolchains.json).");
}

const justfile = readFileSync(justfilePath, "utf8");
if (!justfile.includes("scripts/toolchains.json") || !justfile.includes("nightlyWasm")) {
    fail(`justfile must read the pinned nightly toolchain from scripts/toolchains.json (rust.nightlyWasm).`);
}
const justfileLines = justfile.split(/\r?\n/);
for (let i = 0; i < justfileLines.length; i += 1) {
    const line = justfileLines[i];
    const trimmed = line.trimStart();
    if (trimmed === "" || trimmed.startsWith("#")) {
        continue;
    }

    if (/\brustup\s+toolchain\s+install\s+nightly\b/.test(trimmed)) {
        fail(`justfile:${i + 1} installs unpinned nightly; use scripts/toolchains.json (rust.nightlyWasm).`);
    }
    if (/--toolchain\s+nightly\b/.test(trimmed)) {
        fail(`justfile:${i + 1} references unpinned '--toolchain nightly'; use scripts/toolchains.json (rust.nightlyWasm).`);
    }
}

process.stdout.write(
    [
        "toolchain check ok",
        `- stable: ${stableChannel} (pinned)`,
        `- nightly wasm: ${wasmNightly.trim()} (pinned)`,
        "",
    ].join("\n"),
);
