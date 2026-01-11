#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(fileURLToPath(new URL('.', import.meta.url)), '../..');

function readHeaderLines(filePath, maxLines = 25) {
  const text = readFileSync(filePath, 'utf8');
  return text.split(/\r?\n/).slice(0, maxLines);
}

function findLabel(lines) {
  const commentLines = lines.filter((line) => line.trim().startsWith('#'));
  return commentLines.join('\n').toUpperCase();
}

function requireLabel({ relPath, anyOf = [], mustContain = [] }) {
  const filePath = resolve(repoRoot, relPath);
  if (!existsSync(filePath)) return;

  const header = findLabel(readHeaderLines(filePath));
  const missing = [];

  if (anyOf.length > 0) {
    const ok = anyOf.some((token) => header.includes(token.toUpperCase()));
    if (!ok) {
      missing.push(`one of: ${anyOf.map((t) => `'${t}'`).join(', ')}`);
    }
  }

  for (const token of mustContain) {
    if (!header.includes(token.toUpperCase())) {
      missing.push(`'${token}'`);
    }
  }

  if (missing.length === 0) return;

  return {
    relPath,
    message: `Expected ${relPath} to be clearly labelled (${missing.join(' and ')}) in the first ~25 lines.`,
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

// Root-level compose files exist for service-specific development and should be
// clearly marked to avoid conflicting with production manifests.
for (const relPath of ['docker-compose.yml', 'compose.yaml']) {
  errors.push(
    requireLabel({
      relPath,
      anyOf: ['EXAMPLE', 'LEGACY'],
    }),
  );
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

