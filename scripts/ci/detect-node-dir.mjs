#!/usr/bin/env node
/**
 * Resolve which directory should be treated as the Node workspace entrypoint.
 *
 * This script is intentionally the single source of truth for Node workspace
 * selection across CI and local tooling (including `cargo xtask`).
 *
 * Output (stdout): key=value lines
 * - dir=<workspace dir>         (relative to repo root when possible; "." means repo root)
 * - lockfile=<package-lock.json> (relative to repo root when possible; empty when missing)
 *
 * Logs are written to stderr.
 */

import { appendFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

function usageAndExit() {
    console.error(
        [
            "Usage: detect-node-dir.mjs [options]",
            "",
            "Options:",
            "  --node-dir <path>        Override workspace directory (same as AERO_NODE_DIR).",
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

function toPosixPath(p) {
    return p.replaceAll("\\", "/");
}

function toRepoRelativePath(repoRoot, absPath) {
    const rel = path.relative(repoRoot, absPath);
    if (rel === "") {
        return ".";
    }
    if (rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel)) {
        return toPosixPath(rel);
    }
    return toPosixPath(absPath);
}

function resolveWorkspace(repoRoot, dirArg, reason) {
    const dirAbs = path.isAbsolute(dirArg) ? path.normalize(dirArg) : path.normalize(path.join(repoRoot, dirArg));
    const pkgJson = path.join(dirAbs, "package.json");
    if (!existsSync(pkgJson)) {
        die(
            `${reason} directory '${toPosixPath(dirArg)}' does not contain package.json. ` +
                "Set AERO_NODE_DIR/--node-dir to a directory that contains package.json.",
        );
    }
    return dirAbs;
}

const argv = process.argv.slice(2);
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

if (!githubOutputPath) {
    githubOutputPath = process.env.GITHUB_OUTPUT?.trim() || null;
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../..");

let workspaceAbs = null;
let resolutionReason = "";

if (overrideDir) {
    workspaceAbs = resolveWorkspace(repoRoot, overrideDir, "override");
    resolutionReason = "override";
} else {
    const candidates = [repoRoot, path.join(repoRoot, "frontend"), path.join(repoRoot, "web")];
    for (const candidate of candidates) {
        if (existsSync(path.join(candidate, "package.json"))) {
            workspaceAbs = candidate;
            resolutionReason = `auto (${toRepoRelativePath(repoRoot, candidate)})`;
            break;
        }
    }
}

if (!workspaceAbs) {
    if (allowMissing) {
        const empty = { dir: "", lockfile: "" };
        process.stdout.write(
            Object.entries(empty)
                .map(([k, v]) => `${k}=${v}`)
                .join("\n") + "\n",
        );
        process.exit(0);
    }
    die("unable to locate package.json; pass --node-dir <path> or set AERO_NODE_DIR");
}

const lockfileAbs = path.join(workspaceAbs, "package-lock.json");
let lockfile = "";
if (existsSync(lockfileAbs)) {
    lockfile = toRepoRelativePath(repoRoot, lockfileAbs);
} else if (requireLockfile) {
    die(
        `package-lock.json not found at '${toRepoRelativePath(repoRoot, lockfileAbs)}'. ` +
            "This workflow expects npm; ensure a lockfile exists.",
    );
}

const out = {
    dir: toRepoRelativePath(repoRoot, workspaceAbs),
    lockfile,
};

if (githubOutputPath) {
    const lines = Object.entries(out)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n");
    appendFileSync(githubOutputPath, `${lines}\n`, { encoding: "utf8" });
}

console.error(`[detect-node-dir] Resolved node dir '${out.dir}' via ${resolutionReason}.`);

process.stdout.write(
    Object.entries(out)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n") + "\n",
);

