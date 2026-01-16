import { stripStringsAndComments } from "./js_source_scan_helpers.js";
import { parseQuotedStringLiteral, skipWsAndComments } from "./js_scan_parse_helpers.js";

function findRegexHits(masked, re, kind) {
  const hits = [];
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    hits.push({ kind, index: m.index });
  }
  return hits;
}

function findBracketPropertyHits(source, masked, propName, kind) {
  const hits = [];
  let i = 0;
  for (;;) {
    const idx = masked.indexOf("[", i);
    if (idx < 0) break;
    i = idx + 1;

    const arg0 = skipWsAndComments(source, idx + 1);
    const lit = parseQuotedStringLiteral(source, arg0);
    if (!lit) continue;
    if (lit.value !== propName) continue;

    let j = skipWsAndComments(source, lit.endIdxExclusive);
    if ((source[j] || "") !== "]") continue;

    hits.push({ kind, index: idx });
  }
  return hits;
}

export function findDomXssSinksInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];

  // React sink.
  hits.push(...findRegexHits(masked, /\bdangerouslySetInnerHTML\b/gmu, "dangerouslySetInnerHTML"));

  // DOM sinks (dot access).
  hits.push(...findRegexHits(masked, /\.\s*innerHTML\b/gmu, ".innerHTML"));
  hits.push(...findRegexHits(masked, /\.\s*outerHTML\b/gmu, ".outerHTML"));
  hits.push(...findRegexHits(masked, /\.\s*insertAdjacentHTML\b/gmu, ".insertAdjacentHTML"));
  hits.push(...findRegexHits(masked, /\bdocument\s*\.\s*writeln?\s*\(/gmu, "document.write"));
  hits.push(...findRegexHits(masked, /\.\s*createContextualFragment\b/gmu, ".createContextualFragment"));

  // DOM sinks (bracket access): `el["innerHTML"] = ...`.
  hits.push(...findBracketPropertyHits(source, masked, "innerHTML", '["innerHTML"]'));
  hits.push(...findBracketPropertyHits(source, masked, "outerHTML", '["outerHTML"]'));
  hits.push(...findBracketPropertyHits(source, masked, "insertAdjacentHTML", '["insertAdjacentHTML"]'));
  hits.push(...findBracketPropertyHits(source, masked, "createContextualFragment", '["createContextualFragment"]'));
  hits.push(...findBracketPropertyHits(source, masked, "dangerouslySetInnerHTML", '["dangerouslySetInnerHTML"]'));

  return hits;
}

