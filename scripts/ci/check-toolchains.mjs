import { readdirSync, readFileSync, statSync } from "node:fs";
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
const devcontainerDockerfilePath = path.join(repoRoot, ".devcontainer/Dockerfile");
const fuzzWorkflowPath = path.join(repoRoot, ".github/workflows/fuzz.yml");

function listFilesRecursive(dirPath) {
    const entries = readdirSync(dirPath, { withFileTypes: true });
    const files = [];
    for (const entry of entries) {
        const fullPath = path.join(dirPath, entry.name);
        if (entry.isDirectory()) {
            files.push(...listFilesRecursive(fullPath));
            continue;
        }
        if (entry.isFile()) {
            files.push(fullPath);
        } else if (entry.isSymbolicLink()) {
            // Ignore symlinks in CI checks to avoid surprising traversal.
            // (GitHub checkouts typically do not contain symlinks here anyway.)
            continue;
        } else {
            // Ignore sockets/etc.
            continue;
        }
    }
    return files;
}

function assertNoFloatingNightlyInWorkflows(workflowsDir) {
    if (!statSync(workflowsDir, { throwIfNoEntry: false })?.isDirectory()) {
        return;
    }
    const workflowFiles = listFilesRecursive(workflowsDir).filter((file) => file.endsWith(".yml") || file.endsWith(".yaml"));
    const forbiddenPatterns = [
        { pattern: /\brust-toolchain@nightly\b/, message: "installs Rust via dtolnay/rust-toolchain@nightly" },
        { pattern: /\btoolchain:\s*nightly\b/, message: "uses floating toolchain: nightly" },
        { pattern: /\bcargo\s+\+nightly\b/, message: "uses floating cargo +nightly" },
        { pattern: /\brustc\s+\+nightly\b/, message: "uses floating rustc +nightly" },
        { pattern: /\brustup\s+toolchain\s+install\s+nightly\b/, message: "installs floating rustup nightly" },
        { pattern: /--toolchain\s+nightly\b/, message: "references --toolchain nightly" },
    ];

    for (const filePath of workflowFiles) {
        const rel = path.relative(repoRoot, filePath).replaceAll("\\", "/");
        const content = readFileSync(filePath, "utf8");
        for (const { pattern, message } of forbiddenPatterns) {
            if (pattern.test(content)) {
                fail(`${rel} ${message}; use scripts/toolchains.json (rust.nightlyWasm) instead.`);
            }
        }
    }
}

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
if (!/env\.RUSTUP_TOOLCHAIN\s*=\s*wasmThreadedToolchain\b/.test(wasmBuildScript)) {
    fail(
        "web/scripts/build_wasm.mjs must set env.RUSTUP_TOOLCHAIN from the pinned toolchain loaded from scripts/toolchains.json " +
            "(expected assignment to wasmThreadedToolchain).",
    );
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

const devcontainerDockerfile = readFileSync(devcontainerDockerfilePath, "utf8");
if (
    !devcontainerDockerfile.includes("rust-toolchain.toml") ||
    !devcontainerDockerfile.includes("scripts/toolchains.json")
) {
    fail(
        ".devcontainer/Dockerfile must install Rust toolchains from the repo's pinned sources " +
            "(rust-toolchain.toml + scripts/toolchains.json).",
    );
}
if (/\b--default-toolchain\s+stable\b/.test(devcontainerDockerfile)) {
    fail(".devcontainer/Dockerfile uses floating stable ('--default-toolchain stable'); it must install the pinned version.");
}
if (/\brustup\s+toolchain\s+install\s+nightly\b/.test(devcontainerDockerfile)) {
    fail(".devcontainer/Dockerfile installs floating nightly ('rustup toolchain install nightly'); it must use the pinned nightly.");
}
if (/--toolchain\s+nightly\b/.test(devcontainerDockerfile)) {
    fail(".devcontainer/Dockerfile references '--toolchain nightly'; it must use the pinned nightly-YYYY-MM-DD toolchain.");
}

const fuzzWorkflow = readFileSync(fuzzWorkflowPath, "utf8");
if (!fuzzWorkflow.includes("scripts/toolchains.json")) {
    fail(".github/workflows/fuzz.yml must read the pinned nightly toolchain from scripts/toolchains.json.");
}
if (/\btoolchain:\s*nightly\b/.test(fuzzWorkflow)) {
    fail(".github/workflows/fuzz.yml uses floating 'toolchain: nightly'; it must use the pinned nightly toolchain.");
}
if (/\bcargo\s+\+nightly\b/.test(fuzzWorkflow)) {
    fail(".github/workflows/fuzz.yml uses floating 'cargo +nightly'; it must use the pinned nightly toolchain.");
}
if (/\brust-toolchain@nightly\b/.test(fuzzWorkflow)) {
    fail(".github/workflows/fuzz.yml installs Rust via dtolnay/rust-toolchain@nightly; it must use the pinned nightly toolchain.");
}

assertNoFloatingNightlyInWorkflows(path.join(repoRoot, ".github/workflows"));

process.stdout.write(
    [
        "toolchain check ok",
        `- stable: ${stableChannel} (pinned)`,
        `- nightly wasm: ${wasmNightly.trim()} (pinned)`,
        "",
    ].join("\n"),
);
