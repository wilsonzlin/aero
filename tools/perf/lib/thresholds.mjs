import fs from "node:fs/promises";

export const PERF_THRESHOLDS_SCHEMA_VERSION = 1;
export const DEFAULT_PROFILE = "pr-smoke";
export const DEFAULT_THRESHOLDS_FILE = "bench/perf_thresholds.json";

function assertObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
}

export function validateThresholdPolicy(policy) {
  assertObject(policy, "threshold policy");
  if (policy.schemaVersion !== PERF_THRESHOLDS_SCHEMA_VERSION) {
    throw new Error(
      `Unsupported threshold policy schemaVersion ${String(policy.schemaVersion)} (expected ${PERF_THRESHOLDS_SCHEMA_VERSION})`,
    );
  }
  assertObject(policy.profiles, "threshold policy profiles");
}

export async function loadThresholdPolicy(filePath) {
  const raw = await fs.readFile(filePath, "utf8");
  const parsed = JSON.parse(raw);
  validateThresholdPolicy(parsed);
  return parsed;
}

export function pickThresholdProfile(policy, profileName = DEFAULT_PROFILE) {
  validateThresholdPolicy(policy);
  const name = profileName || DEFAULT_PROFILE;
  const profile = policy.profiles?.[name];
  if (!profile) {
    const available = Object.keys(policy.profiles ?? {}).sort();
    throw new Error(
      `Unknown threshold profile "${name}". Available profiles: ${available.join(", ") || "(none)"}`,
    );
  }
  return { name, profile };
}

export function getSuiteThresholds(profile, suiteName) {
  assertObject(profile, "threshold profile");
  const suite = profile[suiteName];
  if (!suite) {
    throw new Error(`Threshold profile missing suite "${suiteName}"`);
  }
  assertObject(suite, `threshold profile suite "${suiteName}"`);
  assertObject(suite.metrics, `threshold profile suite "${suiteName}".metrics`);
  return suite;
}

export function getMetricThreshold(profile, suiteName, metricName) {
  const suite = getSuiteThresholds(profile, suiteName);
  const metric = suite.metrics?.[metricName];
  return metric ?? null;
}
