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

export function appendKeyValue(filePath, key, value, multilineHint = "") {
  const k = String(key ?? "");
  const v = String(value ?? "");
  if (k.includes("\n") || k.includes("\r")) {
    fail(`error: key contains a newline: ${JSON.stringify(k)}`);
  }
  if (v.includes("\n") || v.includes("\r")) {
    const hint = multilineHint ? ` (use ${multilineHint} for multiline values)` : " (use a multiline output helper)";
    fail(`error: value for ${k} contains a newline${hint}`);
  }
  appendLine(filePath, `${k}=${v}`);
}

export function appendMultilineKeyValue(filePath, key, value, delimiterPrefix = "EOF") {
  const k = String(key ?? "");
  if (k.includes("\n") || k.includes("\r")) {
    fail(`error: key contains a newline: ${JSON.stringify(k)}`);
  }
  const body = String(value ?? "");
  let delimiter = `${delimiterPrefix}_${Math.random().toString(16).slice(2)}`;
  while (body.includes(delimiter)) {
    delimiter = `${delimiterPrefix}_${Math.random().toString(16).slice(2)}`;
  }
  fs.appendFileSync(filePath, `${k}<<${delimiter}\n${body}\n${delimiter}\n`, "utf8");
}

export function appendOutput(key, value) {
  appendKeyValue(requireEnv("GITHUB_OUTPUT"), key, value, "appendMultilineOutput");
}

export function appendMultilineOutput(key, value, delimiterPrefix = "EOF") {
  appendMultilineKeyValue(requireEnv("GITHUB_OUTPUT"), key, value, delimiterPrefix);
}

export function appendEnv(key, value) {
  appendKeyValue(requireEnv("GITHUB_ENV"), key, value, "appendMultilineEnv");
}

export function appendMultilineEnv(key, value, delimiterPrefix = "EOF") {
  appendMultilineKeyValue(requireEnv("GITHUB_ENV"), key, value, delimiterPrefix);
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

