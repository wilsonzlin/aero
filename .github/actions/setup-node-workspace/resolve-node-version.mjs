import fs from "node:fs";
import path from "node:path";
import { appendOutput, fail, resolveWorkspaceRoot } from "../_shared/github_io.mjs";

function toAbsRootDir(workingDirectory) {
  const root = workingDirectory && workingDirectory.trim() ? workingDirectory.trim() : ".";
  const workspace = resolveWorkspaceRoot();
  return path.isAbsolute(root) ? root : path.resolve(workspace, root);
}

const requested = (process.env.AERO_ACTION_NODE_VERSION ?? "").trim();
const workingDirectory = process.env.AERO_ACTION_WORKING_DIRECTORY ?? ".";
const absRoot = toAbsRootDir(workingDirectory);

let version = "";
if (requested) {
  version = requested;
} else {
  const nvmrc = path.join(absRoot, ".nvmrc");
  if (!fs.existsSync(nvmrc)) {
    fail(`error: .nvmrc not found at '${path.join(workingDirectory || ".", ".nvmrc")}' (set the action input node-version to override)`);
  }
  version = fs.readFileSync(nvmrc, "utf8");
}

// Trim whitespace / CRLF and tolerate `.nvmrc` values prefixed with `v`.
version = version.replaceAll("\r", "").trim();
if (version.startsWith("v")) version = version.slice(1);
if (!version) fail("error: resolved empty Node version");
if (!/^[0-9]+\.[0-9]+\.[0-9]+$/.test(version)) {
  fail(`error: expected an exact Node version like 22.11.0 in ${workingDirectory || "."}/.nvmrc; got '${version}'`);
}

appendOutput("node_version", version);
