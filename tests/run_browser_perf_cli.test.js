import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const runBrowserPerfPath = fileURLToPath(new URL("../scripts/ci/run_browser_perf.mjs", import.meta.url));

function writeFile(filePath, contents) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, contents, "utf8");
}

function run(args, { cwd = process.cwd(), env = {} } = {}) {
  return spawnSync(process.execPath, [runBrowserPerfPath, ...args], {
    cwd,
    env: { ...process.env, ...env },
    stdio: ["ignore", "pipe", "pipe"],
    encoding: "utf8",
  });
}

function createFakePerfRunner(runnerPath) {
  writeFile(
    runnerPath,
    `
import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

function readFlag(name) {
  const idx = process.argv.indexOf(name);
  return idx === -1 ? null : process.argv[idx + 1] ?? null;
}

const outDir = readFlag("--out-dir");
if (!outDir) {
  console.error("fake perf runner: missing --out-dir");
  process.exit(2);
}
if (!path.isAbsolute(outDir)) {
  console.error(\`fake perf runner: expected absolute --out-dir, got \${outDir}\`);
  process.exit(3);
}

await fs.mkdir(outDir, { recursive: true });

// Record argv for the test harness.
await fs.writeFile(path.join(outDir, "argv.json"), JSON.stringify(process.argv.slice(2), null, 2));

// Minimal outputs expected by scripts/ci/run_browser_perf.mjs.
await fs.writeFile(path.join(outDir, "raw.json"), JSON.stringify({ ok: true }, null, 2));
await fs.writeFile(path.join(outDir, "summary.json"), JSON.stringify({ meta: { gitSha: "test" }, benchmarks: [] }, null, 2));
    `.trimStart(),
  );
}

test("run_browser_perf.mjs: runs a custom perf runner and normalizes output layout", () => {
  const outDirAbs = fs.mkdtempSync(path.join(os.tmpdir(), "aero-run-browser-perf-"));
  try {
    const runnerPath = path.join(outDirAbs, "fake-perf-runner.mjs");
    createFakePerfRunner(runnerPath);

    const outDirRel = path.relative(process.cwd(), outDirAbs);
    const res = run([
      "--url",
      "http://example.invalid/",
      "--out-dir",
      outDirRel,
      "--iterations",
      "1",
      "--perf-runner",
      runnerPath,
    ]);

    assert.equal(res.status, 0, `expected success, got status=${res.status} stderr=${res.stderr}`);

    // Required outputs.
    assert.ok(fs.existsSync(path.join(outDirAbs, "raw.json")));
    assert.ok(fs.existsSync(path.join(outDirAbs, "summary.json")));

    // The wrapper should create stable placeholder files when the runner doesn't produce them.
    assert.equal(fs.readFileSync(path.join(outDirAbs, "perf_export.json"), "utf8"), "null\n");
    assert.equal(fs.readFileSync(path.join(outDirAbs, "trace.json"), "utf8"), "null\n");
  } finally {
    fs.rmSync(outDirAbs, { recursive: true, force: true });
  }
});

test("run_browser_perf.mjs: forwards trace + microbench flags to tools/perf/run.mjs", () => {
  const outDirAbs = fs.mkdtempSync(path.join(os.tmpdir(), "aero-run-browser-perf-flags-"));
  try {
    const runnerPath = path.join(outDirAbs, "fake-perf-runner.mjs");
    createFakePerfRunner(runnerPath);

    const outDirRel = path.relative(process.cwd(), outDirAbs);
    const res = run([
      "--url",
      "http://example.invalid/",
      "--out-dir",
      outDirRel,
      "--iterations",
      "1",
      "--perf-runner",
      runnerPath,
      "--trace-duration-ms",
      "123",
      "--include-aero-bench",
    ]);

    assert.equal(res.status, 0, `expected success, got status=${res.status} stderr=${res.stderr}`);

    const argv = JSON.parse(fs.readFileSync(path.join(outDirAbs, "argv.json"), "utf8"));
    assert.ok(argv.includes("--trace"), "expected --trace to be forwarded");
    const durationIdx = argv.indexOf("--trace-duration-ms");
    assert.notEqual(durationIdx, -1, "expected --trace-duration-ms to be forwarded");
    assert.equal(argv[durationIdx + 1], "123");
    assert.ok(argv.includes("--include-aero-bench"), "expected --include-aero-bench to be forwarded");
  } finally {
    fs.rmSync(outDirAbs, { recursive: true, force: true });
  }
});

test("run_browser_perf.mjs: rejects invalid option combinations early", () => {
  const res = run(["--url", "http://example.invalid/", "--preview", "--out-dir", "out"]);
  assert.notEqual(res.status, 0);
  assert.match(res.stderr, /Exactly one of --url or --preview is required/u);
});

test("run_browser_perf.mjs: requires --out-dir", () => {
  const res = run(["--url", "http://example.invalid/"]);
  assert.notEqual(res.status, 0);
  assert.match(res.stderr, /--out-dir is required/u);
});

test("run_browser_perf.mjs: requires values for flags like --url", () => {
  const res = run(["--url", "--out-dir", "out"]);
  assert.notEqual(res.status, 0);
  assert.match(res.stderr, /--url requires a value/u);
});

test("run_browser_perf.mjs: rejects unexpected positional args", () => {
  const res = run(["--url", "http://example.invalid/", "--out-dir", "out", "extra"]);
  assert.notEqual(res.status, 0);
  assert.match(res.stderr, /Unexpected argument: extra/u);
});
