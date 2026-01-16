import { stripStringsAndComments } from "./js_source_scan_helpers.js";
import { isSpace, parseQuotedStringLiteral, skipWs } from "./js_scan_parse_helpers.js";

function isChildProcessSpecifier(spec) {
  return spec === "child_process" || spec === "node:child_process";
}

function escapeRegex(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function findFromKeyword(masked, startIdx) {
  const re = /\bfrom\b/gmu;
  re.lastIndex = startIdx;
  const m = re.exec(masked);
  return m ? m.index : -1;
}

export function findSubprocessSinksInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];
  const childProcessNamespaces = new Set();

  // `shell: true` is unsafe in `spawn`/`spawnSync`-style code paths.
  const shellTrueRe = /\bshell\s*:\s*true\b/gmu;
  for (;;) {
    const m = shellTrueRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "shell: true", index: m.index });
  }

  // Named imports of exec/execSync from child_process are forbidden.
  const importRe = /\bimport\b/gmu;
  for (;;) {
    const m = importRe.exec(masked);
    if (!m) break;
    const idx = m.index;

    // Skip type-only imports (`import type ...`).
    let clauseStart = skipWs(masked, idx + "import".length);
    if (masked.slice(clauseStart, clauseStart + 4) === "type") {
      const afterType = masked[clauseStart + 4] || "";
      if (isSpace(afterType) || afterType === "{" || afterType === "*") continue;
    }

    // Only handle static imports with `from <string>`. (Dynamic `import("...")` is ignored.)
    const fromIdx = findFromKeyword(masked, idx + "import".length);
    if (fromIdx < 0) continue;

    const clauseMasked = masked.slice(clauseStart, fromIdx);
    const importsExec = /\bexec\b/gmu.test(clauseMasked);
    const importsExecSync = /\bexecSync\b/gmu.test(clauseMasked);

    let j = skipWs(source, fromIdx + "from".length);
    const lit = parseQuotedStringLiteral(source, j);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;

    if (importsExec) hits.push({ kind: "import{exec} child_process", index: idx });
    if (importsExecSync) hits.push({ kind: "import{execSync} child_process", index: idx });

    // Track namespace/default import identifiers so we can catch `cp.exec(` usage.
    const nsMatch = /\*\s+as\s+([A-Za-z_$][\w$]*)/u.exec(clauseMasked);
    if (nsMatch) {
      childProcessNamespaces.add(nsMatch[1]);
      continue;
    }
    const defaultMatch = /^\s*([A-Za-z_$][\w$]*)\b/u.exec(clauseMasked);
    if (defaultMatch) childProcessNamespaces.add(defaultMatch[1]);
  }

  // CommonJS: track aliases assigned to require("child_process").
  const requireAssignRe = /\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*require\b/gmu;
  for (;;) {
    const m = requireAssignRe.exec(masked);
    if (!m) break;
    const id = m[1];
    const requireIdx = m.index + m[0].length - "require".length;

    let j = skipWs(source, requireIdx + "require".length);
    if (source[j] !== "(") continue;
    j = skipWs(source, j + 1);
    const lit = parseQuotedStringLiteral(source, j);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;
    childProcessNamespaces.add(id);
  }

  // CommonJS destructuring: forbid pulling exec/execSync out of child_process.
  const requireDestructureRe = /\b(?:const|let|var)\s*\{([^}]*)\}\s*=\s*require\b/gmu;
  for (;;) {
    const m = requireDestructureRe.exec(masked);
    if (!m) break;
    const destructure = m[1] || "";
    const hasExec = /\bexec\b/u.test(destructure);
    const hasExecSync = /\bexecSync\b/u.test(destructure);
    if (!hasExec && !hasExecSync) continue;

    const requireIdx = m.index + m[0].length - "require".length;
    let j = skipWs(source, requireIdx + "require".length);
    if (source[j] !== "(") continue;
    j = skipWs(source, j + 1);
    const lit = parseQuotedStringLiteral(source, j);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;

    if (hasExec) hits.push({ kind: "destructure exec child_process", index: m.index });
    if (hasExecSync) hits.push({ kind: "destructure execSync child_process", index: m.index });
  }

  // Catch exec/execSync usage via a namespace/default alias like `cp.exec(`.
  for (const ns of childProcessNamespaces) {
    const execCallRe = new RegExp(`\\b${escapeRegex(ns)}\\s*\\.\\s*exec\\s*\\(`, "gmu");
    for (;;) {
      const m = execCallRe.exec(masked);
      if (!m) break;
      hits.push({ kind: "child_processNamespace.exec(", index: m.index });
    }
    const execSyncCallRe = new RegExp(`\\b${escapeRegex(ns)}\\s*\\.\\s*execSync\\s*\\(`, "gmu");
    for (;;) {
      const m = execSyncCallRe.exec(masked);
      if (!m) break;
      hits.push({ kind: "child_processNamespace.execSync(", index: m.index });
    }
  }

  // require("child_process").exec(/execSync)(...) is forbidden.
  const requireRe = /\brequire\b/gmu;
  for (;;) {
    const m = requireRe.exec(masked);
    if (!m) break;
    const idx = m.index;

    let j = skipWs(source, idx + "require".length);
    if (source[j] !== "(") continue;
    j = skipWs(source, j + 1);

    const lit = parseQuotedStringLiteral(source, j);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;

    j = skipWs(source, lit.endIdxExclusive);
    if (source[j] !== ")") continue; // keep parsing simple/strict
    const afterClose = j + 1;

    const tail = masked.slice(afterClose, afterClose + 64);
    if (/^\s*\.\s*execSync\s*\(/gmu.test(tail)) {
      hits.push({ kind: "require(child_process).execSync(", index: idx });
      continue;
    }
    if (/^\s*\.\s*exec\s*\(/gmu.test(tail)) {
      hits.push({ kind: "require(child_process).exec(", index: idx });
      continue;
    }
  }

  return hits;
}

