import { spawnSync } from "node:child_process";
import { actionTimeoutMs } from "../_shared/exec.mjs";

function run(cmd, args, options = {}) {
  const res = spawnSync(cmd, args, { encoding: "utf8", timeout: actionTimeoutMs(30_000), ...options });
  if (res.stdout) process.stdout.write(res.stdout);
  if (res.stderr) process.stderr.write(res.stderr);
  return res.status ?? 1;
}

console.log("::group::Rust versions");

const rustcStatus = run("rustc", ["--version"]);
const cargoStatus = run("cargo", ["--version"]);

// These are informational; don't fail CI if rustup isn't present.
run("rustup", ["show", "active-toolchain"]);
run("rustup", ["target", "list", "--installed"]);
run("rustup", ["component", "list", "--installed"]);

console.log("::endgroup::");

if (rustcStatus !== 0 || cargoStatus !== 0) {
  process.exitCode = 1;
}

