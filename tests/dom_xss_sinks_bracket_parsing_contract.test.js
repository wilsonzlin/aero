import assert from "node:assert/strict";
import test from "node:test";
import { findDomXssSinksInSource } from "./_helpers/dom_xss_sink_scan_helpers.js";

test("dom xss sink scan: detects bracket-notation HTML injection sinks", () => {
  const src = [
    'el["innerHTML"] = "<p>x</p>";',
    "el['outerHTML'] = '<p>x</p>';",
    'el["insertAdjacentHTML"]("beforeend", "<p>x</p>");',
    'range["createContextualFragment"]("<p>x</p>");',
    "el[`innerHTML`] = '<p>x</p>';",
  ].join("\n");

  const kinds = findDomXssSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes('["innerHTML"]'), kinds.join("\n"));
  assert.ok(kinds.includes('["outerHTML"]'), kinds.join("\n"));
  assert.ok(kinds.includes('["insertAdjacentHTML"]'), kinds.join("\n"));
  assert.ok(kinds.includes('["createContextualFragment"]'), kinds.join("\n"));
});

test("dom xss sink scan: detects optional-chaining dot-access HTML injection sinks", () => {
  const src = [
    'el?.innerHTML = "<p>x</p>";',
    "el?.outerHTML = '<p>x</p>';",
    'el?.insertAdjacentHTML("beforeend", "<p>x</p>");',
    'range?.createContextualFragment("<p>x</p>");',
    'document?.write("<p>x</p>");',
    'document?.writeln("<p>x</p>");',
  ].join("\n");

  const kinds = findDomXssSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes(".innerHTML"), kinds.join("\n"));
  assert.ok(kinds.includes(".outerHTML"), kinds.join("\n"));
  assert.ok(kinds.includes(".insertAdjacentHTML"), kinds.join("\n"));
  assert.ok(kinds.includes(".createContextualFragment"), kinds.join("\n"));
  assert.ok(kinds.includes("document.write"), kinds.join("\n"));
});

test("dom xss sink scan: detects unicode-escaped dot-access HTML injection sinks", () => {
  const src = [
    'el.inn\\u0065rHTML = "<p>x</p>";',
    'el.inn\\u{65}rHTML = "<p>x2</p>";',
    "el.ou\\u0074erHTML = '<p>x</p>';",
    "el.ou\\u{74}erHTML = '<p>x2</p>';",
    'el.insertAdjacentH\\u0054ML("beforeend", "<p>x</p>");',
    'el.insertAdjacentH\\u{54}ML("beforeend", "<p>x2</p>");',
    'range.createContextualFr\\u0061gment("<p>x</p>");',
    'range.createContextualFr\\u{61}gment("<p>x2</p>");',
    'document.wr\\u0069te("<p>x</p>");',
    'document.wr\\u{69}te("<p>x2</p>");',
  ].join("\n");
  const kinds = findDomXssSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes(".innerHTML"), kinds.join("\n"));
  assert.ok(kinds.includes(".outerHTML"), kinds.join("\n"));
  assert.ok(kinds.includes(".insertAdjacentHTML"), kinds.join("\n"));
  assert.ok(kinds.includes(".createContextualFragment"), kinds.join("\n"));
  assert.ok(kinds.includes("document.write"), kinds.join("\n"));
});

test("dom xss sink scan: detects document[write/writeln] bracket-call sinks", () => {
  const src = [
    'document["write"]("<p>x</p>");',
    'document["wr\\u0069te"]("<p>x2</p>");',
    'const w = document["write"];',
    'const w2 = document?.["writeln"];',
    'const w3 = document["wri\\x74eln"];',
    "document[`write`]('<p>x3</p>');",
    "const w4 = document[`writeln`];",
    'document["writeln"]("<p>z</p>");',
  ].join("\n");

  const kinds = findDomXssSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("document.write"), kinds.join("\n"));
});

test("dom xss sink scan: does not flag array literals containing sink-like strings", () => {
  const src = [
    'return ["innerHTML"];',
    'const x = ["outerHTML"];',
    'const y = ["insertAdjacentHTML"];',
    'const z = ["createContextualFragment"];',
  ].join("\n");
  assert.deepEqual(findDomXssSinksInSource(src), []);
});

test("dom xss sink scan: does not flag array-literal statements after control-flow headers", () => {
  const src = [
    'if (x) ["innerHTML"];',
    'while (x) ["outerHTML"];',
    'for (;;) ["insertAdjacentHTML"];',
    'try {} catch (e) ["createContextualFragment"];',
    'with (obj) ["innerHTML"];',
    'switch (x) { default: ["outerHTML"]; }',
  ].join("\n");
  assert.deepEqual(findDomXssSinksInSource(src), []);
});

