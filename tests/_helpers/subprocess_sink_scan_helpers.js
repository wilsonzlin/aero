import { stripStringsAndComments } from "./js_source_scan_helpers.js";

function isSpace(ch) {
  return ch === " " || ch === "\t" || ch === "\n" || ch === "\r";
}

function skipWs(text, i) {
  while (i < text.length && isSpace(text[i])) i++;
  return i;
}

function parseQuotedStringLiteral(source, quoteIdx) {
  const quote = source[quoteIdx];
  if (quote !== "'" && quote !== '"') return null;

  let i = quoteIdx + 1;
  for (;;) {
    if (i >= source.length) return null;
    const ch = source[i];
    if (ch === "\\") {
      i += 2;
      continue;
    }
    if (ch === quote) {
      return { value: source.slice(quoteIdx + 1, i), endIdxExclusive: i + 1 };
    }
    i++;
  }
}

function isChildProcessSpecifier(spec) {
  return spec === "child_process" || spec === "node:child_process";
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

    // Only handle static imports with `from <string>`. (Dynamic `import("...")` is ignored.)
    const fromIdx = findFromKeyword(masked, idx + "import".length);
    if (fromIdx < 0) continue;

    const clauseMasked = masked.slice(idx, fromIdx);
    const importsExec = /\bexec\b/gmu.test(clauseMasked);
    const importsExecSync = /\bexecSync\b/gmu.test(clauseMasked);
    if (!importsExec && !importsExecSync) continue;

    let j = skipWs(source, fromIdx + "from".length);
    const lit = parseQuotedStringLiteral(source, j);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;

    if (importsExec) hits.push({ kind: "import{exec} child_process", index: idx });
    if (importsExecSync) hits.push({ kind: "import{execSync} child_process", index: idx });
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

