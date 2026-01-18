import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

function fallbackFormatOneLineError(err, maxLen = 512) {
    let msg = "Error";
    try {
        if (typeof err === "string") msg = err;
        else if (err && typeof err === "object" && typeof err.message === "string" && err.message) msg = err.message;
    } catch {
        // ignore hostile getters
    }
    try {
        msg = String(msg).replace(/\s+/gu, " ").trim();
    } catch {
        msg = "Error";
    }
    if (!Number.isInteger(maxLen) || maxLen <= 0) return "";
    if (msg.length > maxLen) msg = msg.slice(0, maxLen);
    return msg || "Error";
}

let formatOneLineError = fallbackFormatOneLineError;
try {
    const mod = await import(new URL("../../src/text.js", import.meta.url));
    if (typeof mod?.formatOneLineError === "function") {
        formatOneLineError = mod.formatOneLineError;
    }
} catch {
    // ignore - fallback stays active
}

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
const setupRustActionPath = path.join(repoRoot, ".github/actions/setup-rust/action.yml");

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
        // Workflows should install Rust via our pinned wrapper action (`./.github/actions/setup-rust`) so
        // `toolchain: stable`/`toolchain: nightly` always resolve to the repo-pinned versions.
        { pattern: /\bdtolnay\/rust-toolchain@/u, message: "uses dtolnay/rust-toolchain directly" },
        {
            pattern: /\btoolchain:\s*["']?\d+\.\d+\.\d+\b/u,
            message: "hardcodes a stable toolchain version",
        },
        {
            pattern: /\btoolchain:\s*["']?nightly-\d{4}-\d{2}-\d{2}\b/u,
            message: "hardcodes a pinned nightly toolchain date",
        },
        {
            pattern: /\bcargo\s+\+nightly-\d{4}-\d{2}-\d{2}\b/u,
            message: "hardcodes a pinned nightly toolchain date via `cargo +nightly-YYYY-MM-DD`",
        },
        {
            pattern: /\brustc\s+\+nightly-\d{4}-\d{2}-\d{2}\b/u,
            message: "hardcodes a pinned nightly toolchain date via `rustc +nightly-YYYY-MM-DD`",
        },
        {
            pattern: /\brustup\s+toolchain\s+install\s+nightly-\d{4}-\d{2}-\d{2}\b/u,
            message: "hardcodes a pinned nightly toolchain date via `rustup toolchain install nightly-YYYY-MM-DD`",
        },
        {
            pattern: /--toolchain\s+nightly-\d{4}-\d{2}-\d{2}\b/u,
            message: "hardcodes a pinned nightly toolchain date via `--toolchain nightly-YYYY-MM-DD`",
        },
        {
            pattern: /\bRUSTUP_TOOLCHAIN\b\s*[:=]\s*["']?nightly-\d{4}-\d{2}-\d{2}\b/u,
            message: "hardcodes a pinned nightly toolchain date via RUSTUP_TOOLCHAIN=nightly-YYYY-MM-DD",
        },
        {
            pattern: /\bRUSTUP_TOOLCHAIN\b\s*[:=]\s*["']?nightly(?!-)\b/u,
            message: "uses floating nightly via RUSTUP_TOOLCHAIN=nightly",
        },
        {
            pattern: /\bRUSTUP_TOOLCHAIN\b\s*[:=]\s*["']?stable\b/u,
            message: "uses floating stable via RUSTUP_TOOLCHAIN=stable",
        },
        {
            pattern: /\bRUSTUP_TOOLCHAIN\b\s*[:=]\s*["']?\d+\.\d+\.\d+\b/u,
            message: "hardcodes a stable toolchain version via RUSTUP_TOOLCHAIN=1.xx.y",
        },
        { pattern: /\bcargo\s+\+nightly(?!-)/u, message: "uses floating cargo +nightly" },
        { pattern: /\brustc\s+\+nightly(?!-)/u, message: "uses floating rustc +nightly" },
        { pattern: /\brustup\s+toolchain\s+install\s+nightly(?!-)/u, message: "installs floating rustup nightly" },
        { pattern: /--toolchain\s+nightly(?!-)/u, message: "references --toolchain nightly" },
    ];

    for (const filePath of workflowFiles) {
        const rel = path.relative(repoRoot, filePath).replaceAll("\\", "/");
        const content = readFileSync(filePath, "utf8");
        for (const { pattern, message } of forbiddenPatterns) {
            if (pattern.test(content)) {
                fail(
                    `${rel} ${message}; workflows should install Rust via ./.github/actions/setup-rust ` +
                        "(toolchain: stable/nightly) so toolchain pins come from rust-toolchain.toml and scripts/toolchains.json.",
                );
            }
        }
    }
}

const rustToolchainToml = readFileSync(rustToolchainTomlPath, "utf8");
// Allow trailing comments after the channel assignment so the file can be annotated without
// breaking CI policy checks.
const channelMatch = rustToolchainToml.match(/^\s*channel\s*=\s*"([^"]+)"\s*(?:#.*)?$/m);
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
    fail(`Failed to parse ${toolchainsJsonPath}: ${formatOneLineError(err, 512)}`);
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

const setupRustAction = readFileSync(setupRustActionPath, "utf8");
if (!setupRustAction.includes("scripts/toolchains.json") || !setupRustAction.includes("nightlyWasm")) {
    fail(
        ".github/actions/setup-rust/action.yml must resolve the pinned nightly toolchain from scripts/toolchains.json " +
            "(rust.nightlyWasm) when callers request 'nightly'.",
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

    if (/\brustup\s+toolchain\s+install\s+nightly(?!-)/.test(trimmed)) {
        fail(`justfile:${i + 1} installs unpinned nightly; use scripts/toolchains.json (rust.nightlyWasm).`);
    }
    if (/--toolchain\s+nightly(?!-)/.test(trimmed)) {
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
if (/\brustup\s+toolchain\s+install\s+nightly(?!-)/.test(devcontainerDockerfile)) {
    fail(".devcontainer/Dockerfile installs floating nightly ('rustup toolchain install nightly'); it must use the pinned nightly.");
}
if (/--toolchain\s+nightly(?!-)/.test(devcontainerDockerfile)) {
    fail(".devcontainer/Dockerfile references '--toolchain nightly'; it must use the pinned nightly-YYYY-MM-DD toolchain.");
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
