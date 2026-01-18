import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function rustcHostTarget() {
  try {
    const vv = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
    const m = vv.match(/^host:\s*(.+)\s*$/m);
    return m ? m[1].trim() : null;
  } catch {
    return null;
  }
}

function cargoTargetRustflagsVar(targetTriple) {
  return `CARGO_TARGET_${targetTriple.toUpperCase().replace(/[-.]/g, "_")}_RUSTFLAGS`;
}

function gitFileMode(relPath) {
  const out = execFileSync("git", ["ls-files", "--stage", "--", relPath], {
    cwd: repoRoot,
    encoding: "utf8",
  }).trim();
  if (!out) return null;
  // Format: "<mode> <blob> <stage>\t<path>"
  return out.split(/\s+/, 1)[0];
}

function trackedShellScripts() {
  // Enumerate via git so the test doesn't accidentally recurse into large
  // untracked dirs (node_modules/, target/, etc.) in local checkouts.
  return execFileSync("git", ["ls-files"], {
    cwd: repoRoot,
    encoding: "utf8",
  })
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && line.endsWith(".sh"));
}

function scriptsReferencedByDocs() {
  // Only scan tracked Markdown files to avoid walking huge untracked dirs like
  // node_modules/ or target/ (which may exist in CI/local checkouts).
  const markdownFiles = execFileSync("git", ["ls-files"], {
    cwd: repoRoot,
    encoding: "utf8",
  })
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && line.endsWith(".md"));

  // Match docs that invoke a shell script directly, e.g.:
  //   ./scripts/safe-run.sh cargo test --locked
  //   (cd infra/local-object-store && ./verify.sh)
  // Keep the character class conservative; `.sh` is an unambiguous terminator.
  //
  // Note: avoid matching inside `../foo.sh` or `../../foo.sh` by requiring the
  // `./` not be preceded by another dot.
  const re = /(^|[^.])(\\.\/[0-9A-Za-z_./-]+\.sh)/gm;

  const scripts = new Set();
  const missing = [];
  for (const relDocPath of markdownFiles) {
    const absDocPath = path.join(repoRoot, relDocPath);
    const content = fs.readFileSync(absDocPath, "utf8");
    for (const m of content.matchAll(re)) {
      const match = m[2];
      const relPathNoDot = match.replace(/^\.\//, "");
      // First try repo-root relative (common case in root-level commands).
      const absRepoRoot = path.join(repoRoot, relPathNoDot);
      if (fs.existsSync(absRepoRoot)) {
        scripts.add(path.relative(repoRoot, absRepoRoot));
        continue;
      }

      // Then try doc-relative (common in README.md files that assume `cd` into
      // the directory first, like `infra/local-object-store/README.md`).
      const absDocRelative = path.join(path.dirname(absDocPath), relPathNoDot);
      if (fs.existsSync(absDocRelative)) {
        scripts.add(path.relative(repoRoot, absDocRelative));
        continue;
      }

      missing.push(`${relDocPath}: ${match}`);
    }
  }
  assert.deepEqual(
    missing,
    [],
    `some docs reference shell scripts that do not exist (checked repo-root and doc-relative paths)`,
  );
  return scripts;
}

function scriptsToCheck() {
  const scripts = new Set();

  // Ensure every tracked shell script stays executable (CI/workflows rely on this).
  for (const relPath of trackedShellScripts()) scripts.add(relPath);

  // Ensure any docs invoking `./...*.sh` refer to an existing executable file.
  for (const docScript of scriptsReferencedByDocs()) scripts.add(docScript);

  return [...scripts].sort();
}

test("scripts referenced as ./scripts/*.sh are executable", { skip: process.platform === "win32" }, () => {
  for (const relPath of scriptsToCheck()) {
    const absPath = path.join(repoRoot, relPath);
    assert.ok(fs.existsSync(absPath), `${relPath} is missing`);

    const { mode: fsMode } = fs.statSync(absPath);
    // Any executable bit (user/group/other) is good enough.
    assert.ok((fsMode & 0o111) !== 0, `${relPath} is not executable (expected chmod +x / git mode 100755)`);

    const modeInGit = gitFileMode(relPath);
    if (modeInGit !== null) {
      assert.equal(modeInGit, "100755", `${relPath} is not executable in git (expected mode 100755)`);
    }

    // Scripts invoked directly (via `./foo.sh`) must have a shebang, otherwise
    // Linux will fail with `Exec format error`.
    const firstLine = fs.readFileSync(absPath, "utf8").split(/\r?\n/, 1)[0];
    assert.ok(firstLine.startsWith("#!"), `${relPath} is missing a shebang (expected first line to start with '#!')`);

    // Also ensure the shebang line uses LF, not CRLF. The Linux kernel does not
    // strip '\r' from the interpreter path, so `#!/usr/bin/env bash\r\n` fails
    // with "bad interpreter".
    const raw = fs.readFileSync(absPath);
    const nl = raw.indexOf(0x0a);
    if (nl !== -1) {
      assert.notEqual(
        raw[nl - 1],
        0x0d,
        `${relPath} has CRLF line ending on the shebang (use LF to avoid "bad interpreter")`,
      );
    }
  }
});

test("safe-run.sh can execute a trivial command (Linux)", { skip: process.platform !== "linux" }, () => {
  execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["true"], {
    cwd: repoRoot,
    stdio: "ignore",
  });
});

test("safe-run.sh defaults Cargo parallelism to -j1 (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  delete env.AERO_CARGO_BUILD_JOBS;
  delete env.RAYON_NUM_THREADS;

  const stdout = execFileSync(
    path.join(repoRoot, "scripts/safe-run.sh"),
    ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$RAYON_NUM_THREADS"'],
    {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  assert.equal(stdout, "1|1");
});

test("safe-run.sh respects AERO_CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  delete env.RAYON_NUM_THREADS;
  env.AERO_CARGO_BUILD_JOBS = "2";

  const stdout = execFileSync(
    path.join(repoRoot, "scripts/safe-run.sh"),
    ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$RAYON_NUM_THREADS"'],
    {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  assert.equal(stdout, "2|2");
});

test("safe-run.sh defaults AERO_TOKIO_WORKER_THREADS to CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  delete env.AERO_CARGO_BUILD_JOBS;
  delete env.AERO_TOKIO_WORKER_THREADS;

  const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$AERO_TOKIO_WORKER_THREADS"'], {
    cwd: repoRoot,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.equal(stdout, "1|1");
});

test("safe-run.sh defaults AERO_TOKIO_WORKER_THREADS based on AERO_CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  delete env.AERO_TOKIO_WORKER_THREADS;
  env.AERO_CARGO_BUILD_JOBS = "2";

  const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$AERO_TOKIO_WORKER_THREADS"'], {
    cwd: repoRoot,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.equal(stdout, "2|2");
});

test("safe-run.sh preserves an explicit AERO_TOKIO_WORKER_THREADS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  env.AERO_CARGO_BUILD_JOBS = "2";
  env.AERO_TOKIO_WORKER_THREADS = "7";

  const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$AERO_TOKIO_WORKER_THREADS"'], {
    cwd: repoRoot,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });

  assert.equal(stdout, "2|7");
});

test("safe-run.sh sanitizes invalid AERO_TOKIO_WORKER_THREADS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  env.AERO_CARGO_BUILD_JOBS = "2";
  env.AERO_TOKIO_WORKER_THREADS = "nope";

  const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$AERO_TOKIO_WORKER_THREADS"'], {
    cwd: repoRoot,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.equal(stdout, "2|2");
});

test("safe-run.sh defaults NEXTEST_TEST_THREADS to CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  delete env.AERO_CARGO_BUILD_JOBS;
  delete env.NEXTEST_TEST_THREADS;

  const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$NEXTEST_TEST_THREADS"'], {
    cwd: repoRoot,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.equal(stdout, "1|1");
});

test("safe-run.sh sanitizes invalid NEXTEST_TEST_THREADS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  env.AERO_CARGO_BUILD_JOBS = "2";
  env.NEXTEST_TEST_THREADS = "nope";

  const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$NEXTEST_TEST_THREADS"'], {
    cwd: repoRoot,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.equal(stdout, "2|2");
});

test("safe-run.sh preserves NEXTEST_TEST_THREADS=num-cpus opt-out (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  env.AERO_CARGO_BUILD_JOBS = "2";
  env.NEXTEST_TEST_THREADS = "num-cpus";

  const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$NEXTEST_TEST_THREADS"'], {
    cwd: repoRoot,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.equal(stdout, "2|num-cpus");
});

test("safe-run.sh defaults RUST_TEST_THREADS for cargo test (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-test-threads-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUST_TEST_THREADS"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_BUILD_JOBS;
    delete env.AERO_CARGO_BUILD_JOBS;
    delete env.RUST_TEST_THREADS;
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo", "test"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.equal(stdout, "1");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh sets RUST_TEST_THREADS based on AERO_CARGO_BUILD_JOBS for cargo test (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-test-threads-jobs-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUST_TEST_THREADS"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_BUILD_JOBS;
    delete env.RUST_TEST_THREADS;
    env.AERO_CARGO_BUILD_JOBS = "2";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo", "test"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.equal(stdout, "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh preserves explicit RUST_TEST_THREADS for cargo test (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-test-threads-explicit-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUST_TEST_THREADS"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_BUILD_JOBS;
    delete env.AERO_CARGO_BUILD_JOBS;
    env.RUST_TEST_THREADS = "3";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo", "test"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.equal(stdout, "3");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh sanitizes invalid RUSTC_WORKER_THREADS and RAYON_NUM_THREADS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_BUILD_JOBS;
  delete env.AERO_CARGO_BUILD_JOBS;
  env.RUSTC_WORKER_THREADS = "nope";
  env.RAYON_NUM_THREADS = "nope";

  const stdout = execFileSync(
    path.join(repoRoot, "scripts/safe-run.sh"),
    ["bash", "-c", 'printf "%s|%s" "$RUSTC_WORKER_THREADS" "$RAYON_NUM_THREADS"'],
    {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  assert.equal(stdout, "1|1");
});

test("safe-run.sh: AERO_CARGO_BUILD_JOBS overrides CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  env.CARGO_BUILD_JOBS = "4";
  env.AERO_CARGO_BUILD_JOBS = "2";
  delete env.RAYON_NUM_THREADS;

  const stdout = execFileSync(
    path.join(repoRoot, "scripts/safe-run.sh"),
    ["bash", "-c", 'printf "%s|%s" "$CARGO_BUILD_JOBS" "$RAYON_NUM_THREADS"'],
    {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  assert.equal(stdout, "2|2");
});

test("safe-run.sh sanitizes invalid CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  env.CARGO_BUILD_JOBS = "nope";
  delete env.AERO_CARGO_BUILD_JOBS;
  delete env.RAYON_NUM_THREADS;

  const stdout = execFileSync(
    path.join(repoRoot, "scripts/safe-run.sh"),
    ["bash", "-c", 'printf "%s" "$CARGO_BUILD_JOBS"'],
    {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  assert.equal(stdout, "1");
});

test("safe-run.sh can isolate CARGO_HOME to avoid registry lock contention (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  delete env.CARGO_HOME;
  env.AERO_ISOLATE_CARGO_HOME = "1";

  const stdout = execFileSync(
    path.join(repoRoot, "scripts/safe-run.sh"),
    ["bash", "-c", 'printf "%s" "$CARGO_HOME"'],
    {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  assert.equal(stdout, path.join(repoRoot, ".cargo-home"));
});

test("safe-run.sh: AERO_ISOLATE_CARGO_HOME overrides an existing CARGO_HOME (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  env.CARGO_HOME = path.join(os.tmpdir(), "aero-safe-run-preexisting-cargo-home");
  env.AERO_ISOLATE_CARGO_HOME = "1";

  const stdout = execFileSync(
    path.join(repoRoot, "scripts/safe-run.sh"),
    ["bash", "-c", 'printf "%s" "$CARGO_HOME"'],
    {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  assert.equal(stdout, path.join(repoRoot, ".cargo-home"));
});

test("safe-run.sh: AERO_ISOLATE_CARGO_HOME accepts a custom path value (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-custom-cargo-home-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$CARGO_HOME"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_HOME;
    env.AERO_ISOLATE_CARGO_HOME = "my-cargo-home";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, path.join(repoRoot, "my-cargo-home"));
    assert.ok(fs.existsSync(stdout), `expected custom cargo home directory to exist: ${stdout}`);
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh: AERO_ISOLATE_CARGO_HOME expands ~ using HOME (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-tilde-cargo-home-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$CARGO_HOME"\n', { mode: 0o755 });

    const homeDir = path.join(tmpRoot, "home");
    fs.mkdirSync(homeDir, { recursive: true });

    const env = { ...process.env };
    delete env.CARGO_HOME;
    env.HOME = homeDir;
    env.AERO_ISOLATE_CARGO_HOME = "~/my-cargo-home";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, path.join(homeDir, "my-cargo-home"));
    assert.ok(fs.existsSync(stdout), `expected expanded cargo home directory to exist: ${stdout}`);
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh: AERO_ISOLATE_CARGO_HOME only expands ~ or ~/ (not ~user) (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-tilde-user-cargo-home-"));
  try {
    const repoDir = path.join(tmpRoot, "repo");
    const scriptsDir = path.join(repoDir, "scripts");
    fs.mkdirSync(scriptsDir, { recursive: true });

    for (const rel of ["safe-run.sh", "with-timeout.sh", "run_limited.sh"]) {
      const src = path.join(repoRoot, "scripts", rel);
      const dst = path.join(scriptsDir, rel);
      fs.copyFileSync(src, dst);
      fs.chmodSync(dst, 0o755);
    }

    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$CARGO_HOME"\n', { mode: 0o755 });

    const homeDir = path.join(tmpRoot, "home");
    fs.mkdirSync(homeDir, { recursive: true });

    const env = { ...process.env };
    delete env.CARGO_HOME;
    env.HOME = homeDir;
    // `~user` expansion is intentionally not supported; treat it as a literal path.
    env.AERO_ISOLATE_CARGO_HOME = "~otheruser/my-cargo-home";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(scriptsDir, "safe-run.sh"), ["cargo"], {
      cwd: repoDir,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, path.join(repoDir, "~otheruser/my-cargo-home"));
    assert.ok(fs.existsSync(stdout), `expected literal cargo home directory to exist: ${stdout}`);
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh auto-uses .cargo-home when present and CARGO_HOME is default (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-auto-cargo-home-"));
  try {
    const repoDir = path.join(tmpRoot, "repo");
    const scriptsDir = path.join(repoDir, "scripts");
    fs.mkdirSync(scriptsDir, { recursive: true });

    for (const rel of ["safe-run.sh", "with-timeout.sh", "run_limited.sh"]) {
      const src = path.join(repoRoot, "scripts", rel);
      const dst = path.join(scriptsDir, rel);
      fs.copyFileSync(src, dst);
      fs.chmodSync(dst, 0o755);
    }

    fs.mkdirSync(path.join(repoDir, ".cargo-home"), { recursive: true });

    const homeDir = path.join(tmpRoot, "home");
    fs.mkdirSync(homeDir, { recursive: true });

    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$CARGO_HOME"\n', { mode: 0o755 });

    const env = { ...process.env };
    delete env.AERO_ISOLATE_CARGO_HOME;
    env.HOME = homeDir;
    env.CARGO_HOME = path.join(homeDir, ".cargo");
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(scriptsDir, "safe-run.sh"), ["cargo"], {
      cwd: repoDir,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout.trim(), path.join(repoDir, ".cargo-home"));
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh clears sccache wrappers by default (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-sccache-wrapper-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      '#!/usr/bin/env bash\nprintf "%s|%s|%s|%s" "$RUSTC_WRAPPER" "$RUSTC_WORKSPACE_WRAPPER" "$CARGO_BUILD_RUSTC_WRAPPER" "$CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"\n',
    );
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    env.RUSTC_WRAPPER = "sccache";
    env.RUSTC_WORKSPACE_WRAPPER = "sccache";
    env.CARGO_BUILD_RUSTC_WRAPPER = "sccache";
    env.CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER = "sccache";
    delete env.AERO_DISABLE_RUSTC_WRAPPER;
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.equal(stdout, "|||");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh preserves non-sccache rustc wrappers by default (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-non-sccache-wrapper-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUSTC_WRAPPER"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    env.RUSTC_WRAPPER = "ccache";
    delete env.AERO_DISABLE_RUSTC_WRAPPER;
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.equal(stdout, "ccache");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh can force-disable wrappers via AERO_DISABLE_RUSTC_WRAPPER (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-disable-wrapper-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUSTC_WRAPPER"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    env.RUSTC_WRAPPER = "ccache";
    env.AERO_DISABLE_RUSTC_WRAPPER = "1";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.equal(stdout, "");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh sets NODE_OPTIONS without disallowed flags (Linux)", { skip: process.platform !== "linux" }, () => {
  const env = { ...process.env };
  // Simulate a broken outer environment that injects disallowed node flags via NODE_OPTIONS
  // (Node rejects --test-concurrency in NODE_OPTIONS).
  // Cover both `--test-concurrency=<n>` and `--test-concurrency <n>` spellings.
  env.NODE_OPTIONS = "--trace-warnings --test-concurrency=7 --test-concurrency 3";

  const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["node", "-e", 'process.stdout.write(process.env.NODE_OPTIONS || "")'], {
    cwd: repoRoot,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.match(stdout, /(^|\s)--trace-warnings(\s|$)/);
  assert.match(stdout, /--max-old-space-size=4096\b/);
  assert.ok(!stdout.includes("--test-concurrency"), `expected NODE_OPTIONS not to include --test-concurrency, got: ${stdout}`);
});

test("safe-run.sh silences check-node-version mismatch notes on Node major mismatch (Linux)", { skip: process.platform !== "linux" }, () => {
  const nvmrc = fs.readFileSync(path.join(repoRoot, ".nvmrc"), "utf8").trim();
  const expectedMajor = Number(nvmrc.replace(/^v/, "").split(".", 1)[0]);
  assert.ok(Number.isFinite(expectedMajor), "expected .nvmrc to contain a major.minor.patch Node version");

  const overrideMajor = expectedMajor + 3;
  const overrideVersion = `${overrideMajor}.0.0`;

  // Default: safe-run should set AERO_CHECK_NODE_QUIET=1 for a major mismatch, so `npm run check:node`
  // should emit no "note: Node.js ..." noise from `scripts/check-node-version.mjs`.
  const resQuiet = spawnSync("bash", ["scripts/safe-run.sh", "npm", "run", "check:node"], {
    cwd: repoRoot,
    env: {
      ...process.env,
      // Used both by `check-node-version.mjs` *and* by safe-run's major mismatch detection.
      AERO_NODE_VERSION_OVERRIDE: overrideVersion,
    },
    encoding: "utf8",
  });
  assert.equal(resQuiet.status, 0);
  assert.ok(!`${resQuiet.stdout ?? ""}${resQuiet.stderr ?? ""}`.includes("note: Node.js"), "expected safe-run to silence Node mismatch notes");

  // Opt-out: if the caller explicitly sets AERO_CHECK_NODE_QUIET (even to "0"), safe-run should not
  // override it, and the note should be visible again.
  const resNoisy = spawnSync("bash", ["scripts/safe-run.sh", "npm", "run", "check:node"], {
    cwd: repoRoot,
    env: {
      ...process.env,
      AERO_NODE_VERSION_OVERRIDE: overrideVersion,
      AERO_CHECK_NODE_QUIET: "0",
    },
    encoding: "utf8",
  });
  assert.equal(resNoisy.status, 0);
  assert.ok(`${resNoisy.stdout ?? ""}${resNoisy.stderr ?? ""}`.includes("note: Node.js"), "expected note output when opt-out is set");
});

test("safe-run.sh does not force rustc codegen-units by default (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-cargo-env-"));
  try {
    // `safe-run.sh` caps lld threads via per-target rustflags env vars
    // (`CARGO_TARGET_<TRIPLE>_RUSTFLAGS`) instead of mutating global `RUSTFLAGS`,
    // because global `RUSTFLAGS` breaks wasm32 builds (`rust-lld -flavor wasm`
    // does not understand `-Wl,...`).
    const hostTarget = rustcHostTarget();
    const hostTargetVar = hostTarget === null ? null : cargoTargetRustflagsVar(hostTarget);

    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s|||%s" "${RUSTFLAGS:-}" "${!AERO_TARGET_RUSTFLAGS_VAR:-}"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    // Keep the test deterministic: pick an explicit native target triple so we can assert on the
    // per-target rustflags variable that safe-run sets to cap linker parallelism.
    env.CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    delete env.CARGO_BUILD_JOBS;
    delete env.AERO_CARGO_BUILD_JOBS;
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.RUSTFLAGS;
    delete env.AERO_TARGET_RUSTFLAGS_VAR;
    if (hostTargetVar) {
      delete env[hostTargetVar];
      env.AERO_TARGET_RUSTFLAGS_VAR = hostTargetVar;
    }
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    const [rustflags, targetFlags] = stdout.split("|||");
    assert.ok(!rustflags.includes("codegen-units="), `expected RUSTFLAGS not to force codegen-units, got: ${rustflags}`);
    assert.ok(
      !rustflags.includes("--threads=") && !rustflags.includes("-Wl,--threads="),
      `expected safe-run.sh not to inject lld --threads into global RUSTFLAGS, got: ${rustflags}`,
    );

    // Still cap LLVM lld parallelism to match build jobs (default: 1), but do it via the host
    // target's per-target rustflags env var.
    if (hostTargetVar) {
      assert.match(targetFlags, /-C link-arg=-Wl,--threads=1\b/);
    }
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh does not force codegen-units based on AERO_CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-cargo-jobs-"));
  try {
    const hostTarget = rustcHostTarget();
    const hostTargetVar = hostTarget === null ? null : cargoTargetRustflagsVar(hostTarget);

    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s|||%s" "${RUSTFLAGS:-}" "${!AERO_TARGET_RUSTFLAGS_VAR:-}"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    // Keep the test deterministic: pick an explicit native target triple so we can assert on the
    // per-target rustflags variable that safe-run sets to cap linker parallelism.
    env.CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    delete env.CARGO_BUILD_JOBS;
    delete env.RUSTFLAGS;
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    env.AERO_CARGO_BUILD_JOBS = "2";
    delete env.AERO_TARGET_RUSTFLAGS_VAR;
    if (hostTargetVar) {
      delete env[hostTargetVar];
      env.AERO_TARGET_RUSTFLAGS_VAR = hostTargetVar;
    }
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    const [rustflags, targetFlags] = stdout.split("|||");
    assert.ok(!rustflags.includes("codegen-units="), `expected RUSTFLAGS not to force codegen-units, got: ${rustflags}`);
    assert.ok(
      !rustflags.includes("--threads=") && !rustflags.includes("-Wl,--threads="),
      `expected safe-run.sh not to inject lld --threads into global RUSTFLAGS, got: ${rustflags}`,
    );
    if (hostTargetVar) {
      assert.match(targetFlags, /-C link-arg=-Wl,--threads=2\b/);
    }
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh uses wasm lld threads flag when building wasm32 targets (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-lld-threads-wasm-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s|||%s" "${RUSTFLAGS:-}" "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-}"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_BUILD_JOBS;
    delete env.AERO_CARGO_BUILD_JOBS;
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.CARGO_BUILD_TARGET;
    delete env.RUSTFLAGS;
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    // Use an explicit `--target` flag to ensure safe-run does not inject the native-only
    // `-Wl,--threads=` form, which breaks `rust-lld -flavor wasm` link steps.
    const stdout = execFileSync(
      path.join(repoRoot, "scripts/safe-run.sh"),
      ["cargo", "--target", "wasm32-unknown-unknown"],
      {
        cwd: repoRoot,
        env,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    const [rustflags, wasmTargetFlags] = stdout.split("|||");
    assert.ok(
      !rustflags.includes("--threads=") && !rustflags.includes("-Wl,--threads="),
      `expected safe-run.sh not to inject lld --threads into global RUSTFLAGS, got: ${rustflags}`,
    );
    assert.match(wasmTargetFlags, /-C link-arg=--threads=1\b/);
    assert.ok(
      !wasmTargetFlags.includes("-Wl,--threads="),
      `expected wasm build not to use -Wl,--threads=; got: ${wasmTargetFlags}`,
    );
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries Cargo when it hits fork EAGAIN under contention (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-fork-eagain-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    // Track how many times safe-run invoked "cargo".
    const invocationsFile = path.join(tmpRoot, "cargo-invocations.txt");

    // Fake `cargo` that fails once with the fork EAGAIN signature safe-run should retry, then succeeds.
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${CARGO_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  echo "run_limited.sh: fork: retry: Resource temporarily unavailable" >&2
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeCargo, 0o755);

    // Override `sleep` so the safe-run exponential backoff doesn't slow down tests.
    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.CARGO_INVOCATIONS_FILE = invocationsFile;

    // Keep the retry loop bounded (one retry max).
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";

    // Ensure the codegen-units override path isn't accidentally triggered by the outer environment.
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.RUSTFLAGS;

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries Cargo on rustc unwrap(EAGAIN) panic signature (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-unwrap-eagain-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    const invocationsFile = path.join(tmpRoot, "cargo-invocations.txt");
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${CARGO_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  echo "called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: \\"Resource temporarily unavailable\\" }" >&2
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeCargo, 0o755);

    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.CARGO_INVOCATIONS_FILE = invocationsFile;
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.RUSTFLAGS;

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries Cargo on rustc unwrap(EAGAIN) with System(Os { .. }) wrapper (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-unwrap-eagain-system-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    const invocationsFile = path.join(tmpRoot, "cargo-invocations.txt");
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${CARGO_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  echo "called Result::unwrap() on an Err value: System(Os { code: 11, kind: WouldBlock, message: \\"Resource temporarily unavailable\\" })" >&2
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeCargo, 0o755);

    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.CARGO_INVOCATIONS_FILE = invocationsFile;
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.RUSTFLAGS;

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries Cargo on multiline rustc unwrap(EAGAIN) panic signature (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-unwrap-eagain-multiline-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    const invocationsFile = path.join(tmpRoot, "cargo-invocations.txt");
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${CARGO_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  echo "called Result::unwrap() on an Err value:" >&2
  echo "Os { code: 11, kind: WouldBlock, message: \\"Resource temporarily unavailable\\" }" >&2
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeCargo, 0o755);

    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.CARGO_INVOCATIONS_FILE = invocationsFile;
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.RUSTFLAGS;

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries Cargo on ThreadPoolBuildError (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-threadpool-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    const invocationsFile = path.join(tmpRoot, "cargo-invocations.txt");
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${CARGO_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  echo "error: ThreadPoolBuildError { kind: WouldBlock, message: Resource temporarily unavailable }" >&2
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeCargo, 0o755);

    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.CARGO_INVOCATIONS_FILE = invocationsFile;
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.RUSTFLAGS;

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries Cargo on std::system_error (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-system-error-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    const invocationsFile = path.join(tmpRoot, "cargo-invocations.txt");
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${CARGO_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  echo "ld.lld: std::system_error: Resource temporarily unavailable" >&2
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeCargo, 0o755);

    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.CARGO_INVOCATIONS_FILE = invocationsFile;
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.RUSTFLAGS;

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries npm when it hits rustc EAGAIN under contention (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-npm-eagain-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    const invocationsFile = path.join(tmpRoot, "npm-invocations.txt");
    const fakeNpm = path.join(binDir, "npm");
    fs.writeFileSync(
      fakeNpm,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${NPM_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  echo "thread 'main' panicked at compiler/rustc_driver_impl/src/lib.rs:1608:6:" >&2
  echo "Unable to install ctrlc handler: System(Os { code: 11, kind: WouldBlock, message: \\"Resource temporarily unavailable\\" })" >&2
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeNpm, 0o755);

    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.NPM_INVOCATIONS_FILE = invocationsFile;
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["npm"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries Cargo when rustc panics with unwrap(EAGAIN) (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-unwrap-eagain-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    const invocationsFile = path.join(tmpRoot, "cargo-invocations.txt");
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${CARGO_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  cat >&2 <<'EOF'
thread 'rustc' panicked at 'called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }', library/core/src/result.rs:1:1
EOF
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeCargo, 0o755);

    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.CARGO_INVOCATIONS_FILE = invocationsFile;
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh retries Cargo on could not exec the linker (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-retry-linker-exec-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });

    const invocationsFile = path.join(tmpRoot, "cargo-invocations.txt");
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(
      fakeCargo,
      `#!/usr/bin/env bash
set -euo pipefail

count_file="\${CARGO_INVOCATIONS_FILE:?}"
count=0
if [[ -f "\${count_file}" ]]; then
  count="$(cat "\${count_file}")"
fi
count=$((count + 1))
echo "\${count}" > "\${count_file}"

if [[ "\${count}" -eq 1 ]]; then
  echo "error: could not exec the linker \\\`cc\\\`: Resource temporarily unavailable" >&2
  exit 1
fi
exit 0
`,
      "utf8",
    );
    fs.chmodSync(fakeCargo, 0o755);

    const fakeSleep = path.join(binDir, "sleep");
    fs.writeFileSync(fakeSleep, "#!/usr/bin/env bash\nexit 0\n", "utf8");
    fs.chmodSync(fakeSleep, 0o755);

    const env = { ...process.env };
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;
    env.CARGO_INVOCATIONS_FILE = invocationsFile;
    env.AERO_SAFE_RUN_RUSTC_RETRIES = "2";
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.RUSTFLAGS;

    execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      stdio: "ignore",
    });

    assert.equal(fs.readFileSync(invocationsFile, "utf8").trim(), "2");
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh allows overriding codegen-units via AERO_RUST_CODEGEN_UNITS (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-codegen-units-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUSTFLAGS"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_BUILD_JOBS;
    delete env.RUSTFLAGS;
    env.AERO_CARGO_BUILD_JOBS = "4";
    env.AERO_RUST_CODEGEN_UNITS = "1";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.match(stdout, /-C codegen-units=1/);
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("safe-run.sh allows overriding codegen-units via AERO_CODEGEN_UNITS (alias) (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-codegen-units-alias-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUSTFLAGS"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_BUILD_JOBS;
    delete env.RUSTFLAGS;
    env.AERO_CARGO_BUILD_JOBS = "4";
    delete env.AERO_RUST_CODEGEN_UNITS;
    env.AERO_CODEGEN_UNITS = "1";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.match(stdout, /-C codegen-units=1/);
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test(
  "safe-run.sh works via bash even if scripts lose executable bits (Linux)",
  { skip: process.platform !== "linux" },
  () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-nonexec-"));
    try {
      const tmpScripts = path.join(tmpRoot, "scripts");
      fs.mkdirSync(tmpScripts, { recursive: true });
      for (const script of ["safe-run.sh", "with-timeout.sh", "run_limited.sh"]) {
        const src = path.join(repoRoot, "scripts", script);
        const dst = path.join(tmpScripts, script);
        fs.copyFileSync(src, dst);
        // Simulate environments/filesystems that lose exec bits on checkout.
        fs.chmodSync(dst, 0o644);
      }

      execFileSync("bash", ["scripts/safe-run.sh", "true"], {
        cwd: tmpRoot,
        stdio: "ignore",
      });
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  },
);

test(
  "safe-run.sh prints actionable restore instructions if helper scripts are missing",
  { skip: process.platform === "win32" },
  () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-missing-helpers-"));
    try {
      const tmpScripts = path.join(tmpRoot, "scripts");
      fs.mkdirSync(tmpScripts, { recursive: true });

      // Copy only safe-run.sh, leaving its helper scripts missing to simulate a broken checkout.
      fs.copyFileSync(path.join(repoRoot, "scripts", "safe-run.sh"), path.join(tmpScripts, "safe-run.sh"));

      const res = spawnSync("bash", ["scripts/safe-run.sh", "true"], {
        cwd: tmpRoot,
        encoding: "utf8",
      });
      assert.notEqual(res.status, 0);
      assert.match(res.stderr, /\[safe-run\] error: missing\/empty required script: scripts\/with-timeout\.sh/);
      assert.match(res.stderr, /git checkout -- scripts/);
      assert.match(res.stderr, /git checkout -- \./);
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  },
);

test(
  "safe-run.sh prints actionable restore instructions if helper scripts are empty",
  { skip: process.platform === "win32" },
  () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-empty-helpers-"));
    try {
      const tmpScripts = path.join(tmpRoot, "scripts");
      fs.mkdirSync(tmpScripts, { recursive: true });

      fs.copyFileSync(path.join(repoRoot, "scripts", "safe-run.sh"), path.join(tmpScripts, "safe-run.sh"));
      // Simulate a broken checkout producing 0-byte tracked files.
      fs.writeFileSync(path.join(tmpScripts, "with-timeout.sh"), "", "utf8");
      fs.writeFileSync(path.join(tmpScripts, "run_limited.sh"), "", "utf8");

      const res = spawnSync("bash", ["scripts/safe-run.sh", "true"], {
        cwd: tmpRoot,
        encoding: "utf8",
      });
      assert.notEqual(res.status, 0);
      assert.match(res.stderr, /\[safe-run\] error: missing\/empty required script: scripts\/with-timeout\.sh/);
      assert.match(res.stderr, /git checkout -- scripts/);
      assert.match(res.stderr, /git checkout -- \./);
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  },
);

test("safe-run.sh prints a hint when a command times out (Linux)", { skip: process.platform !== "linux" }, () => {
  const res = spawnSync(path.join(repoRoot, "scripts/safe-run.sh"), ["bash", "-c", "sleep 3"], {
    cwd: repoRoot,
    encoding: "utf8",
    env: {
      ...process.env,
      AERO_TIMEOUT: "1",
    },
  });

  // `timeout` uses exit code 124 when it terminates the child after exceeding the timeout.
  assert.equal(res.status, 124);
  assert.match(res.stderr, /\[safe-run\] error: command exceeded timeout of 1s/);
  assert.match(res.stderr, /\[safe-run\] Tip: retry with a larger timeout/);
  // Next timeout is doubled (1 -> 2) and the original command is echoed (shell-escaped).
  assert.match(res.stderr, /\[safe-run\]\s+AERO_TIMEOUT=2 bash \.\/scripts\/safe-run\.sh/);
  assert.match(res.stderr, /\bsleep\\ 3\b/);
});
