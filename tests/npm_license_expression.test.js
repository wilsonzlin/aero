import assert from "node:assert/strict";
import test from "node:test";

import { evaluateLicenseExpression } from "../scripts/ci/check-npm-licenses.mjs";

test("npm license allowlist: parses SPDX-style OR chains", () => {
    assert.equal(evaluateLicenseExpression("MIT"), true);
    assert.equal(evaluateLicenseExpression("MIT OR Apache-2.0 OR BSD-3-Clause"), true);
});

test("npm license allowlist: parses SPDX-style AND chains", () => {
    assert.equal(evaluateLicenseExpression("MIT AND Apache-2.0 AND BSD-3-Clause"), true);
    assert.equal(evaluateLicenseExpression("MIT AND GPL-3.0"), false);
});

test("npm license allowlist: supports parentheses and separators", () => {
    assert.equal(evaluateLicenseExpression("MIT OR (Apache-2.0 AND GPL-3.0)"), true);
    assert.equal(evaluateLicenseExpression("GPL-3.0 OR (GPL-2.0 AND LGPL-2.1)"), false);
    assert.equal(evaluateLicenseExpression("MIT;Apache-2.0"), true);
});

test("npm license allowlist: normalizes common Apache 2.0 aliases", () => {
    assert.equal(evaluateLicenseExpression("Apache 2.0"), true);
    assert.equal(evaluateLicenseExpression("Apache License 2.0 OR GPL-3.0"), true);
});

test("npm license allowlist: does not throw on non-string inputs", () => {
    const hostile = {
        toString() {
            throw new Error("boom");
        },
    };
    assert.equal(evaluateLicenseExpression(hostile), false);
});

