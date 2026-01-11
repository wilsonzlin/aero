import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");
const validator = path.join(repoRoot, "tools/perf/validate_perf_export.mjs");
const schema = path.join(repoRoot, "bench/schema/perf-output.schema.json");

function runValidate(args) {
  const result = spawnSync(process.execPath, [validator, "--schema", schema, ...args], {
    cwd: repoRoot,
    encoding: "utf8",
  });

  return {
    code: result.status ?? -1,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
  };
}

test("validate_perf_export accepts a minimal v2 export", () => {
  const input = path.join(__dirname, "fixtures/perf_export_valid_v2.json");
  const res = runValidate(["--input", input]);
  assert.equal(res.code, 0, `expected exit code 0, got ${res.code}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
});

test("validate_perf_export rejects schema-incompatible exports with a useful path", () => {
  const input = path.join(__dirname, "fixtures/perf_export_invalid.json");
  const res = runValidate(["--input", input]);
  assert.notEqual(res.code, 0, "expected non-zero exit code");
  assert.match(res.stderr, /\/build\/git_sha/, `expected stderr to mention /build/git_sha, got:\n${res.stderr}`);
});

test("validate_perf_export rejects null exports unless --allow-null is set", () => {
  const input = path.join(__dirname, "fixtures/perf_export_null.json");

  const res = runValidate(["--input", input]);
  assert.notEqual(res.code, 0, "expected non-zero exit code for null export");
  assert.match(res.stderr, /--allow-null/, `expected stderr to suggest --allow-null, got:\n${res.stderr}`);

  const allowed = runValidate(["--input", input, "--allow-null"]);
  assert.equal(
    allowed.code,
    0,
    `expected exit code 0 for --allow-null, got ${allowed.code}\nstdout:\n${allowed.stdout}\nstderr:\n${allowed.stderr}`,
  );
});

