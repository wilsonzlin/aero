import fs from "node:fs";
import path from "node:path";
import { appendOutput, fail, resolveWorkspaceRoot } from "../_shared/github_io.mjs";

const workspace = resolveWorkspaceRoot();
const policy = (process.env.INPUT_LOCKED ?? "always").trim() || "always";
const hasLock = fs.existsSync(path.join(workspace, "Cargo.lock"));

let flag = "";
switch (policy) {
  case "always":
    flag = "--locked";
    break;
  case "never":
    flag = "";
    break;
  case "auto":
    flag = hasLock ? "--locked" : "";
    break;
  default:
    fail(`::error::Invalid locked policy '${policy}' (expected auto|always|never).`);
}

appendOutput("cargo_locked_flag", flag);

console.log("::group::Cargo.lock policy");
console.log(flag ? `Using cargo ${flag}` : "Not using cargo --locked");
console.log(`Cargo.lock: ${hasLock ? "present" : "missing"}`);
console.log("::endgroup::");

