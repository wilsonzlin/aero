import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(SCRIPT_DIR, '../..');

// Regenerate all scancode outputs first.
await import('./gen_scancodes.mjs');

const CANDIDATE_PATHS = [
  'src/input/scancodes.ts',
  'web/src/input/scancodes.ts',
  'crates/aero-devices-input/src/scancodes_generated.rs',
];

const tracked = execFileSync('git', ['ls-files', '--', ...CANDIDATE_PATHS], {
  cwd: REPO_ROOT,
  encoding: 'utf8',
})
  .split('\n')
  .map((s) => s.trim())
  .filter(Boolean);

if (tracked.length === 0) {
  throw new Error(`No tracked scancode outputs found (candidates: ${CANDIDATE_PATHS.join(', ')})`);
}

try {
  execFileSync('git', ['diff', '--exit-code', '--', ...tracked], { cwd: REPO_ROOT, stdio: 'inherit' });
} catch {
  console.error(
    `\nScancode generated outputs are out of date.\n` +
      `Regenerate and commit the result:\n` +
      `  node tools/gen_scancodes/gen_scancodes.mjs\n`,
  );
  process.exit(1);
}

