#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { canonicalSecurityHeaders } from '../security_headers.mjs';

function fallbackFormatOneLineError(err, maxLen = 512) {
  let msg = 'Error';
  try {
    if (typeof err === 'string') msg = err;
    else if (err && typeof err === 'object' && typeof err.message === 'string' && err.message) msg = err.message;
  } catch {
    // ignore hostile getters
  }
  msg = msg.replace(/\s+/gu, ' ').trim();
  if (!Number.isInteger(maxLen) || maxLen <= 0) return '';
  if (msg.length > maxLen) msg = msg.slice(0, maxLen);
  return msg || 'Error';
}

let formatOneLineError = fallbackFormatOneLineError;
try {
  const mod = await import(new URL('../../src/text.js', import.meta.url));
  if (typeof mod?.formatOneLineError === 'function') {
    formatOneLineError = mod.formatOneLineError;
  }
} catch {
  // ignore - fallback stays active
}

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
  if (typeof value !== 'string') return '';
  if (normalizeHeaderKey(key) === 'content-security-policy') return normalizeCsp(value);
  return value.trim();
}

function toLowerHeaderMap(headers) {
  const out = {};
  for (const [key, value] of Object.entries(headers)) {
    out[normalizeHeaderKey(key)] = normalizeHeaderValue(key, value);
  }
  return out;
}

function validateCanonicalHeaders() {
  const errors = [];
  const expectExact = (key, value) => {
    const actual = canonicalSecurityHeaders[key];
    if (actual !== value) errors.push(`canonical ${key} must be ${JSON.stringify(value)} (got ${JSON.stringify(actual)})`);
  };

  // Cross-origin isolation (SharedArrayBuffer / WASM threads)
  expectExact('Cross-Origin-Opener-Policy', 'same-origin');
  expectExact('Cross-Origin-Embedder-Policy', 'require-corp');
  expectExact('Cross-Origin-Resource-Policy', 'same-origin');
  expectExact('Origin-Agent-Cluster', '?1');

  // Baseline security headers.
  expectExact('X-Content-Type-Options', 'nosniff');
  const referrer = canonicalSecurityHeaders['Referrer-Policy'];
  if (!referrer) errors.push('canonical Referrer-Policy must be non-empty');

  const permissions = canonicalSecurityHeaders['Permissions-Policy'];
  if (!permissions) {
    errors.push('canonical Permissions-Policy must be non-empty');
  } else {
    if (!permissions.includes('camera=()')) errors.push("canonical Permissions-Policy must include 'camera=()'");
    if (!permissions.includes('geolocation=()')) errors.push("canonical Permissions-Policy must include 'geolocation=()'");
    if (!permissions.includes('microphone=')) errors.push("canonical Permissions-Policy must include a microphone policy");
  }

  // CSP must allow dynamic WASM compilation for JIT, and allow workers.
  const csp = canonicalSecurityHeaders['Content-Security-Policy'];
  if (!csp) {
    errors.push('canonical Content-Security-Policy must be non-empty');
  } else {
    const norm = normalizeCsp(csp);
    if (!norm.includes("script-src 'self' 'wasm-unsafe-eval'")) {
      errors.push("canonical CSP must include: script-src 'self' 'wasm-unsafe-eval'");
    }
    if (!norm.includes("worker-src 'self' blob:")) {
      errors.push("canonical CSP must include: worker-src 'self' blob:");
    }
  }

  return errors;
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

function parseFastifyHeaders(filePath) {
  const headers = {};
  const content = readText(filePath);
  const re = /reply\.header\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]*)['"]\s*\)/g;
  for (const match of content.matchAll(re)) {
    const [, key, value] = match;
    headers[key] = value;
  }
  return [{ matcher: 'fastify', headers }];
}

function parseNodeSetHeader(filePath) {
  const headers = {};
  const content = readText(filePath);
  const re =
    /(?:^|\W)(?:res|reply)\.setHeader\(\s*(['"])(.*?)\1\s*,\s*(['"])(.*?)\3\s*,?\s*\)/gs;
  for (const match of content.matchAll(re)) {
    const [, , key, , value] = match;
    headers[key] = value;
  }
  return [{ matcher: 'node-http', headers }];
}

function parseSimpleYamlValues(filePath) {
  const out = {};
  const stack = [];
  const lines = readText(filePath).split(/\r?\n/);
  for (const rawLine of lines) {
    if (!rawLine.trim()) continue;
    if (rawLine.trim().startsWith('#')) continue;
    // Ignore list items; we only care about the chart's map-based defaults.
    if (rawLine.trim().startsWith('-')) continue;

    const match = rawLine.match(/^(\s*)([A-Za-z0-9_.-]+)\s*:\s*(.*)$/);
    if (!match) continue;
    const [, indentPrefix, key, rawValue] = match;
    const indent = indentPrefix.length;

    while (stack.length > 0 && indent <= stack.at(-1).indent) {
      stack.pop();
    }
    stack.push({ indent, key });

    const value = rawValue.trim();
    if (!value) continue;
    // Ignore YAML block scalars (not used in the values files we validate here).
    if (value === '|' || value === '>') continue;

    let normalized = value;
    if (
      (normalized.startsWith('"') && normalized.endsWith('"')) ||
      (normalized.startsWith("'") && normalized.endsWith("'"))
    ) {
      normalized = normalized.slice(1, -1);
    }

    const path = stack.map((entry) => entry.key).join('.');
    out[path] = normalized;
  }
  return out;
}

function checkHelmValues(fileLabel, filePath) {
  const values = parseSimpleYamlValues(filePath);
  const errors = [];

  const required = [
    // Chart values map to the canonical header keys.
    { path: 'ingress.coopCoep.coop', headerKey: 'Cross-Origin-Opener-Policy' },
    { path: 'ingress.coopCoep.coep', headerKey: 'Cross-Origin-Embedder-Policy' },
    { path: 'ingress.coopCoep.corp', headerKey: 'Cross-Origin-Resource-Policy' },
    { path: 'ingress.coopCoep.originAgentCluster', headerKey: 'Origin-Agent-Cluster' },
    { path: 'ingress.securityHeaders.xContentTypeOptions', headerKey: 'X-Content-Type-Options' },
    { path: 'ingress.securityHeaders.referrerPolicy', headerKey: 'Referrer-Policy' },
    { path: 'ingress.securityHeaders.permissionsPolicy', headerKey: 'Permissions-Policy' },
    { path: 'ingress.securityHeaders.contentSecurityPolicy', headerKey: 'Content-Security-Policy' },
  ];

  for (const item of required) {
    const expected = canonicalSecurityHeaders[item.headerKey];
    const actual = values[item.path];
    if (actual === undefined) {
      errors.push(`${fileLabel}: missing ${item.path} (expected canonical ${item.headerKey}=${expected})`);
      continue;
    }
    if (normalizeHeaderValue(item.headerKey, actual) !== normalizeHeaderValue(item.headerKey, expected)) {
      errors.push(`${fileLabel}: ${item.path} does not match canonical ${item.headerKey}`);
      errors.push(`  - expected: ${normalizeHeaderValue(item.headerKey, expected)}`);
      errors.push(`  + actual:   ${normalizeHeaderValue(item.headerKey, actual)}`);
    }
  }

  // Ensure the chart defaults enable the ingress-level header strategy.
  for (const path of ['ingress.coopCoep.enabled', 'ingress.securityHeaders.enabled']) {
    const actual = values[path];
    if (actual !== 'true') {
      errors.push(`${fileLabel}: expected ${path}=true (got ${actual ?? '<missing>'})`);
    }
  }

  return errors;
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

function pickCanonicalHeaders(keys) {
  const out = {};
  for (const key of keys) {
    const value = canonicalSecurityHeaders[key];
    if (value === undefined) {
      throw new Error(`canonical header missing key: ${key}`);
    }
    out[key] = value;
  }
  return out;
}

function checkViteConfig(fileLabel, filePath) {
  const content = readText(filePath);
  const errors = [];

  // Detect copy/pasted header strings or drift by ensuring the Vite configs
  // are wired to the canonical header exports. This is intentionally a
  // dependency-free static check (no TS parser).
  if (!content.includes('security_headers.mjs')) {
    errors.push(`${fileLabel}: missing import from scripts/security_headers.mjs`);
  }

  for (const symbol of ['crossOriginIsolationHeaders', 'baselineSecurityHeaders', 'cspHeaders']) {
    if (!content.includes(symbol)) {
      errors.push(`${fileLabel}: expected to reference ${symbol} (from scripts/security_headers.mjs)`);
    }
  }

  // Ensure cross-origin isolation headers are actually *applied*, not just imported.
  // Otherwise the "import present" checks above can be satisfied while the Vite
  // server silently stops being crossOriginIsolated (breaking WASM threads).
  const withoutStaticImports = content.replace(/^\s*import(?!\s*\()[\s\S]*?;\s*/gm, '');
  if (!withoutStaticImports.includes('crossOriginIsolationHeaders')) {
    errors.push(`${fileLabel}: expected to apply crossOriginIsolationHeaders in Vite headers`);
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
  // Helm chart defaults (ingress header injection) should also match canonical headers.
  { type: 'helm-values', path: 'deploy/k8s/chart/aero-gateway/values.yaml' },
  // Backend (aero-gateway) middleware that injects headers when the gateway is used as an origin.
  // Keep this aligned so single-origin deployments stay crossOriginIsolated.
  {
    type: 'fastify',
    path: 'backend/aero-gateway/src/middleware/crossOriginIsolation.ts',
    expectedKeys: [
      'Cross-Origin-Opener-Policy',
      'Cross-Origin-Embedder-Policy',
      'Cross-Origin-Resource-Policy',
      'Origin-Agent-Cluster',
    ],
  },
  {
    type: 'fastify',
    path: 'backend/aero-gateway/src/middleware/securityHeaders.ts',
    expectedKeys: ['X-Content-Type-Options', 'Referrer-Policy', 'Permissions-Policy'],
  },
  // Legacy backend (`server/`) that can optionally serve the frontend.
  {
    type: 'node-http',
    path: 'server/src/http.js',
    expectedKeys: [
      'Cross-Origin-Opener-Policy',
      'Cross-Origin-Embedder-Policy',
      'Cross-Origin-Resource-Policy',
      'Origin-Agent-Cluster',
      'X-Content-Type-Options',
      'Referrer-Policy',
      'Permissions-Policy',
      'Content-Security-Policy',
    ],
  },
  { type: 'headers', path: 'web/public/_headers' },
  { type: 'headers', path: 'deploy/cloudflare-pages/_headers' },
  { type: 'netlify', path: 'netlify.toml' },
  { type: 'netlify', path: 'deploy/netlify.toml' },
  { type: 'vercel', path: 'deploy/vercel.json' },
  // The primary Vercel deployment config lives at repo root.
  { type: 'vercel', path: 'vercel.json' },
  { type: 'nginx', path: 'deploy/nginx/nginx.conf' },
  { type: 'caddy', path: 'deploy/caddy/Caddyfile' },
];

const allErrors = [];

allErrors.push(...validateCanonicalHeaders().map((msg) => `scripts/headers.json: ${msg}`));

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
      allErrors.push(`${target.path}: failed to read file: ${formatOneLineError(err, 512)}`);
    }
    continue;
  }
  if (target.type === 'helm-values') {
    try {
      allErrors.push(...checkHelmValues(target.path, filePath));
    } catch (err) {
      allErrors.push(`${target.path}: failed to validate Helm values: ${formatOneLineError(err, 512)}`);
    }
    continue;
  }
  if (target.type === 'fastify') {
    try {
      const expected = pickCanonicalHeaders(target.expectedKeys ?? []);
      const rules = parseFastifyHeaders(filePath);
      const diffs = diffHeaderMaps(expected, toLowerHeaderMap(rules[0].headers));
      if (diffs.length !== 0) {
        allErrors.push(`\n${target.path}`);
        allErrors.push(...diffs.map((line) => `  ${line}`));
      }
    } catch (err) {
      allErrors.push(`${target.path}: failed to validate Fastify headers: ${formatOneLineError(err, 512)}`);
    }
    continue;
  }
  if (target.type === 'node-http') {
    try {
      const expected = pickCanonicalHeaders(target.expectedKeys ?? []);
      const rules = parseNodeSetHeader(filePath);
      const diffs = diffHeaderMaps(expected, toLowerHeaderMap(rules[0].headers));
      if (diffs.length !== 0) {
        allErrors.push(`\n${target.path}`);
        allErrors.push(...diffs.map((line) => `  ${line}`));
      }
    } catch (err) {
      allErrors.push(`${target.path}: failed to validate Node header middleware: ${formatOneLineError(err, 512)}`);
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
    allErrors.push(`${target.path}: failed to parse: ${formatOneLineError(err, 512)}`);
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
