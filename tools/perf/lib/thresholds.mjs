import fs from "node:fs/promises";

export const PERF_THRESHOLDS_SCHEMA_VERSION = 1;
export const DEFAULT_PROFILE = "pr-smoke";
export const DEFAULT_THRESHOLDS_FILE = "bench/perf_thresholds.json";

function parseJsonNoDuplicateKeys(raw, label = "JSON") {
  let i = 0;
  const len = raw.length;

  const stack = [];

  const isWhitespace = (c) => c === " " || c === "\n" || c === "\r" || c === "\t";

  const skipWhitespace = () => {
    while (i < len && isWhitespace(raw[i])) i += 1;
  };

  const formatPath = () => {
    const parts = [];
    for (const ctx of stack) {
      if (ctx.pathKey != null) parts.push(ctx.pathKey);
    }
    return parts.join(".");
  };

  const parseStringLiteral = () => {
    const start = i;
    i += 1; // opening quote
    while (i < len) {
      const c = raw[i];
      if (c === '"') {
        i += 1;
        break;
      }
      if (c === "\\") {
        i += 1; // backslash
        if (i >= len) {
          throw new Error(`${label} contains an unterminated string`);
        }
        if (raw[i] === "u") {
          // Skip `uXXXX`.
          if (i + 5 > len) {
            throw new Error(`${label} contains an unterminated string`);
          }
          i += 5;
        } else {
          i += 1;
        }
        continue;
      }
      i += 1;
    }

    if (i > len) {
      throw new Error(`${label} contains an unterminated string`);
    }

    const token = raw.slice(start, i);
    const value = JSON.parse(token);
    if (typeof value !== "string") {
      throw new Error(`${label} contains a non-string object key`);
    }
    return value;
  };

  const parsePrimitive = () => {
    const start = i;
    while (i < len) {
      const c = raw[i];
      if (isWhitespace(c) || c === "," || c === "}" || c === "]") break;
      i += 1;
    }
    const token = raw.slice(start, i);
    // Validate token; this keeps our parser aligned with JSON semantics.
    JSON.parse(token);
  };

  const parseValue = (pathKeyForContainer) => {
    skipWhitespace();
    if (i >= len) throw new Error(`${label} ended unexpectedly`);

    const c = raw[i];
    if (c === "{") {
      parseObject(pathKeyForContainer);
      return;
    }
    if (c === "[") {
      parseArray(pathKeyForContainer);
      return;
    }
    if (c === '"') {
      parseStringLiteral();
      return;
    }
    parsePrimitive();
  };

  const parseObject = (pathKey) => {
    i += 1; // '{'
    const ctx = { type: "object", keys: new Set(), pathKey };
    stack.push(ctx);

    skipWhitespace();
    if (raw[i] === "}") {
      i += 1;
      stack.pop();
      return;
    }

    while (true) {
      skipWhitespace();
      if (raw[i] !== '"') {
        throw new Error(`${label} contains an invalid object key at ${formatPath() || "<root>"}`);
      }

      const key = parseStringLiteral();
      if (ctx.keys.has(key)) {
        const path = formatPath();
        const where = path ? ` at "${path}"` : "";
        throw new Error(`${label} contains a duplicate key "${key}"${where}`);
      }
      ctx.keys.add(key);

      skipWhitespace();
      if (raw[i] !== ":") {
        throw new Error(`${label} contains an invalid object entry for key "${key}"`);
      }
      i += 1;

      parseValue(key);

      skipWhitespace();
      if (raw[i] === ",") {
        i += 1;
        continue;
      }
      if (raw[i] === "}") {
        i += 1;
        stack.pop();
        return;
      }
      throw new Error(`${label} contains an invalid object delimiter after key "${key}"`);
    }
  };

  const parseArray = (pathKey) => {
    i += 1; // '['
    stack.push({ type: "array", pathKey });

    skipWhitespace();
    if (raw[i] === "]") {
      i += 1;
      stack.pop();
      return;
    }

    while (true) {
      parseValue(null);

      skipWhitespace();
      if (raw[i] === ",") {
        i += 1;
        continue;
      }
      if (raw[i] === "]") {
        i += 1;
        stack.pop();
        return;
      }
      throw new Error(`${label} contains an invalid array delimiter at ${formatPath() || "<root>"}`);
    }
  };

  parseValue(null);
  skipWhitespace();
  if (i !== len) {
    throw new Error(`${label} contains trailing content`);
  }

  return JSON.parse(raw);
}

function assertObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
}

export function validateThresholdPolicy(policy) {
  assertObject(policy, "threshold policy");
  if (policy.schemaVersion !== PERF_THRESHOLDS_SCHEMA_VERSION) {
    const got =
      typeof policy.schemaVersion === "number" || typeof policy.schemaVersion === "string"
        ? policy.schemaVersion
        : "unknown";
    throw new Error(
      `Unsupported threshold policy schemaVersion ${got} (expected ${PERF_THRESHOLDS_SCHEMA_VERSION})`,
    );
  }
  assertObject(policy.profiles, "threshold policy profiles");
}

export async function loadThresholdPolicy(filePath) {
  const raw = await fs.readFile(filePath, "utf8");
  const parsed = parseJsonNoDuplicateKeys(raw, `threshold policy (${filePath})`);
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
