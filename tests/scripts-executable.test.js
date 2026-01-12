import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

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

test("safe-run.sh sets rustc codegen-units based on CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-cargo-env-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUSTFLAGS"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_BUILD_JOBS;
    delete env.AERO_CARGO_BUILD_JOBS;
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.RUSTFLAGS;
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

test("safe-run.sh sets codegen-units based on AERO_CARGO_BUILD_JOBS (Linux)", { skip: process.platform !== "linux" }, () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-cargo-jobs-"));
  try {
    const binDir = path.join(tmpRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const fakeCargo = path.join(binDir, "cargo");
    fs.writeFileSync(fakeCargo, '#!/usr/bin/env bash\nprintf "%s" "$RUSTFLAGS"\n');
    fs.chmodSync(fakeCargo, 0o755);

    const env = { ...process.env };
    delete env.CARGO_BUILD_JOBS;
    delete env.RUSTFLAGS;
    delete env.AERO_RUST_CODEGEN_UNITS;
    env.AERO_CARGO_BUILD_JOBS = "2";
    env.PATH = `${binDir}${path.delimiter}${env.PATH || ""}`;

    const stdout = execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["cargo"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.match(stdout, /-C codegen-units=2/);
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
