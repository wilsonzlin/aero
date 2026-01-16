import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export function fail(message) {
  process.stderr.write(`${String(message)}\n`);
  process.exit(1);
}

export function requireEnv(name) {
  const value = process.env[name];
  if (!value) fail(`error: ${name} is not set`);
  return value;
}

export function appendLine(filePath, line) {
  fs.appendFileSync(filePath, `${line}\n`, "utf8");
}

export function appendKeyValue(filePath, key, value) {
  appendLine(filePath, `${key}=${value}`);
}

export function appendOutput(key, value) {
  appendKeyValue(requireEnv("GITHUB_OUTPUT"), key, value);
}

export function appendEnv(key, value) {
  appendKeyValue(requireEnv("GITHUB_ENV"), key, value);
}

export function ensureFileExists(filePath) {
  if (!fs.existsSync(filePath)) fs.writeFileSync(filePath, "", "utf8");
}

export function resolveWorkspaceRoot() {
  return process.env.GITHUB_WORKSPACE ?? process.cwd();
}

export function normalizeRel(p) {
  const s = String(p ?? "")
    .replaceAll("\\", "/")
    .replace(/\/+$/u, "")
    .replace(/^\.\//u, "");
  return s || ".";
}

export function expandHome(p) {
  const s = String(p ?? "").trim();
  if (s === "~") return os.homedir();
  if (s.startsWith("~/") || s.startsWith("~\\")) return path.join(os.homedir(), s.slice(2));
  return s;
}

