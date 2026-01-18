import assert from "node:assert/strict";
import test from "node:test";

import { unrefBestEffort } from "../src/unref_safe.js";
import unrefSafeCjs from "../src/unref_safe.cjs";
import { unrefBestEffort as unrefBestEffortWeb } from "../web/src/unrefSafe.ts";

const implementations = [
  { name: "esm", unrefBestEffort },
  { name: "cjs", unrefBestEffort: unrefSafeCjs.unrefBestEffort },
  { name: "web", unrefBestEffort: unrefBestEffortWeb },
];

for (const impl of implementations) {
  test(`unref_safe (${impl.name}): calls unref() when present`, () => {
    let called = 0;
    const handle = {
      unref() {
        called += 1;
      },
    };

    impl.unrefBestEffort(handle);
    assert.equal(called, 1);
  });

  test(`unref_safe (${impl.name}): no-op when unref is missing or not a function`, () => {
    assert.doesNotThrow(() => impl.unrefBestEffort(null));
    assert.doesNotThrow(() => impl.unrefBestEffort(undefined));
    assert.doesNotThrow(() => impl.unrefBestEffort(123));
    assert.doesNotThrow(() => impl.unrefBestEffort({}));
    assert.doesNotThrow(() => impl.unrefBestEffort({ unref: 123 }));
  });

  test(`unref_safe (${impl.name}): does not throw if unref getter throws`, () => {
    const handle = {
      get unref() {
        throw new Error("boom");
      },
    };
    assert.doesNotThrow(() => impl.unrefBestEffort(handle));
  });

  test(`unref_safe (${impl.name}): does not throw if unref() throws`, () => {
    const handle = {
      unref() {
        throw new Error("boom");
      },
    };
    assert.doesNotThrow(() => impl.unrefBestEffort(handle));
  });
}

