#!/usr/bin/env node
/**
 * Resolve which directory should be treated as the Node workspace entrypoint.
 *
 * This script is intentionally the single source of truth for Node workspace
 * selection across CI and local tooling (including `cargo xtask`).
 *
 * Output (stdout): key=value lines
 * - dir=<workspace dir>          (relative to the current working directory when possible; "." means cwd)
 * - lockfile=<package-lock.json> (relative to the current working directory when possible; empty when missing)
 * - package_name=<npm package name> (from package.json; empty when missing)
 * - package_version=<npm package version> (from package.json; empty when missing)
 *
 * Logs are written to stderr.
 */

import { appendFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

function usageAndExit() {
    console.error(
        [
            "Usage: detect-node-dir.mjs [options]",
            "",
            "Options:",
            "  --root <dir>            Checkout root directory to search (default: repo root).",
            "  --node-dir <path>        Override workspace directory (same as AERO_NODE_DIR; deprecated aliases: AERO_WEB_DIR, WEB_DIR).",
            "  --require-lockfile       Fail if package-lock.json is missing in the workspace.",
            "  --allow-missing           Exit 0 with empty outputs when no workspace is found.",
            "  --github-output <path>    Append outputs to the given GitHub output file.",
            "  -h, --help                Show this help.",
        ].join("\n"),
    );
    process.exit(2);
}

function die(message) {
    console.error(`error: ${message}`);
    process.exit(1);
}

function appendGithubOutput(githubOutputPath, lines) {
    const dir = path.dirname(githubOutputPath);
    if (dir && dir !== "." && !existsSync(dir)) {
        mkdirSync(dir, { recursive: true });
    }
    appendFileSync(githubOutputPath, lines, { encoding: "utf8" });
}

function toPosixPath(p) {
    return p.replaceAll("\\", "/");
}

function toOutputRelativePath(outputRoot, absPath) {
    const rel = path.relative(outputRoot, absPath);
    if (rel === "") {
        return ".";
    }
    if (rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel)) {
        return toPosixPath(rel);
    }
    return toPosixPath(absPath);
}

function resolveWorkspace(searchRoot, dirArg, reason) {
    const dirAbs = path.isAbsolute(dirArg) ? path.normalize(dirArg) : path.normalize(path.join(searchRoot, dirArg));
    const pkgJson = path.join(dirAbs, "package.json");
    if (!existsSync(pkgJson)) {
        die(
            `${reason} directory '${toPosixPath(dirArg)}' does not contain package.json. ` +
                "Set AERO_NODE_DIR/--node-dir (deprecated: AERO_WEB_DIR/WEB_DIR) to a directory that contains package.json.",
        );
    }
    return dirAbs;
}

function readPackageInfo(outputRoot, workspaceAbs) {
    const packageJsonAbs = path.join(workspaceAbs, "package.json");
    try {
        const raw = readFileSync(packageJsonAbs, "utf8");
        const parsed = JSON.parse(raw);
        return {
            packageName: typeof parsed?.name === "string" ? parsed.name : "",
            packageVersion: typeof parsed?.version === "string" ? parsed.version : "",
        };
    } catch (err) {
        die(
            `failed to read/parse package.json at '${toOutputRelativePath(outputRoot, packageJsonAbs)}': ` +
                (err instanceof Error ? err.message : String(err)),
        );
    }
}

const argv = process.argv.slice(2);
let rootArg = null;
let overrideDir = null;
let allowMissing = false;
let requireLockfile = false;
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
    if (arg === "--root") {
        const next = argv[i + 1];
        if (!next) {
            die("--root requires a value");
        }
        rootArg = next;
        i++;
        continue;
    }
    if (arg.startsWith("--root=")) {
        rootArg = arg.split("=", 2)[1] ?? "";
        if (!rootArg) {
            die("--root requires a value");
        }
        continue;
    }
    if (arg === "--require-lockfile") {
        requireLockfile = true;
        continue;
    }
    if (arg === "--node-dir" || arg === "--web-dir" || arg === "--dir") {
        const next = argv[i + 1];
        if (!next) {
            die(`${arg} requires a value`);
        }
        overrideDir = next;
        i++;
        continue;
    }
    if (arg.startsWith("--node-dir=") || arg.startsWith("--web-dir=") || arg.startsWith("--dir=")) {
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
    overrideDir = process.env.AERO_NODE_DIR?.trim() || null;
}
if (!overrideDir) {
    overrideDir = process.env.AERO_WEB_DIR?.trim() || null;
}
if (!overrideDir) {
    overrideDir = process.env.WEB_DIR?.trim() || null;
}

if (!githubOutputPath) {
    githubOutputPath = process.env.GITHUB_OUTPUT?.trim() || null;
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../..");
const outputRoot = path.resolve(process.cwd());
const searchRoot = rootArg ? path.resolve(outputRoot, rootArg) : repoRoot;

let workspaceAbs = null;
let resolutionReason = "";

if (overrideDir) {
    workspaceAbs = resolveWorkspace(searchRoot, overrideDir, "override");
    resolutionReason = "override";
} else {
    const candidates = [searchRoot, path.join(searchRoot, "frontend"), path.join(searchRoot, "web")];
    for (const candidate of candidates) {
        if (existsSync(path.join(candidate, "package.json"))) {
            workspaceAbs = candidate;
            resolutionReason = `auto (${toOutputRelativePath(outputRoot, candidate)})`;
            break;
        }
    }
}

if (!workspaceAbs) {
    if (allowMissing) {
        const empty = { dir: "", lockfile: "", package_name: "", package_version: "" };
        if (githubOutputPath) {
            const lines = Object.entries(empty)
                .map(([k, v]) => `${k}=${v}`)
                .join("\n");
            appendGithubOutput(githubOutputPath, `${lines}\n`);
        }

        console.error("[detect-node-dir] No package.json found; returning empty outputs (--allow-missing).");

        process.stdout.write(
            Object.entries(empty)
                .map(([k, v]) => `${k}=${v}`)
                .join("\n") + "\n",
        );
        process.exit(0);
    }
    die(
        "unable to locate package.json; pass --node-dir <path> or set AERO_NODE_DIR (deprecated: AERO_WEB_DIR/WEB_DIR)",
    );
}

const lockfileAbs = path.join(workspaceAbs, "package-lock.json");
let lockfile = "";
if (existsSync(lockfileAbs)) {
    lockfile = toOutputRelativePath(outputRoot, lockfileAbs);
} else if (requireLockfile) {
    const relToRoot = path.relative(searchRoot, workspaceAbs);
    const insideSearchRoot = relToRoot === "" || (!relToRoot.startsWith("..") && !path.isAbsolute(relToRoot));
    const rootLockfileAbs = path.join(searchRoot, "package-lock.json");
    if (insideSearchRoot && existsSync(rootLockfileAbs)) {
        lockfile = toOutputRelativePath(outputRoot, rootLockfileAbs);
    } else {
        die(
            `package-lock.json not found at '${toOutputRelativePath(outputRoot, lockfileAbs)}'. ` +
                "This workflow expects npm; ensure a lockfile exists.",
        );
    }
} else {
    // Workspaces: the lockfile may live at the checkout root even when the selected
    // Node directory is a workspace subdirectory (e.g. AERO_NODE_DIR=web).
    const relToRoot = path.relative(searchRoot, workspaceAbs);
    const insideSearchRoot = relToRoot === "" || (!relToRoot.startsWith("..") && !path.isAbsolute(relToRoot));
    const rootLockfileAbs = path.join(searchRoot, "package-lock.json");
    if (insideSearchRoot && existsSync(rootLockfileAbs)) {
        lockfile = toOutputRelativePath(outputRoot, rootLockfileAbs);
    }
}

const pkg = readPackageInfo(outputRoot, workspaceAbs);

const out = {
    dir: toOutputRelativePath(outputRoot, workspaceAbs),
    lockfile,
    package_name: pkg.packageName,
    package_version: pkg.packageVersion,
};

if (githubOutputPath) {
    const lines = Object.entries(out)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n");
    appendGithubOutput(githubOutputPath, `${lines}\n`);
}

console.error(`[detect-node-dir] Resolved node dir '${out.dir}' via ${resolutionReason}.`);

process.stdout.write(
    Object.entries(out)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n") + "\n",
);
