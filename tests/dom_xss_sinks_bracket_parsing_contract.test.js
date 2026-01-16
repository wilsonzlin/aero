import assert from "node:assert/strict";
import test from "node:test";
import { findDomXssSinksInSource } from "./_helpers/dom_xss_sink_scan_helpers.js";

test("dom xss sink scan: detects bracket-notation HTML injection sinks", () => {
  const src = [
    'el["innerHTML"] = "<p>x</p>";',
    "el['outerHTML'] = '<p>x</p>';",
    'el["insertAdjacentHTML"]("beforeend", "<p>x</p>");',
    'range["createContextualFragment"]("<p>x</p>");',
  ].join("\n");

  const kinds = findDomXssSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes('["innerHTML"]'), kinds.join("\n"));
  assert.ok(kinds.includes('["outerHTML"]'), kinds.join("\n"));
  assert.ok(kinds.includes('["insertAdjacentHTML"]'), kinds.join("\n"));
  assert.ok(kinds.includes('["createContextualFragment"]'), kinds.join("\n"));
});

