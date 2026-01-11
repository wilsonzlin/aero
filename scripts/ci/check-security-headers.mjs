#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { canonicalSecurityHeaders } from '../security_headers.mjs';

const repoRoot = resolve(fileURLToPath(new URL('.', import.meta.url)), '../..');

function normalizeHeaderKey(key) {
  return key.toLowerCase();
}

function normalizeCsp(value) {
  // Allow templates to optionally splice in Caddy env vars without failing the
  // canonicalization checks.
  const stripped = value.replace(/\{\$AERO_CSP_CONNECT_SRC_EXTRA\}/g, '').replace(/\s+/g, ' ').trim();
  return stripped
    .split(';')
    .map((part) => part.trim().replace(/\s+/g, ' '))
    .filter(Boolean)
    .join('; ');
}

function normalizeHeaderValue(key, value) {
  if (normalizeHeaderKey(key) === 'content-security-policy') return normalizeCsp(value);
  return String(value).trim();
}

function toLowerHeaderMap(headers) {
  const out = {};
  for (const [key, value] of Object.entries(headers)) {
    out[normalizeHeaderKey(key)] = normalizeHeaderValue(key, value);
  }
  return out;
}

function diffHeaderMaps(expected, actual) {
  const diffs = [];
  for (const [rawKey, rawExpectedValue] of Object.entries(expected)) {
    const key = normalizeHeaderKey(rawKey);
    const expectedValue = normalizeHeaderValue(rawKey, rawExpectedValue);
    const actualValue = actual[key];
    if (actualValue === undefined) {
      diffs.push(`- ${rawKey}: ${expectedValue}`);
      diffs.push(`+ ${rawKey}: <missing>`);
      continue;
    }
    if (normalizeHeaderValue(rawKey, actualValue) !== expectedValue) {
      diffs.push(`- ${rawKey}: ${expectedValue}`);
      diffs.push(`+ ${rawKey}: ${normalizeHeaderValue(rawKey, actualValue)}`);
    }
  }
  return diffs;
}

function readText(filePath) {
  return readFileSync(filePath, 'utf8');
}

function parseHeadersFile(filePath) {
  const rules = [];
  const lines = readText(filePath).split(/\r?\n/);
  let current = null;
  for (const line of lines) {
    if (!line.trim()) continue;

    const isRuleLine = line.trim() === line;
    if (isRuleLine) {
      current = { matcher: line.trim(), headers: {} };
      rules.push(current);
      continue;
    }

    if (!current) continue;
    const trimmed = line.trim();
    const idx = trimmed.indexOf(':');
    if (idx === -1) continue;
    const key = trimmed.slice(0, idx).trim();
    const value = trimmed.slice(idx + 1).trim();
    current.headers[key] = value;
  }
  return rules;
}

function parseVercelJson(filePath) {
  const json = JSON.parse(readText(filePath));
  const rules = [];
  for (const entry of json.headers ?? []) {
    const headers = {};
    for (const { key, value } of entry.headers ?? []) {
      headers[key] = value;
    }
    rules.push({ matcher: entry.source ?? '<unknown>', headers });
  }
  return rules;
}

function parseCaddyfile(filePath) {
  const headers = {};
  const lines = readText(filePath).split(/\r?\n/);
  for (const line of lines) {
    const match = line.match(/^\s*([A-Za-z0-9-]+)\s+"([^"]*)"\s*$/);
    if (!match) continue;
    const [, key, value] = match;
    headers[key] = value;
  }
  return [{ matcher: 'caddyfile', headers }];
}

function parseNetlifyToml(filePath) {
  const headers = {};
  const lines = readText(filePath).split(/\r?\n/);
  let inValues = false;
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;

    if (/^\[headers\.values\]\s*$/.test(trimmed)) {
      inValues = true;
      continue;
    }
    if (inValues && trimmed.startsWith('[')) {
      // Next TOML section.
      break;
    }
    if (!inValues) continue;

    const match = trimmed.match(/^([A-Za-z0-9-]+)\s*=\s*"([^"]*)"\s*$/);
    if (!match) continue;
    const [, key, value] = match;
    headers[key] = value;
  }
  return [{ matcher: 'netlify', headers }];
}

function parseNginxConf(filePath) {
  const headers = {};
  const lines = readText(filePath).split(/\r?\n/);
  for (const line of lines) {
    const match = line.match(/^\s*add_header\s+([A-Za-z0-9-]+)\s+"([^"]*)"(?:\s+always)?\s*;/);
    if (!match) continue;
    const [, key, value] = match;
    headers[key] = value;
  }
  return [{ matcher: 'nginx', headers }];
}

function checkRules(fileLabel, rules) {
  const expected = canonicalSecurityHeaders;
  const errors = [];
  for (const rule of rules) {
    const diffs = diffHeaderMaps(expected, toLowerHeaderMap(rule.headers));
    if (diffs.length === 0) continue;
    errors.push(`\n${fileLabel} (${rule.matcher})`);
    errors.push(...diffs.map((line) => `  ${line}`));
  }
  return errors;
}

function checkViteConfig(fileLabel, filePath) {
  const content = readText(filePath);
  const errors = [];

  if (!content.includes('security_headers.mjs')) {
    errors.push(`${fileLabel}: missing import from scripts/security_headers.mjs`);
  }

  for (const symbol of ['crossOriginIsolationHeaders', 'baselineSecurityHeaders', 'cspHeaders']) {
    if (!content.includes(symbol)) {
      errors.push(`${fileLabel}: expected to reference ${symbol} (from scripts/security_headers.mjs)`);
    }
  }

  if (!content.includes('...baselineSecurityHeaders')) {
    errors.push(`${fileLabel}: expected to spread baselineSecurityHeaders into Vite headers`);
  }
  if (!content.includes('...cspHeaders')) {
    errors.push(`${fileLabel}: expected to spread cspHeaders into preview headers`);
  }

  for (const key of Object.keys(canonicalSecurityHeaders)) {
    if (content.includes(key)) {
      errors.push(`${fileLabel}: should not hardcode ${key}; load from scripts/headers.json instead`);
    }
  }

  return errors;
}

const targets = [
  // Vite configs that must stay in sync with the canonical headers.
  { type: 'vite', path: 'vite.harness.config.ts' },
  { type: 'vite', path: 'web/vite.config.ts' },
  // Deployment templates that must stay in sync with the canonical headers.
  { type: 'headers', path: 'web/public/_headers' },
  { type: 'headers', path: 'deploy/cloudflare-pages/_headers' },
  { type: 'netlify', path: 'netlify.toml' },
  { type: 'vercel', path: 'deploy/vercel.json' },
  // The primary Vercel deployment config lives at repo root.
  { type: 'vercel', path: 'vercel.json' },
  { type: 'nginx', path: 'deploy/nginx/nginx.conf' },
  { type: 'caddy', path: 'deploy/caddy/Caddyfile' },
];

const allErrors = [];

for (const target of targets) {
  const filePath = resolve(repoRoot, target.path);
  if (!existsSync(filePath)) {
    allErrors.push(
      `${target.path}: missing template file (CI validates templates against scripts/headers.json via scripts/security_headers.mjs)`,
    );
    continue;
  }
  if (target.type === 'vite') {
    try {
      allErrors.push(...checkViteConfig(target.path, filePath));
    } catch (err) {
      allErrors.push(`${target.path}: failed to read file: ${err?.message ?? String(err)}`);
    }
    continue;
  }

  let rules;
  try {
    switch (target.type) {
      case 'headers':
        rules = parseHeadersFile(filePath);
        break;
      case 'netlify':
        rules = parseNetlifyToml(filePath);
        break;
      case 'vercel':
        rules = parseVercelJson(filePath);
        break;
      case 'nginx':
        rules = parseNginxConf(filePath);
        break;
      case 'caddy':
        rules = parseCaddyfile(filePath);
        break;
      default:
        allErrors.push(`${target.path}: unknown target type: ${target.type}`);
        continue;
    }
  } catch (err) {
    allErrors.push(`${target.path}: failed to parse: ${err?.message ?? String(err)}`);
    continue;
  }

  allErrors.push(...checkRules(target.path, rules));
}

if (allErrors.length !== 0) {
  console.error('Security header templates are out of sync with the canonical values.');
  console.error('Canonical values live in scripts/headers.json (loaded by scripts/security_headers.mjs).');
  console.error('Fix the templates (or update scripts/headers.json), then re-run: node scripts/ci/check-security-headers.mjs\n');
  console.error(allErrors.join('\n'));
  process.exit(1);
}

console.log('Security header templates are consistent.');
