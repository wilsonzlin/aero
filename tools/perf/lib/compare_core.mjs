import { summarize } from "./stats.mjs";

export const PERF_COMPARE_SCHEMA_VERSION = 1;

function isFiniteNumber(n) {
  return typeof n === "number" && Number.isFinite(n);
}

export function computeDeltaPct(baselineValue, candidateValue) {
  if (!isFiniteNumber(baselineValue) || !isFiniteNumber(candidateValue) || baselineValue === 0) return null;
  return (candidateValue - baselineValue) / baselineValue;
}

export function computeRegressionPct(better, baselineValue, candidateValue) {
  if (!isFiniteNumber(baselineValue) || !isFiniteNumber(candidateValue) || baselineValue === 0) return null;
  if (better === "higher") {
    return candidateValue < baselineValue ? (baselineValue - candidateValue) / baselineValue : 0;
  }
  if (better === "lower") {
    return candidateValue > baselineValue ? (candidateValue - baselineValue) / baselineValue : 0;
  }
  throw new Error(`Unsupported metric better="${better}" (expected "lower"|"higher")`);
}

export function computeImprovementPct(better, baselineValue, candidateValue) {
  if (!isFiniteNumber(baselineValue) || !isFiniteNumber(candidateValue) || baselineValue === 0) return null;
  if (better === "higher") {
    return candidateValue > baselineValue ? (candidateValue - baselineValue) / baselineValue : 0;
  }
  if (better === "lower") {
    return candidateValue < baselineValue ? (baselineValue - candidateValue) / baselineValue : 0;
  }
  throw new Error(`Unsupported metric better="${better}" (expected "lower"|"higher")`);
}

export function statsFromSamples(samples) {
  if (!Array.isArray(samples) || samples.length === 0) return null;
  const filtered = samples.filter((v) => isFiniteNumber(v));
  if (filtered.length === 0) return null;
  const s = summarize(filtered);
  const cv = isFiniteNumber(s.cv) ? s.cv : null;
  return { value: s.median, cv, n: s.n };
}

export function normalizeValueStats(valueOrStats) {
  if (valueOrStats == null) return null;
  if (isFiniteNumber(valueOrStats)) return { value: valueOrStats, cv: null, n: null };
  if (typeof valueOrStats !== "object") return null;

  const samples = valueOrStats.samples;
  if (Array.isArray(samples)) {
    return statsFromSamples(samples);
  }

  const value = valueOrStats.value ?? valueOrStats.median;
  if (!isFiniteNumber(value)) return null;
  const cv = isFiniteNumber(valueOrStats.cv) ? valueOrStats.cv : null;
  const n = Number.isFinite(valueOrStats.n) ? valueOrStats.n : null;
  return { value, cv, n };
}

function isUnstable(cvThreshold, baselineCv, candidateCv) {
  if (!isFiniteNumber(cvThreshold)) return false;
  const base = isFiniteNumber(baselineCv) ? baselineCv : null;
  const cand = isFiniteNumber(candidateCv) ? candidateCv : null;
  return (base != null && base >= cvThreshold) || (cand != null && cand >= cvThreshold);
}

export function compareCase({
  suite,
  scenario,
  metric,
  unit,
  better,
  threshold,
  baseline,
  candidate,
}) {
  if (typeof suite !== "string" || suite.length === 0) throw new Error("compareCase: suite must be a string");
  if (typeof scenario !== "string" || scenario.length === 0) throw new Error("compareCase: scenario must be a string");
  if (typeof metric !== "string" || metric.length === 0) throw new Error("compareCase: metric must be a string");
  if (better !== "lower" && better !== "higher") throw new Error(`compareCase: unsupported better="${better}"`);

  const baselineStats = normalizeValueStats(baseline);
  const candidateStats = normalizeValueStats(candidate);

  const informational = Boolean(threshold?.informational);
  const maxRegressionPct = isFiniteNumber(threshold?.maxRegressionPct) ? threshold.maxRegressionPct : null;
  const extremeCvThreshold = isFiniteNumber(threshold?.extremeCvThreshold) ? threshold.extremeCvThreshold : null;
  const minValue = isFiniteNumber(threshold?.minValue) ? threshold.minValue : null;
  const maxValue = isFiniteNumber(threshold?.maxValue) ? threshold.maxValue : null;

  if (!baselineStats) {
    const unstable = !informational;
    return {
      suite,
      scenario,
      metric,
      unit,
      better,
      threshold: {
        maxRegressionPct,
        extremeCvThreshold,
        minValue,
        maxValue,
        informational,
      },
      baseline: null,
      candidate: candidateStats,
      deltaAbs: null,
      deltaPct: null,
      regressionPct: null,
      improvementPct: null,
      informational,
      status: "missing_baseline",
      unstable,
    };
  }

  if (!candidateStats) {
    const unstable = !informational;
    return {
      suite,
      scenario,
      metric,
      unit,
      better,
      threshold: {
        maxRegressionPct,
        extremeCvThreshold,
        minValue,
        maxValue,
        informational,
      },
      baseline: baselineStats,
      candidate: null,
      deltaAbs: null,
      deltaPct: null,
      regressionPct: null,
      improvementPct: null,
      informational,
      status: "missing_candidate",
      unstable,
    };
  }

  const deltaAbs = candidateStats.value - baselineStats.value;
  const deltaPct = computeDeltaPct(baselineStats.value, candidateStats.value);
  const regressionPct = computeRegressionPct(better, baselineStats.value, candidateStats.value);
  const improvementPct = computeImprovementPct(better, baselineStats.value, candidateStats.value);

  const maxValueRegression = isFiniteNumber(maxValue) ? candidateStats.value > maxValue : false;
  const minValueRegression = isFiniteNumber(minValue) ? candidateStats.value < minValue : false;

  const regression =
    isFiniteNumber(maxRegressionPct) && regressionPct != null ? regressionPct >= maxRegressionPct : false;

  const unstable = isUnstable(extremeCvThreshold, baselineStats.cv, candidateStats.cv);

  const status =
    regression || maxValueRegression || minValueRegression
      ? informational
        ? "informational_regression"
        : "regression"
      : "ok";

  return {
    suite,
    scenario,
    metric,
    unit,
    better,
    threshold: { maxRegressionPct, extremeCvThreshold, minValue, maxValue, informational },
    baseline: baselineStats,
    candidate: candidateStats,
    deltaAbs,
    deltaPct,
    regressionPct,
    improvementPct,
    informational,
    status,
    unstable,
  };
}

export function buildCompareResult({
  suite,
  profile,
  thresholdsFile,
  baselineMeta,
  candidateMeta,
  cases,
}) {
  if (!Array.isArray(cases)) throw new Error("buildCompareResult: cases must be an array");

  const comparisons = cases.map((c) => compareCase({ suite, ...c }));

  let regressionCount = 0;
  let informationalRegressionCount = 0;
  let missingCount = 0;
  let unstableCount = 0;

  for (const c of comparisons) {
    if (c.status === "regression") regressionCount += 1;
    if (c.status === "informational_regression") informationalRegressionCount += 1;
    if (c.status === "missing_baseline" || c.status === "missing_candidate") missingCount += 1;
    if (c.unstable) unstableCount += 1;
  }

  const topRegressions = comparisons
    .filter((c) => c.regressionPct != null && c.regressionPct > 0)
    .sort((a, b) => (b.regressionPct ?? 0) - (a.regressionPct ?? 0))
    .slice(0, 5)
    .map((c) => ({
      scenario: c.scenario,
      metric: c.metric,
      unit: c.unit,
      deltaAbs: c.deltaAbs,
      deltaPct: c.deltaPct,
      regressionPct: c.regressionPct,
      informational: c.informational || c.status === "informational_regression",
    }));

  const topImprovements = comparisons
    .filter((c) => c.improvementPct != null && c.improvementPct > 0)
    .sort((a, b) => (b.improvementPct ?? 0) - (a.improvementPct ?? 0))
    .slice(0, 5)
    .map((c) => ({
      scenario: c.scenario,
      metric: c.metric,
      unit: c.unit,
      deltaAbs: c.deltaAbs,
      deltaPct: c.deltaPct,
      improvementPct: c.improvementPct,
      informational: c.informational,
    }));

  const overallStatus = unstableCount > 0 ? "unstable" : regressionCount > 0 ? "regression" : "pass";

  return {
    schemaVersion: PERF_COMPARE_SCHEMA_VERSION,
    suite,
    profile,
    thresholdsFile,
    status: overallStatus,
    baselineMeta: baselineMeta ?? null,
    candidateMeta: candidateMeta ?? null,
    summary: {
      total: comparisons.length,
      regressions: regressionCount,
      informationalRegressions: informationalRegressionCount,
      unstable: unstableCount,
      missing: missingCount,
    },
    comparisons,
    topRegressions,
    topImprovements,
  };
}

export function exitCodeForStatus(status) {
  if (status === "unstable") return 2;
  if (status === "regression") return 1;
  return 0;
}

function formatNumber(value, { decimals = 2 } = {}) {
  if (value == null || Number.isNaN(value)) return "—";
  if (!Number.isFinite(value)) return String(value);
  if (Math.abs(value) >= 1000) return Math.round(value).toString();
  return Number(value.toFixed(decimals)).toString();
}

function formatSignedNumber(value, opts) {
  if (value == null || Number.isNaN(value)) return "—";
  const sign = value > 0 ? "+" : "";
  return `${sign}${formatNumber(value, opts)}`;
}

function formatSignedPct(pct, { decimals = 2 } = {}) {
  if (pct == null || Number.isNaN(pct)) return "—";
  const sign = pct > 0 ? "+" : "";
  return `${sign}${Number((pct * 100).toFixed(decimals))}%`;
}

function coerceScalarString(value) {
  if (value == null) return "";
  switch (typeof value) {
    case "string":
      return value;
    case "number":
    case "boolean":
    case "bigint":
      return String(value);
    case "symbol":
    case "undefined":
    case "object":
    case "function":
    default:
      return "";
  }
}

function mdEscape(text) {
  return coerceScalarString(text).replaceAll("|", "\\|");
}

function renderThresholdSummary(threshold, unit) {
  const parts = [];
  if (isFiniteNumber(threshold?.maxRegressionPct)) {
    parts.push(`≤ ${(threshold.maxRegressionPct * 100).toFixed(0)}% regression`);
  }
  if (isFiniteNumber(threshold?.minValue)) {
    parts.push(`min ${formatNumber(threshold.minValue)} ${unit ?? ""}`.trim());
  }
  if (isFiniteNumber(threshold?.maxValue)) {
    parts.push(`max ${formatNumber(threshold.maxValue)} ${unit ?? ""}`.trim());
  }
  if (isFiniteNumber(threshold?.extremeCvThreshold)) {
    parts.push(`cv < ${threshold.extremeCvThreshold}`);
  }
  if (threshold?.informational) parts.push("informational");
  return parts.length === 0 ? "—" : parts.join(", ");
}

function statusLabel(status, unstable) {
  const base = (() => {
    switch (status) {
      case "ok":
        return "OK";
      case "regression":
        return "REGRESSION";
      case "informational_regression":
        return "INFO";
      case "missing_baseline":
        return "MISSING_BASELINE";
      case "missing_candidate":
        return "MISSING_CANDIDATE";
      default:
        return coerceScalarString(status) || "UNKNOWN";
    }
  })();
  return unstable ? `${base} (UNSTABLE)` : base;
}

export function renderCompareMarkdown(result, { title } = {}) {
  const lines = [];
  const header = title ?? `Perf comparison (${result.suite ?? "unknown"})`;
  lines.push(`# ${header}`);
  lines.push("");
  if (result.profile) lines.push(`- Threshold profile: \`${result.profile}\``);
  if (result.thresholdsFile) lines.push(`- Threshold policy: \`${result.thresholdsFile}\``);
  if (result.baselineMeta?.gitSha) lines.push(`- Baseline: \`${result.baselineMeta.gitSha}\``);
  if (result.candidateMeta?.gitSha) lines.push(`- Candidate: \`${result.candidateMeta.gitSha}\``);
  lines.push("");

  lines.push("## Summary");
  lines.push("");
  lines.push(`- Status: **${result.status}**`);
  lines.push(`- Total comparisons: ${result.summary?.total ?? 0}`);
  lines.push(`- Regressions: ${result.summary?.regressions ?? 0}`);
  lines.push(`- Informational regressions: ${result.summary?.informationalRegressions ?? 0}`);
  lines.push(`- Unstable metrics: ${result.summary?.unstable ?? 0}`);
  lines.push(`- Missing metrics: ${result.summary?.missing ?? 0}`);
  lines.push("");

  lines.push("## Details");
  lines.push("");
  lines.push(
    "| Scenario | Metric | Baseline | Candidate | Δ | Δ% | Baseline CV | Candidate CV | Threshold | Status |",
  );
  lines.push("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |");

  for (const c of result.comparisons ?? []) {
    const baselineValue =
      c.baseline?.value != null ? `${formatNumber(c.baseline.value)} ${c.unit ?? ""}`.trim() : "—";
    const candidateValue =
      c.candidate?.value != null ? `${formatNumber(c.candidate.value)} ${c.unit ?? ""}`.trim() : "—";
    const deltaValue =
      c.deltaAbs != null ? `${formatSignedNumber(c.deltaAbs)} ${c.unit ?? ""}`.trim() : "—";
    const deltaPctValue = c.deltaPct != null ? formatSignedPct(c.deltaPct) : "—";
    const baseCv = c.baseline?.cv != null ? formatNumber(c.baseline.cv, { decimals: 2 }) : "—";
    const candCv = c.candidate?.cv != null ? formatNumber(c.candidate.cv, { decimals: 2 }) : "—";
    const thresholdSummary = renderThresholdSummary(c.threshold ?? {}, c.unit);
    const status = statusLabel(c.status, c.unstable);
    lines.push(
      `| ${mdEscape(c.scenario)} | ${mdEscape(c.metric)} | ${baselineValue} | ${candidateValue} | ${deltaValue} | ${deltaPctValue} | ${baseCv} | ${candCv} | ${mdEscape(thresholdSummary)} | ${status} |`,
    );
  }

  lines.push("");
  return `${lines.join("\n")}\n`;
}
