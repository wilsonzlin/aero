#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(fileURLToPath(new URL('.', import.meta.url)), '../..');

function extractComposeEnvVars(text) {
  const vars = new Set();

  // ${VAR}, ${VAR:-default}, etc.
  for (const match of text.matchAll(/\$\{([A-Z0-9_]+)(?::-[^}]*)?\}/g)) {
    vars.add(match[1]);
  }

  // Compose "passthrough env vars" in list form:
  //   environment:
  //     - FOO
  //
  // We only consider upper snake case tokens to avoid deleting common YAML keys.
  for (const match of text.matchAll(/^\s*-\s*([A-Z0-9_]+)\s*$/gm)) {
    vars.add(match[1]);
  }

  // Compose "passthrough env vars" in mapping form:
  //   environment:
  //     FOO:
  //
  // docker compose resolves these from the host environment/.env when present, so
  // clear them to keep validation deterministic.
  for (const match of text.matchAll(/^\s*([A-Z0-9_]+)\s*:\s*$/gm)) {
    vars.add(match[1]);
  }

  return vars;
}

function readHeaderLines(filePath, maxLines = 25) {
  const text = readFileSync(filePath, 'utf8');
  return text.split(/\r?\n/).slice(0, maxLines);
}

function extractLabels(lines) {
  const labels = new Set();
  for (const line of lines) {
    const match = line.match(/^\s*#\s*(CANONICAL|EXAMPLE|LEGACY)\b/i);
    if (!match) continue;
    labels.add(match[1].toUpperCase());
  }
  return labels;
}

function requireLabel({ relPath, anyOf = [], mustContain = [] }) {
  const filePath = resolve(repoRoot, relPath);
  if (!existsSync(filePath)) return;

  const labels = extractLabels(readHeaderLines(filePath));
  const missing = [];

  if (anyOf.length > 0) {
    const ok = anyOf.some((token) => labels.has(token.toUpperCase()));
    if (!ok) {
      missing.push(`one of: ${anyOf.map((t) => `'${t}'`).join(', ')}`);
    }
  }

  for (const token of mustContain) {
    if (!labels.has(token.toUpperCase())) {
      missing.push(`'${token}'`);
    }
  }

  if (missing.length === 0) return;

  return {
    relPath,
    message: `Expected ${relPath} to be clearly labelled (${missing.join(' and ')}) in the first ~25 lines.`,
  };
}

function forbidLabel({ relPath, forbidden = [] }) {
  const filePath = resolve(repoRoot, relPath);
  if (!existsSync(filePath)) return;

  const labels = extractLabels(readHeaderLines(filePath));
  const hits = forbidden.filter((token) => labels.has(token.toUpperCase()));
  if (hits.length === 0) return;

  return {
    relPath,
    message: `Expected ${relPath} to NOT be labelled with ${hits.map((t) => `'${t}'`).join(', ')}.`,
  };
}

function ensureSingleLabel(relPath) {
  const filePath = resolve(repoRoot, relPath);
  if (!existsSync(filePath)) return;

  const labels = extractLabels(readHeaderLines(filePath));
  const known = ['CANONICAL', 'EXAMPLE', 'LEGACY'];
  const present = known.filter((label) => labels.has(label));
  if (present.length <= 1) return;

  return {
    relPath,
    message: `Expected ${relPath} to have exactly one label (CANONICAL/EXAMPLE/LEGACY); found: ${present.map((l) => `'${l}'`).join(', ')}`,
  };
}

function gitTrackedFiles() {
  const res = spawnSync('git', ['ls-files'], { cwd: repoRoot, encoding: 'utf8' });
  if (res.status !== 0) {
    throw new Error(`git ls-files failed: ${res.stderr || res.stdout}`);
  }
  return res.stdout
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

function dockerComposeAvailable() {
  try {
    const res = spawnSync('docker', ['compose', 'version'], { cwd: repoRoot, encoding: 'utf8' });
    return res.status === 0;
  } catch {
    return false;
  }
}

function validateComposeConfig(relPath) {
  const projectDir = dirname(relPath);
  const filePath = resolve(repoRoot, relPath);
  const composeText = readFileSync(filePath, 'utf8');
  const referencedEnvVars = extractComposeEnvVars(composeText);

  // docker compose implicitly loads `<project_dir>/.env` and also interpolates
  // values from the process environment. That makes local runs flaky (developer
  // shells often have AERO_* vars set). Keep the validation deterministic by:
  //  - forcing an empty env-file
  //  - clearing any env vars referenced by the compose manifest
  const env = { ...process.env };
  for (const key of referencedEnvVars) {
    delete env[key];
  }

  const args = ['compose', '--env-file', '/dev/null', '-f', relPath];
  if (projectDir && projectDir !== '.') {
    args.push('--project-directory', projectDir);
  }
  args.push('config', '-q');
  const cmd = `docker ${args.join(' ')}`;

  const res = spawnSync('docker', args, { cwd: repoRoot, env, encoding: 'utf8' });
  const output = [res.stdout, res.stderr].filter(Boolean).join('\n').trim();
  if (res.status === 0) {
    // docker compose can exit 0 while still printing warnings about unset
    // variables, deprecated fields, etc. Treat any output as a CI failure so
    // "example" manifests remain copy/paste-safe and warning-free.
    if (!output) return;
    return {
      relPath,
      message: `docker compose config produced warnings for ${relPath}:\n$ ${cmd}\n${output}`,
    };
  }

  return {
    relPath,
    message: `docker compose config failed for ${relPath}${output ? `:\n$ ${cmd}\n${output}` : ''}`,
  };
}

const errors = [];

// Canonical deployment entry points should say so explicitly.
errors.push(
  requireLabel({
    relPath: 'deploy/docker-compose.yml',
    mustContain: ['CANONICAL'],
  }),
);
errors.push(
  forbidLabel({
    relPath: 'deploy/docker-compose.yml',
    forbidden: ['EXAMPLE', 'LEGACY'],
  }),
);
errors.push(ensureSingleLabel('deploy/docker-compose.yml'));

// Any other Compose manifests are treated as reference-only examples, since the
// canonical production entry point is `deploy/docker-compose.yml`.
const tracked = gitTrackedFiles();
const composeManifests = Array.from(
  new Set([
    ...tracked.filter((path) => {
      const base = path.split('/').at(-1) ?? path;
      return (base.startsWith('docker-compose') && (base.endsWith('.yml') || base.endsWith('.yaml'))) || false;
    }),
    ...tracked.filter((path) => {
      const base = path.split('/').at(-1) ?? path;
      return base === 'compose.yaml' || base === 'compose.yml';
    }),
  ]),
).sort();

for (const relPath of composeManifests) {
  if (relPath === 'deploy/docker-compose.yml') continue;
  errors.push(
    requireLabel({
      relPath,
      anyOf: ['EXAMPLE', 'LEGACY'],
    }),
  );
  errors.push(
    forbidLabel({
      relPath,
      forbidden: ['CANONICAL'],
    }),
  );
  errors.push(ensureSingleLabel(relPath));
}

// Parse-level validation for all compose manifests. This catches accidental YAML
// syntax errors and unset variable interpolation issues early.
if (!dockerComposeAvailable()) {
  if (process.env.CI === 'true' || process.env.GITHUB_ACTIONS === 'true') {
    errors.push({
      relPath: '<docker>',
      message: 'docker compose is required for CI deploy manifest validation but was not found.',
    });
  } else {
    console.warn('warning: docker compose not found; skipping docker compose config validation.');
  }
} else {
  for (const relPath of composeManifests) {
    errors.push(validateComposeConfig(relPath));
  }
}

const filtered = errors.filter(Boolean);
if (filtered.length !== 0) {
  console.error('Deployment manifest hygiene check failed.\n');
  for (const err of filtered) {
    console.error(`- ${err.message}`);
  }
  console.error('\nFix: add a top-of-file comment like:');
  console.error('  # CANONICAL: <what this manifest is for>');
  console.error('  # EXAMPLE: <what this manifest is for>');
  process.exit(1);
}

console.log('Deployment manifest hygiene check passed.');
