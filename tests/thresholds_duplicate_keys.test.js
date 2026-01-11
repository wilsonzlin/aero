import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { loadThresholdPolicy } from "../tools/perf/lib/thresholds.mjs";

test("loadThresholdPolicy rejects duplicate keys (helps avoid silent JSON overrides)", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-thresholds-"));
  const policyPath = path.join(dir, "policy.json");
  try {
    // Intentionally write raw JSON with a duplicate key; using JS objects would
    // lose the duplicate before it reaches disk.
    await writeFile(
      policyPath,
      `{
  "schemaVersion": 1,
  "profiles": {
    "pr-smoke": {
      "browser": { "metrics": { "microbench_ms": { "better": "lower", "maxRegressionPct": 0.1 } } },
      "gateway": { "metrics": {} },
      "gateway": { "metrics": {} }
    }
  }
}
`,
      "utf8",
    );

    await assert.rejects(() => loadThresholdPolicy(policyPath), /duplicate key/i);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("loadThresholdPolicy can parse the repo's perf_thresholds.json", async () => {
  const policy = await loadThresholdPolicy("bench/perf_thresholds.json");
  assert.equal(policy.schemaVersion, 1);
  assert.ok(policy.profiles?.["pr-smoke"]);
  assert.ok(policy.profiles?.nightly);
});
