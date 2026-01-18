import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const scriptPath = path.join(repoRoot, "scripts", "print-pr-url.mjs");

function git(cwd, args) {
  return execFileSync("git", args, { cwd, encoding: "utf8" }).trim();
}

function initTempRepo() {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-print-pr-url-"));
  git(tmpRoot, ["init"]);

  // Ensure we have a real branch name and remote config to query.
  git(tmpRoot, ["checkout", "-b", "feature/test-branch"]);

  fs.writeFileSync(path.join(tmpRoot, "README.txt"), "hi\n", "utf8");
  git(tmpRoot, ["add", "README.txt"]);
  execFileSync(
    "git",
    ["-c", "user.email=test@example.com", "-c", "user.name=test", "commit", "-m", "init"],
    { cwd: tmpRoot, stdio: "ignore" },
  );

  git(tmpRoot, ["remote", "add", "origin", "git@github.com:example-owner/example-repo.git"]);
  return tmpRoot;
}

test("print-pr-url: prints compare URL for current branch", () => {
  const tmpRepo = initTempRepo();
  try {
    const res = spawnSync(process.execPath, [scriptPath], { cwd: tmpRepo, encoding: "utf8" });
    assert.equal(res.status, 0, `${res.stderr ?? ""}`);
    assert.equal(res.stderr ?? "", "");
    assert.equal(
      res.stdout,
      "https://github.com/example-owner/example-repo/compare/main...feature/test-branch?expand=1\n",
    );
  } finally {
    fs.rmSync(tmpRepo, { recursive: true, force: true });
  }
});

test("print-pr-url: --actions prints an Actions URL too", () => {
  const tmpRepo = initTempRepo();
  try {
    const res = spawnSync(process.execPath, [scriptPath, "--actions"], { cwd: tmpRepo, encoding: "utf8" });
    assert.equal(res.status, 0, `${res.stderr ?? ""}`);
    assert.equal(res.stderr ?? "", "");
    assert.equal(
      res.stdout,
      [
        "https://github.com/example-owner/example-repo/compare/main...feature/test-branch?expand=1",
        "https://github.com/example-owner/example-repo/actions?query=branch%3Afeature/test-branch",
        "",
      ].join("\n"),
    );
  } finally {
    fs.rmSync(tmpRepo, { recursive: true, force: true });
  }
});

test("print-pr-url: legacy AERO_PR_INCLUDE_ACTIONS_URL=1 still works", () => {
  const tmpRepo = initTempRepo();
  try {
    const res = spawnSync(process.execPath, [scriptPath], {
      cwd: tmpRepo,
      encoding: "utf8",
      env: { ...process.env, AERO_PR_INCLUDE_ACTIONS_URL: "1" },
    });
    assert.equal(res.status, 0, `${res.stderr ?? ""}`);
    assert.equal(res.stderr ?? "", "");
    assert.equal(
      res.stdout,
      [
        "https://github.com/example-owner/example-repo/compare/main...feature/test-branch?expand=1",
        "https://github.com/example-owner/example-repo/actions?query=branch%3Afeature/test-branch",
        "",
      ].join("\n"),
    );
  } finally {
    fs.rmSync(tmpRepo, { recursive: true, force: true });
  }
});

test("print-pr-url: unknown args fail with usage", () => {
  const tmpRepo = initTempRepo();
  try {
    const res = spawnSync(process.execPath, [scriptPath, "--nope"], { cwd: tmpRepo, encoding: "utf8" });
    assert.equal(res.status, 1);
    assert.match(`${res.stderr ?? ""}`, /unknown argument/i);
    assert.match(`${res.stderr ?? ""}`, /Usage: node scripts\/print-pr-url\.mjs/i);
  } finally {
    fs.rmSync(tmpRepo, { recursive: true, force: true });
  }
});

