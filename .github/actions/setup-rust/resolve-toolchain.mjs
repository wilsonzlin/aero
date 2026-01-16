import path from "node:path";
import fs from "node:fs";
import { appendOutput, fail, resolveWorkspaceRoot } from "../_shared/github_io.mjs";

function readText(filePath) {
  try {
    return fs.readFileSync(filePath, "utf8");
  } catch {
    return null;
  }
}


function resolvePinnedStable(workspace) {
  const p = path.join(workspace, "rust-toolchain.toml");
  const raw = readText(p);
  if (raw === null) fail("::error::setup-rust: rust-toolchain.toml not found (required to resolve pinned stable toolchain).");

  // Minimal TOML extraction: find `channel = "..."` anywhere in the file.
  const m = raw.match(/(^|\s)channel\s*=\s*"([^"]+)"/m);
  const pinned = m?.[2] ?? "";
  if (!pinned) fail("::error::setup-rust: unable to resolve toolchain.channel from rust-toolchain.toml.");
  if (pinned === "stable") fail("::error::setup-rust: rust-toolchain.toml must pin stable to an explicit version (channel must not be 'stable').");
  if (!/^[0-9]+\.[0-9]+\.[0-9]+$/.test(pinned)) {
    fail(`::error::setup-rust: rust-toolchain.toml channel must look like a semver (e.g. 1.92.0); got '${pinned}'.`);
  }
  return pinned;
}

function resolvePinnedNightly(workspace) {
  const p = path.join(workspace, "scripts", "toolchains.json");
  const raw = readText(p);
  if (raw === null) fail("::error::setup-rust: scripts/toolchains.json not found (required to resolve pinned nightly toolchain).");

  let json;
  try {
    json = JSON.parse(raw);
  } catch (err) {
    fail(`::error::setup-rust: failed to parse scripts/toolchains.json: ${String(err)}`);
  }

  const pinned = String(json?.rust?.nightlyWasm ?? "");
  if (!pinned) fail("::error::setup-rust: unable to resolve rust.nightlyWasm from scripts/toolchains.json.");
  if (!/^nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}$/.test(pinned)) {
    fail(`::error::setup-rust: rust.nightlyWasm must be pinned to nightly-YYYY-MM-DD; got '${pinned}'.`);
  }
  return pinned;
}

const workspace = resolveWorkspaceRoot();
const requested = (process.env.INPUT_TOOLCHAIN ?? "").trim() || "stable";
if (!requested) fail("::error::setup-rust: 'toolchain' input must not be empty.");

let resolved = requested;
if (requested === "stable") resolved = resolvePinnedStable(workspace);
if (requested === "nightly") resolved = resolvePinnedNightly(workspace);

appendOutput("toolchain", resolved);

console.log("::group::Resolved Rust toolchain");
console.log(`requested: ${requested}`);
console.log(`resolved: ${resolved}`);
console.log("::endgroup::");

