import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function runNodeModule(code, env) {
  const res = spawnSync(process.execPath, ["--input-type=module", "-e", code], {
    cwd: repoRoot,
    env: { ...process.env, ...env },
    encoding: "utf8",
  });
  return res;
}

test("github_io: appendOutput rejects newline values with a helpful hint", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-gh-io-"));
  const outFile = path.join(tmp, "out.txt");
  await fs.writeFile(outFile, "", "utf8");

  const res = runNodeModule(
    `
      import { appendOutput } from "./.github/actions/_shared/github_io.mjs";
      process.env.GITHUB_OUTPUT = ${JSON.stringify(outFile)};
      appendOutput("k", "a\\nb");
    `,
    { GITHUB_OUTPUT: outFile },
  );

  assert.notEqual(res.status, 0);
  assert.match(res.stderr, /contains a newline/i);
  assert.match(res.stderr, /appendMultilineOutput/i);
});

test("github_io: appendEnv rejects newline values with a helpful hint", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-gh-io-"));
  const envFile = path.join(tmp, "env.txt");
  await fs.writeFile(envFile, "", "utf8");

  const res = runNodeModule(
    `
      import { appendEnv } from "./.github/actions/_shared/github_io.mjs";
      process.env.GITHUB_ENV = ${JSON.stringify(envFile)};
      appendEnv("k", "a\\nb");
    `,
    { GITHUB_ENV: envFile },
  );

  assert.notEqual(res.status, 0);
  assert.match(res.stderr, /contains a newline/i);
  assert.match(res.stderr, /appendMultilineEnv/i);
});

test("github_io: appendMultilineEnv writes a well-formed delimiter block", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-gh-io-"));
  const envFile = path.join(tmp, "env.txt");
  await fs.writeFile(envFile, "", "utf8");

  const res = runNodeModule(
    `
      import { appendMultilineEnv } from "./.github/actions/_shared/github_io.mjs";
      process.env.GITHUB_ENV = ${JSON.stringify(envFile)};
      appendMultilineEnv("k", "a\\nb");
    `,
    { GITHUB_ENV: envFile },
  );

  assert.equal(res.status, 0, res.stderr || res.stdout || "");
  const raw = await fs.readFile(envFile, "utf8");
  const m = raw.match(/^k<<(.+)$/m);
  assert.ok(m, "expected multiline env entry");
  const delimiter = m[1];
  assert.ok(raw.includes(`\na\nb\n${delimiter}\n`));
});

