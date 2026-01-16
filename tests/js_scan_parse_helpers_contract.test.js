import assert from "node:assert/strict";
import test from "node:test";
import { parseBracketStringProperty, parseIdentifierWithUnicodeEscapes } from "./_helpers/js_scan_parse_helpers.js";

test("js_scan_parse_helpers: parseBracketStringProperty parses quoted bracket properties", () => {
  const src = 'obj["innerHTML"]';
  const open = src.indexOf("[");
  const parsed = parseBracketStringProperty(src, open);
  assert.ok(parsed, "expected parse result");
  assert.equal(parsed.property, "innerHTML");
  assert.equal(parsed.closeBracketIdx, src.indexOf("]"));
  assert.equal(src.slice(open, parsed.endIdxExclusive), '["innerHTML"]');
});

test("js_scan_parse_helpers: parseBracketStringProperty skips whitespace and comments", () => {
  const src = 'obj[ /*a*/ "innerHTML" /*b*/ ]';
  const open = src.indexOf("[");
  const parsed = parseBracketStringProperty(src, open);
  assert.ok(parsed, "expected parse result");
  assert.equal(parsed.property, "innerHTML");
  assert.equal(src[parsed.closeBracketIdx], "]");
});

test("js_scan_parse_helpers: parseBracketStringProperty treats U+2028/U+2029 as line terminators for // comments", () => {
  const LS = "\u2028";
  const PS = "\u2029";
  assert.ok(parseBracketStringProperty(`obj[//x${LS}"innerHTML"]`, 3));
  assert.ok(parseBracketStringProperty(`obj[//x${PS}"innerHTML"]`, 3));
});

test("js_scan_parse_helpers: parseBracketStringProperty treats CR/CRLF as line terminators for // comments", () => {
  assert.ok(parseBracketStringProperty('obj[//x\r"innerHTML"]', 3));
  assert.ok(parseBracketStringProperty('obj[//x\r\n"innerHTML"]', 3));
});

test("js_scan_parse_helpers: parseBracketStringProperty decodes escapes", () => {
  const src = 'obj["wr\\u0069te"] obj["child\\x5fprocess"] obj["inn\\u{65}rHTML"]';
  const props = [];
  for (let i = 0; i < src.length; i++) {
    if (src[i] !== "[") continue;
    const parsed = parseBracketStringProperty(src, i);
    if (parsed) props.push(parsed.property);
  }
  assert.deepEqual(props, ["write", "child_process", "innerHTML"]);
});

test("js_scan_parse_helpers: parseBracketStringProperty decodes line continuations", () => {
  const LS = "\u2028";
  const PS = "\u2029";
  const src = `obj["child_\\\nprocess"] obj["child_\\\r\nprocess"] obj["child_\\${LS}process"] obj["child_\\${PS}process"] obj[\`wr\\\n\\u0069te\`]`;
  const props = [];
  for (let i = 0; i < src.length; i++) {
    if (src[i] !== "[") continue;
    const parsed = parseBracketStringProperty(src, i);
    if (parsed) props.push(parsed.property);
  }
  assert.deepEqual(props, ["child_process", "child_process", "child_process", "child_process", "write"]);
});

test("js_scan_parse_helpers: parseBracketStringProperty rejects raw line separators in quoted strings", () => {
  const LS = "\u2028";
  const PS = "\u2029";
  assert.equal(parseBracketStringProperty(`obj["in${LS}nerHTML"]`, 3), null);
  assert.equal(parseBracketStringProperty(`obj["in${PS}nerHTML"]`, 3), null);
});

test("js_scan_parse_helpers: parseBracketStringProperty parses no-substitution template literals", () => {
  const src = "obj[`innerHTML`] obj[`wr\\u0069te`] obj[`a\\x5fb`]";
  const props = [];
  for (let i = 0; i < src.length; i++) {
    if (src[i] !== "[") continue;
    const parsed = parseBracketStringProperty(src, i);
    if (parsed) props.push(parsed.property);
  }
  assert.deepEqual(props, ["innerHTML", "write", "a_b"]);
});

test("js_scan_parse_helpers: parseBracketStringProperty rejects template literals with expressions", () => {
  const src = "obj[`inn${x}erHTML`]";
  const open = src.indexOf("[");
  assert.equal(parseBracketStringProperty(src, open), null);
});

test("js_scan_parse_helpers: parseBracketStringProperty rejects non-quoted properties", () => {
  const src = "obj[innerHTML]";
  const open = src.indexOf("[");
  assert.equal(parseBracketStringProperty(src, open), null);
});

test("js_scan_parse_helpers: parseBracketStringProperty rejects missing closing bracket", () => {
  const src = 'obj["innerHTML"';
  const open = src.indexOf("[");
  assert.equal(parseBracketStringProperty(src, open), null);
});

test("js_scan_parse_helpers: parseIdentifierWithUnicodeEscapes decodes \\u escapes in identifiers", () => {
  const src = "x.inn\\u0065rHTML y.wr\\u0069te z.e\\u0076al a.m\\u{65}ssage";
  const idxs = [src.indexOf("inn"), src.indexOf("wr"), src.indexOf("e\\"), src.indexOf("m\\u{")];
  const values = idxs.map((i) => parseIdentifierWithUnicodeEscapes(src, i)?.value ?? null);
  assert.deepEqual(values, ["innerHTML", "write", "eval", "message"]);
});

test("js_scan_parse_helpers: parseIdentifierWithUnicodeEscapes rejects non-ASCII escapes", () => {
  const src = "x.\\u{1F600}";
  const idx = src.indexOf("\\u");
  assert.equal(parseIdentifierWithUnicodeEscapes(src, idx), null);
});

test("js_scan_parse_helpers: parseIdentifierWithUnicodeEscapes rejects out-of-range \\u{...} escapes", () => {
  const src = "x.\\u{110000}";
  const idx = src.indexOf("\\u");
  assert.equal(parseIdentifierWithUnicodeEscapes(src, idx), null);
});

