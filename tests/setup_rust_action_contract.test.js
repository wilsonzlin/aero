import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { readKeyValueFile, repoRootFromTestUrl, runNodeScriptFromRepoRoot } from "./_helpers/action_contract_helpers.js";

const repoRoot = repoRootFromTestUrl(import.meta.url);

test("setup-rust action: resolves stable to pinned semver from rust-toolchain.toml", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-setup-rust-"));
  const outFile = path.join(tmp, "out.txt");

  await fs.writeFile(path.join(tmp, "rust-toolchain.toml"), '[toolchain]\nchannel = "1.92.0"\n', "utf8");
  await fs.mkdir(path.join(tmp, "scripts"), { recursive: true });
  await fs.writeFile(path.join(tmp, "scripts", "toolchains.json"), '{"rust":{"nightlyWasm":"nightly-2025-12-08"}}', "utf8");
  await fs.writeFile(outFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-rust/resolve-toolchain.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    INPUT_TOOLCHAIN: "stable",
  });

  const outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("toolchain"), "1.92.0");
});

test("setup-rust action: resolves nightly to pinned nightlyWasm from scripts/toolchains.json", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-setup-rust-"));
  const outFile = path.join(tmp, "out.txt");

  await fs.writeFile(path.join(tmp, "rust-toolchain.toml"), '[toolchain]\nchannel = "1.92.0"\n', "utf8");
  await fs.mkdir(path.join(tmp, "scripts"), { recursive: true });
  await fs.writeFile(path.join(tmp, "scripts", "toolchains.json"), '{"rust":{"nightlyWasm":"nightly-2025-12-08"}}', "utf8");
  await fs.writeFile(outFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-rust/resolve-toolchain.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    INPUT_TOOLCHAIN: "nightly",
  });

  const outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("toolchain"), "nightly-2025-12-08");
});

test("setup-rust action: cargo locked flag respects policy and Cargo.lock presence", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-setup-rust-"));
  const outFile = path.join(tmp, "out.txt");

  await fs.writeFile(outFile, "", "utf8");
  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-rust/cargo-locked-flag.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    INPUT_LOCKED: "auto",
  });
  let outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("cargo_locked_flag"), "");

  await fs.writeFile(path.join(tmp, "Cargo.lock"), "", "utf8");
  await fs.writeFile(outFile, "", "utf8");
  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-rust/cargo-locked-flag.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    INPUT_LOCKED: "auto",
  });
  outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("cargo_locked_flag"), "--locked");
});

