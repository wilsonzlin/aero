import { readdir } from 'node:fs/promises';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const projectRoot = fileURLToPath(new URL('..', import.meta.url));
const tsStripLoader = new URL('../../../scripts/register-ts-strip-loader.mjs', import.meta.url).href;
const testSetup = new URL('./test-setup.mjs', import.meta.url).href;

const args = process.argv.slice(2);
const first = args[0] ?? null;
const firstIsPath = first !== null && first !== '' && !first.startsWith('-');

// CLI contract:
// - Optional first arg: a test root path (relative to package root).
// - Remaining args: forwarded to `node --test` (e.g. `--test-name-pattern`, `--test-only`).
//
// This lets callers do:
// - `npm test` (default test root)
// - `npm run test:property` (custom test root)
// - `npm test -- --test-name-pattern=...` (forwarded to node --test)
// - `npm test -- test/property --test-name-pattern=...` (custom root + forwarded args)
const testRoot = firstIsPath
  ? path.resolve(projectRoot, first)
  : path.join(projectRoot, 'test');
const nodeTestArgs = firstIsPath ? args.slice(1) : args;

const testFilePattern = /\.test\.(c|m)?(j|t)s$/;

async function collectTestFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];

  for (const entry of entries) {
    if (entry.name.startsWith('.')) continue;
    const fullPath = path.join(dir, entry.name);

    if (entry.isDirectory()) {
      files.push(...(await collectTestFiles(fullPath)));
      continue;
    }

    if (!entry.isFile()) continue;
    if (!testFilePattern.test(entry.name)) continue;

    files.push(fullPath);
  }

  return files;
}

let testFiles = [];
try {
  testFiles = await collectTestFiles(testRoot);
} catch (err) {
  if (err && typeof err === 'object' && 'code' in err && err.code === 'ENOENT') {
    testFiles = [];
  } else {
    throw err;
  }
}

testFiles.sort();

if (testFiles.length === 0) {
  process.stderr.write(`No test files found under ${path.relative(projectRoot, testRoot)}\n`);
  process.exitCode = 1;
} else {
  const stdioMode = process.env.AERO_TEST_STDIO ?? 'inherit';
  const stdio = stdioMode === 'ignore' ? ['ignore', 'ignore', 'ignore'] : 'inherit';
  const child = spawn(
    process.execPath,
    ['--experimental-strip-types', '--import', tsStripLoader, '--import', testSetup, '--test', ...nodeTestArgs, ...testFiles],
    {
      cwd: projectRoot,
      stdio,
    },
  );

  child.on('exit', (code, signal) => {
    if (signal) process.exit(1);
    process.exit(code ?? 1);
  });
}
