#!/usr/bin/env node
/**
 * Enforce a strict npm dependency license allowlist (copyleft avoidance).
 *
 * This is intentionally stricter than "flag GPL only": we only allow a short
 * list of permissive SPDX identifiers (plus BlueOak which appears in the
 * existing Node dependency graph).
 *
 * - Unknown/missing license metadata is treated as a CI failure.
 * - SPDX expressions are supported:
 *   - `MIT OR Apache-2.0` is allowed when at least one side is allowlisted.
 *   - `MIT AND Zlib` requires both to be allowlisted.
 * - Private packages (our workspace packages) are excluded via
 *   `license-checker-rseidelsohn --excludePrivatePackages`.
 */

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

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

function usageAndExit() {
    console.error(
        [
            "Usage: check-npm-licenses.mjs --project <dir> [--project <dir> ...] [--out-dir <dir>]",
            "",
            "Options:",
            "  --project <dir>     Project directory (relative to repo root or absolute). Repeatable.",
            "  --out-dir <dir>     Output directory for reports (default: license-reports/npm).",
            "  -h, --help          Show this help.",
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

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../..");

const ALLOWED_LICENSES = new Set([
    "Apache-2.0",
    "MIT",
    // "MIT No Attribution" (permissive; common in modern JS deps).
    "MIT-0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "BSL-1.0",
    "Unicode-3.0",
    "Zlib",
    "0BSD",
    "CC0-1.0",
    // SPDX data packages (e.g. `spdx-exceptions`) are published under CC-BY-3.0.
    "CC-BY-3.0",
    "Unicode-DFS-2016",
    // npm ecosystem: used by several foundational packages (e.g. jackspeak).
    "BlueOak-1.0.0",
]);

const LICENSE_ALIASES = new Map([
    ["Apache 2.0", "Apache-2.0"],
    ["Apache License 2.0", "Apache-2.0"],
]);

function normalizeLicenseId(id) {
    let normalized = id.trim();
    normalized = normalized.replace(/\*+$/, "");
    normalized = LICENSE_ALIASES.get(normalized) ?? normalized;
    return normalized;
}

function tokenizeExpression(expr) {
    const tokens = [];
    let i = 0;
    while (i < expr.length) {
        const ch = expr[i];
        if (ch === "(" || ch === ")") {
            tokens.push(ch);
            i++;
            continue;
        }
        if (/\s/.test(ch)) {
            i++;
            continue;
        }
        const start = i;
        while (i < expr.length) {
            const next = expr[i];
            if (next === "(" || next === ")" || /\s/.test(next)) {
                break;
            }
            i++;
        }
        tokens.push(expr.slice(start, i));
    }
    return tokens;
}

function evaluateLicenseExpression(expr) {
    if (typeof expr !== "string") {
        return false;
    }
    let normalized = expr.replaceAll(";", " OR ").replaceAll(",", " OR ").replaceAll("|", " OR ").trim();
    // `license-checker` occasionally emits a human-readable license name rather than a
    // single SPDX token (e.g. "Apache 2.0"). Normalize these before tokenizing so the
    // whitespace doesn't split the identifier into invalid tokens.
    for (const [alias, canonical] of LICENSE_ALIASES.entries()) {
        normalized = normalized.replaceAll(alias, canonical);
    }
    normalized = normalized.trim();
    if (!normalized) {
        return false;
    }

    const tokens = tokenizeExpression(normalized);
    if (tokens.length === 0) {
        return false;
    }

    let index = 0;

    function peek() {
        return tokens[index];
    }

    function consume() {
        const token = tokens[index];
        index++;
        return token;
    }

    function parseOr() {
        let value = parseAnd();
        while (true) {
            const op = peek();
            if (!op || op.toUpperCase() !== "OR") {
                break;
            }
            consume();
            // Always parse the RHS even if `value` is already true (avoid JS
            // short-circuit semantics breaking the parser state).
            const rhs = parseAnd();
            value = value || rhs;
        }
        return value;
    }

    function parseAnd() {
        let value = parsePrimary();
        while (true) {
            const op = peek();
            if (!op || op.toUpperCase() !== "AND") {
                break;
            }
            consume();
            // Always parse the RHS even if `value` is already false (avoid JS
            // short-circuit semantics breaking the parser state).
            const rhs = parsePrimary();
            value = value && rhs;
        }
        return value;
    }

    function parsePrimary() {
        const token = consume();
        if (!token) {
            throw new Error("unexpected end of license expression");
        }
        if (token === "(") {
            const value = parseOr();
            const closing = consume();
            if (closing !== ")") {
                throw new Error("missing closing ')'");
            }
            return value;
        }
        if (token === ")") {
            throw new Error("unexpected ')'");
        }

        const licenseId = normalizeLicenseId(token);
        if (!licenseId || licenseId === "UNKNOWN" || licenseId === "UNLICENSED") {
            return false;
        }
        return ALLOWED_LICENSES.has(licenseId);
    }

    const value = parseOr();
    if (index !== tokens.length) {
        throw new Error(`unexpected trailing token '${tokens[index]}'`);
    }
    return value;
}

function isAllowlisted(licenses) {
    if (!licenses) {
        return false;
    }
    if (Array.isArray(licenses)) {
        return licenses.some((entry) => isAllowlisted(entry));
    }
    if (typeof licenses !== "string") {
        return false;
    }

    try {
        return evaluateLicenseExpression(licenses);
    } catch {
        return false;
    }
}

function isThirdPartyPath(packagePath) {
    return packagePath.includes(`${path.sep}node_modules${path.sep}`) || packagePath.includes(`/node_modules/`);
}

function slugForProject(projectRel) {
    if (projectRel === "." || projectRel === "") {
        return "root";
    }
    return projectRel.replaceAll("\\", "/").replaceAll("/", "__");
}

export { ALLOWED_LICENSES, LICENSE_ALIASES, evaluateLicenseExpression, isAllowlisted, normalizeLicenseId, tokenizeExpression };

function runCli(argv) {
    let outDir = null;
    const projects = [];

    for (let i = 0; i < argv.length; i++) {
        const arg = argv[i];
        if (arg === "-h" || arg === "--help") {
            usageAndExit();
        }
        if (arg === "--project") {
            const next = argv[i + 1];
            if (!next) {
                die("--project requires a value");
            }
            projects.push(next);
            i++;
            continue;
        }
        if (arg.startsWith("--project=")) {
            const value = arg.split("=", 2)[1] ?? "";
            if (!value) {
                die("--project requires a value");
            }
            projects.push(value);
            continue;
        }
        if (arg === "--out-dir") {
            const next = argv[i + 1];
            if (!next) {
                die("--out-dir requires a value");
            }
            outDir = next;
            i++;
            continue;
        }
        if (arg.startsWith("--out-dir=")) {
            const value = arg.split("=", 2)[1] ?? "";
            if (!value) {
                die("--out-dir requires a value");
            }
            outDir = value;
            continue;
        }

        die(`unknown argument: ${arg}`);
    }

    if (projects.length === 0) {
        die("at least one --project <dir> must be provided");
    }

    const outDirAbs = path.resolve(repoRoot, outDir ?? "license-reports/npm");
    mkdirSync(outDirAbs, { recursive: true });

    const require = createRequire(import.meta.url);
    let licenseCheckerBin = null;

    try {
        const pkgJsonPath = require.resolve("license-checker-rseidelsohn/package.json");
        const pkgJson = JSON.parse(readFileSync(pkgJsonPath, "utf8"));
        const pkgDir = path.dirname(pkgJsonPath);
        const bin = pkgJson.bin;
        let binRel = null;
        if (typeof bin === "string") {
            binRel = bin;
        } else if (bin && typeof bin === "object") {
            binRel = bin["license-checker-rseidelsohn"] ?? Object.values(bin)[0] ?? null;
        }
        if (!binRel) {
            throw new Error("unable to resolve license-checker-rseidelsohn bin from package.json");
        }
        licenseCheckerBin = path.resolve(pkgDir, binRel);
    } catch (err) {
        die(
            `unable to locate license-checker-rseidelsohn. ` +
                `Run 'npm ci' in the repo root before running this script.\n` +
                `details: ${formatOneLineError(err, 512)}`,
        );
    }

    const results = [];
    let totalViolations = 0;

    for (const project of projects) {
        const projectAbs = path.isAbsolute(project)
            ? path.normalize(project)
            : path.normalize(path.join(repoRoot, project));
        const projectRel = toRepoRelativePath(repoRoot, projectAbs);
        const slug = slugForProject(projectRel);

        if (!existsSync(path.join(projectAbs, "package.json"))) {
            die(`project '${projectRel}' does not contain package.json`);
        }
        // npm workspaces: dependencies are typically installed at the repo root even when
        // the project being scanned is a workspace subdirectory.
        const projectNodeModules = path.join(projectAbs, "node_modules");
        const repoNodeModules = path.join(repoRoot, "node_modules");
        if (!existsSync(projectNodeModules) && !existsSync(repoNodeModules)) {
            die(
                `project '${projectRel}' is missing node_modules; ` +
                    `run 'npm ci --ignore-scripts' in the repo root before running this script`,
            );
        }

        const proc = spawnSync(
            process.execPath,
            [
                licenseCheckerBin,
                "--json",
                "--excludePrivatePackages",
                "--start",
                projectAbs,
            ],
            { encoding: "utf8", maxBuffer: 1024 * 1024 * 50 },
        );

        if (proc.status !== 0) {
            const stderr = proc.stderr?.trim();
            die(
                `license-checker-rseidelsohn failed for '${projectRel}' (exit ${proc.status}).` +
                    (stderr ? `\n${stderr}` : ""),
            );
        }

        let report = null;
        try {
            report = JSON.parse(proc.stdout);
        } catch (err) {
            die(
                `failed to parse license-checker output for '${projectRel}'. ` +
                    `Ensure the tool is producing valid JSON.\n` +
                    `details: ${formatOneLineError(err, 512)}`,
            );
        }

        const violations = [];
        const entries = Object.entries(report).sort(([a], [b]) => a.localeCompare(b));
        for (const [pkg, info] of entries) {
            const pkgPath = info?.path;
            if (!pkgPath || typeof pkgPath !== "string") {
                continue;
            }
            if (!isThirdPartyPath(pkgPath)) {
                continue;
            }

            const licenseValue = info.licenses;
            if (!isAllowlisted(licenseValue)) {
                violations.push({
                    package: pkg,
                    licenses: licenseValue,
                    licenseFile: info.licenseFile ?? null,
                    path: pkgPath,
                });
            }
        }

        const reportPath = path.join(outDirAbs, `${slug}.json`);
        writeFileSync(reportPath, JSON.stringify(report, null, 2) + "\n", { encoding: "utf8" });

        const summaryLines = [];
        summaryLines.push(`Project: ${projectRel}`);
        summaryLines.push(`Allowlist: ${Array.from(ALLOWED_LICENSES).sort().join(", ")}`);
        summaryLines.push(`Total dependencies scanned: ${entries.length}`);
        summaryLines.push(`Violations: ${violations.length}`);
        summaryLines.push("");

        if (violations.length) {
            summaryLines.push("Disallowed dependencies:");
            for (const v of violations) {
                summaryLines.push(
                    `- ${v.package}: ${typeof v.licenses === "string" ? v.licenses : JSON.stringify(v.licenses)}`,
                );
                if (v.licenseFile) {
                    summaryLines.push(`  licenseFile: ${v.licenseFile}`);
                }
                summaryLines.push(`  path: ${v.path}`);
            }
        } else {
            summaryLines.push("No disallowed dependency licenses found.");
        }

        const summaryPath = path.join(outDirAbs, `${slug}.summary.txt`);
        writeFileSync(summaryPath, summaryLines.join("\n") + "\n", { encoding: "utf8" });

        results.push({
            project: projectRel,
            reportPath: toRepoRelativePath(repoRoot, reportPath),
            violations: violations.length,
        });
        totalViolations += violations.length;
    }

    if (process.env.GITHUB_STEP_SUMMARY) {
        const lines = [];
        lines.push("### npm license allowlist");
        lines.push("");
        lines.push("| project | violations | report |");
        lines.push("| --- | ---: | --- |");
        for (const result of results) {
            lines.push(`| \`${result.project}\` | ${result.violations} | \`${result.reportPath}\` |`);
        }
        lines.push("");
        writeFileSync(process.env.GITHUB_STEP_SUMMARY, lines.join("\n") + "\n", { encoding: "utf8", flag: "a" });
    }

    if (totalViolations > 0) {
        die(
            `found ${totalViolations} npm dependencies with disallowed licenses; see ${toRepoRelativePath(repoRoot, outDirAbs)}`,
        );
    }

    console.error(`[check-npm-licenses] OK (${results.length} projects scanned).`);
}

const mainPath = process.argv[1] ? pathToFileURL(process.argv[1]).href : "";
if (import.meta.url === mainPath) {
    runCli(process.argv.slice(2));
}
