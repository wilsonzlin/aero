#!/usr/bin/env node
/**
 * Resolve which Rust crate should be treated as the wasm-pack entrypoint.
 *
 * This script is intentionally the single source of truth for WASM crate
 * selection across CI and local tooling.
 *
 * Output (stdout): key=value lines
 * - dir=<crate dir>            (relative to repo root when possible)
 * - manifest_path=<Cargo.toml> (relative to repo root when possible)
 * - crate_name=<cargo package name>
 *
 * Logs are written to stderr.
 */

import { spawnSync } from "node:child_process";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

function usageAndExit() {
    // Keep usage terse: this is mainly consumed by CI scripts/actions.
    console.error(
        [
            "Usage: detect-wasm-crate.mjs [options]",
            "",
            "Options:",
            "  --wasm-crate-dir <path>  Override crate directory (same as AERO_WASM_CRATE_DIR).",
            "  --allow-missing          Exit 0 with empty outputs when no crate is found.",
            "  --github-output <path>   Append outputs to the given GitHub output file.",
            "  -h, --help               Show this help.",
        ].join("\n"),
    );
    process.exit(2);
}

function die(message) {
    console.error(`error: ${message}`);
    process.exit(1);
}

function toPosixPath(p) {
    return p.replaceAll("\\", "/");
}

function toRepoRelativePath(repoRoot, absPath) {
    const rel = path.relative(repoRoot, absPath);
    if (rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel)) {
        return toPosixPath(rel);
    }
    return toPosixPath(absPath);
}

function parsePackageNameFromCargoToml(manifestPath) {
    const raw = readFileSync(manifestPath, "utf8");
    let currentSection = "";
    for (const line of raw.split(/\r?\n/u)) {
        const trimmed = line.trim();
        if (!trimmed || trimmed.startsWith("#")) {
            continue;
        }

        const sectionMatch = trimmed.match(/^\[([^\]]+)\]$/u);
        if (sectionMatch) {
            currentSection = sectionMatch[1] ?? "";
            continue;
        }

        if (currentSection !== "package") {
            continue;
        }

        // Strip end-of-line comments (best-effort; TOML parsing is intentionally lightweight here).
        const noComment = trimmed.split("#")[0]?.trim() ?? "";
        const nameMatch = noComment.match(/^name\s*=\s*["']([^"']+)["']\s*$/u);
        if (nameMatch) {
            return nameMatch[1];
        }
    }

    throw new Error("failed to parse [package].name from Cargo.toml");
}

function resolveCrateFromDir(repoRoot, dirArg, reason) {
    const dirAbs = path.isAbsolute(dirArg) ? path.normalize(dirArg) : path.normalize(path.join(repoRoot, dirArg));
    const manifestAbs = path.join(dirAbs, "Cargo.toml");
    if (!existsSync(manifestAbs)) {
        die(
            `${reason} directory '${toPosixPath(dirArg)}' does not contain Cargo.toml. ` +
                "Set AERO_WASM_CRATE_DIR/--wasm-crate-dir to a crate directory that contains Cargo.toml.",
        );
    }

    let crateName;
    try {
        crateName = parsePackageNameFromCargoToml(manifestAbs);
    } catch (err) {
        die(
            `${reason} Cargo.toml at '${toRepoRelativePath(repoRoot, manifestAbs)}' does not look like a Rust crate manifest ` +
                "(missing [package].name).",
        );
    }

    return { dirAbs, manifestAbs, crateName };
}

function runCargoMetadata(repoRoot) {
    const result = spawnSync("cargo", ["metadata", "--no-deps", "--format-version=1"], {
        cwd: repoRoot,
        stdio: ["ignore", "pipe", "pipe"],
        encoding: "utf8",
    });
    if ((result.status ?? 1) !== 0) {
        const details = (result.stderr || result.stdout || "").trim();
        die(`cargo metadata failed.\n\n${details}`);
    }
    try {
        return JSON.parse(result.stdout);
    } catch (err) {
        die("cargo metadata returned invalid JSON.");
    }
}

const argv = process.argv.slice(2);
let overrideDir = null;
let allowMissing = false;
let githubOutputPath = null;

for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "-h" || arg === "--help") {
        usageAndExit();
    }
    if (arg === "--allow-missing" || arg === "--optional") {
        allowMissing = true;
        continue;
    }
    if (arg === "--wasm-crate-dir" || arg === "--crate-dir" || arg === "--dir") {
        const next = argv[i + 1];
        if (!next) {
            die(`${arg} requires a value`);
        }
        overrideDir = next;
        i++;
        continue;
    }
    if (arg.startsWith("--wasm-crate-dir=") || arg.startsWith("--crate-dir=") || arg.startsWith("--dir=")) {
        overrideDir = arg.split("=", 2)[1] ?? "";
        if (!overrideDir) {
            die(`${arg.split("=", 1)[0]} requires a value`);
        }
        continue;
    }
    if (arg === "--github-output") {
        const next = argv[i + 1];
        if (!next) {
            die("--github-output requires a value");
        }
        githubOutputPath = next;
        i++;
        continue;
    }
    if (arg.startsWith("--github-output=")) {
        githubOutputPath = arg.split("=", 2)[1] ?? "";
        if (!githubOutputPath) {
            die("--github-output requires a value");
        }
        continue;
    }

    die(`unknown argument: ${arg}`);
}

if (!overrideDir) {
    overrideDir = process.env.AERO_WASM_CRATE_DIR?.trim() || null;
}

if (!githubOutputPath) {
    githubOutputPath = process.env.GITHUB_OUTPUT?.trim() || null;
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../..");

const canonicalCandidates = ["crates/aero-wasm"];

let resolved = null;
let resolutionReason = "";

if (overrideDir) {
    resolved = resolveCrateFromDir(repoRoot, overrideDir, "override");
    resolutionReason = "override";
} else {
    for (const candidate of canonicalCandidates) {
        const abs = path.join(repoRoot, candidate);
        const manifestAbs = path.join(abs, "Cargo.toml");
        if (existsSync(manifestAbs)) {
            resolved = resolveCrateFromDir(repoRoot, candidate, "canonical");
            resolutionReason = `canonical (${candidate})`;
            break;
        }
    }
}

if (!resolved) {
    const rootManifest = path.join(repoRoot, "Cargo.toml");
    if (!existsSync(rootManifest)) {
        if (allowMissing) {
            // A repo can legitimately be missing Rust sources during early bootstrapping.
            // Keep local tooling ergonomic by allowing "not found" to be non-fatal when asked.
            const empty = { dir: "", manifest_path: "", crate_name: "" };
            process.stdout.write(
                Object.entries(empty)
                    .map(([k, v]) => `${k}=${v}`)
                    .join("\n") + "\n",
            );
            process.exit(0);
        }
        die("Cargo.toml not found at repo root and no override provided (AERO_WASM_CRATE_DIR).");
    }

    const metadata = runCargoMetadata(repoRoot);
    const packages = metadata.packages ?? [];
    const cdylibPkgs = [];
    for (const pkg of packages) {
        const targets = pkg.targets ?? [];
        const isCdylib = targets.some((tgt) => (tgt.kind ?? []).includes("cdylib"));
        if (isCdylib) {
            const manifestPath = pkg.manifest_path ?? "";
            if (!manifestPath) {
                continue;
            }
            cdylibPkgs.push({
                name: pkg.name ?? "",
                manifest_path: manifestPath,
                dir: path.dirname(manifestPath),
            });
        }
    }

    if (cdylibPkgs.length === 0) {
        if (allowMissing) {
            const empty = { dir: "", manifest_path: "", crate_name: "" };
            process.stdout.write(
                Object.entries(empty)
                    .map(([k, v]) => `${k}=${v}`)
                    .join("\n") + "\n",
            );
            process.exit(0);
        }
        die(
            "unable to auto-detect a wasm-pack crate (no workspace packages expose a cdylib target). " +
                "Set AERO_WASM_CRATE_DIR/--wasm-crate-dir to the crate directory containing Cargo.toml.",
        );
    }

    if (cdylibPkgs.length > 1) {
        const lines = cdylibPkgs
            .map((pkg) => {
                const dirRel = toRepoRelativePath(repoRoot, pkg.dir);
                const name = pkg.name || "<unknown>";
                return `- ${name}: ${dirRel}`;
            })
            .join("\n");
        die(
            [
                "multiple workspace crates expose a cdylib target, so the WASM crate is ambiguous:",
                lines,
                "",
                "Set AERO_WASM_CRATE_DIR (or pass --wasm-crate-dir) to choose the correct crate directory.",
            ].join("\n"),
        );
    }

    const pkg = cdylibPkgs[0];
    resolved = resolveCrateFromDir(repoRoot, pkg.dir, "auto-detected");
    resolutionReason = "cargo metadata (single cdylib)";
}

const out = {
    dir: toRepoRelativePath(repoRoot, resolved.dirAbs),
    manifest_path: toRepoRelativePath(repoRoot, resolved.manifestAbs),
    crate_name: resolved.crateName,
};

if (githubOutputPath) {
    const lines = Object.entries(out)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n");
    appendFileSync(githubOutputPath, `${lines}\n`, { encoding: "utf8" });
}

console.error(
    `[detect-wasm-crate] Resolved wasm crate '${out.crate_name}' (${out.dir}) via ${resolutionReason}.`,
);

process.stdout.write(
    Object.entries(out)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n") + "\n",
);
