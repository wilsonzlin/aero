import { readdir } from 'node:fs/promises';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repoRoot = path.resolve(projectRoot, '..', '..');
const tsStripLoader = pathToFileURL(path.join(repoRoot, 'scripts', 'register-ts-strip-loader.mjs')).href;
const testSetup = pathToFileURL(path.join(projectRoot, 'scripts', 'test-setup.mjs')).href;
const testRoot = process.argv[2]
  ? path.resolve(projectRoot, process.argv[2])
  : path.join(projectRoot, 'test');

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
  const child = spawn(
    process.execPath,
    ['--experimental-strip-types', '--import', tsStripLoader, '--import', testSetup, '--test', ...testFiles],
    {
      cwd: projectRoot,
      stdio: 'inherit',
    },
  );

  child.on('exit', (code, signal) => {
    if (signal) process.exit(1);
    process.exit(code ?? 1);
  });
}
