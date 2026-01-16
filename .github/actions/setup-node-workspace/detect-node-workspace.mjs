import fs from "node:fs";
import path from "node:path";
import { appendOutput, fail, normalizeRel, resolveWorkspaceRoot } from "../_shared/github_io.mjs";

function absFromWorkspace(relOrAbs) {
  const workspace = resolveWorkspaceRoot();
  const p = String(relOrAbs ?? "").trim() || ".";
  if (path.isAbsolute(p)) return p;
  return path.resolve(workspace, p);
}

function normalizeAbs(p) {
  const s = String(p ?? "").trim();
  if (!s) return s;
  return s.replace(/[\\/]+$/u, "");
}

const workingDirectory = process.env.AERO_ACTION_WORKING_DIRECTORY ?? ".";
const rootRel = normalizeRel(workingDirectory && workingDirectory.trim() ? workingDirectory.trim() : ".");
const rootAbs = absFromWorkspace(rootRel);

const overrideRaw = (process.env.AERO_NODE_DIR ?? process.env.AERO_WEB_DIR ?? "").trim();

const workspaceAbs = resolveWorkspaceRoot();

let dirAbs = "";
let dirOut = "";
if (overrideRaw) {
  const overrideTrim = overrideRaw.trim();
  const overrideNormalized = overrideTrim.replaceAll("\\", "/");
  if (overrideNormalized === ".") {
    dirAbs = rootAbs;
    dirOut = rootRel;
  } else if (path.isAbsolute(overrideTrim)) {
    dirAbs = normalizeAbs(overrideTrim);
    dirOut = dirAbs;
  } else {
    const rel = rootRel === "." ? normalizeRel(overrideNormalized) : normalizeRel(`${rootRel}/${overrideNormalized}`);
    dirAbs = absFromWorkspace(rel);
    dirOut = rel;
  }

  if (!fs.existsSync(path.join(dirAbs, "package.json"))) {
    fail(`error: override directory '${overrideRaw}' does not contain package.json\nSet AERO_NODE_DIR to a directory containing package.json.`);
  }
} else {
  const candidatesAbs = [rootAbs, path.join(rootAbs, "frontend"), path.join(rootAbs, "web")];
  for (const candidateAbs of candidatesAbs) {
    if (fs.existsSync(path.join(candidateAbs, "package.json"))) {
      dirAbs = candidateAbs;
      break;
    }
  }
  if (!dirAbs) {
    fail(`error: unable to locate package.json under '${rootRel}' (set AERO_NODE_DIR to override)`);
  }
  const rel = path.relative(workspaceAbs, dirAbs);
  dirOut = normalizeRel(rel);
}

const lockfileAbs = fs.existsSync(path.join(dirAbs, "package-lock.json"))
  ? path.join(dirAbs, "package-lock.json")
  : fs.existsSync(path.join(rootAbs, "package-lock.json"))
    ? path.join(rootAbs, "package-lock.json")
    : "";

if (!lockfileAbs) {
  fail(`error: package-lock.json not found in '${dirOut}' or '${rootRel}' (npm ci requires a lockfile)`);
}

let lockfileOut = "";
if (overrideRaw && path.isAbsolute(overrideRaw.trim())) {
  lockfileOut = normalizeAbs(lockfileAbs);
} else {
  lockfileOut = normalizeRel(path.relative(workspaceAbs, lockfileAbs));
}

const installDirOut = path.isAbsolute(lockfileOut) ? path.dirname(lockfileOut) : normalizeRel(path.posix.dirname(lockfileOut));

appendOutput("dir", dirOut);
appendOutput("lockfile", lockfileOut);
appendOutput("install_dir", installDirOut || ".");
