'use strict';

const { coefficientOfVariation, median } = require('./stats.cjs');

const RESULTS_SCHEMA_VERSION = 1;
const THRESHOLDS_SCHEMA_VERSION = 1;

function inferBetter(name, unit) {
  const metricName = String(name ?? '');
  const metricUnit = String(unit ?? '');

  if (
    metricUnit === 'ms' ||
    metricUnit === 's' ||
    metricUnit === 'sec' ||
    metricName.endsWith('_ms') ||
    metricName.includes('time')
  ) {
    return 'lower';
  }

  if (metricUnit === 'fps' || metricName.includes('fps')) return 'higher';

  if (
    metricUnit.includes('ops') ||
    metricUnit.includes('op') ||
    metricName.includes('ops') ||
    metricName.includes('ips')
  ) {
    return 'higher';
  }

  return 'lower';
}

function normalizeScenarioRunnerReport(label, report) {
  const scenarioId = report.scenarioId;
  if (typeof scenarioId !== 'string' || scenarioId.length === 0) {
    throw new Error(`${label}: scenario runner report missing scenarioId`);
  }

  if (report.status && report.status !== 'ok') {
    throw new Error(`${label}: scenario runner report status is ${String(report.status)} (expected ok)`);
  }

  if (!Array.isArray(report.metrics)) {
    throw new Error(`${label}: scenario runner report missing metrics array`);
  }

  const metrics = {};
  for (const metric of report.metrics) {
    if (!metric || typeof metric !== 'object') {
      throw new Error(`${label}: scenario runner metric must be an object`);
    }
    if (typeof metric.id !== 'string' || metric.id.length === 0) {
      throw new Error(`${label}: scenario runner metric.id must be a non-empty string`);
    }
    if (typeof metric.unit !== 'string' || metric.unit.length === 0) {
      throw new Error(`${label}: scenario runner metric ${metric.id} must provide unit`);
    }
    if (typeof metric.value !== 'number' || !Number.isFinite(metric.value)) {
      throw new Error(`${label}: scenario runner metric ${metric.id} must provide finite value`);
    }

    metrics[metric.id] = {
      unit: metric.unit,
      better: inferBetter(metric.id, metric.unit),
      samples: [metric.value],
    };
  }

  const finishedAtMs = report.finishedAtMs;
  const recordedAt =
    typeof finishedAtMs === 'number' && Number.isFinite(finishedAtMs)
      ? new Date(finishedAtMs).toISOString()
      : undefined;

  return {
    schemaVersion: RESULTS_SCHEMA_VERSION,
    meta: {
      recordedAt,
      scenarioId,
      scenarioName: typeof report.scenarioName === 'string' ? report.scenarioName : undefined,
      source: 'scenario-runner',
    },
    scenarios: {
      [scenarioId]: {
        metrics,
      },
    },
  };
}

function normalizePerfToolResult(label, result) {
  const scenarioId = 'perf';
  if (!Array.isArray(result.benchmarks)) {
    throw new Error(`${label}: tools/perf payload missing benchmarks array`);
  }

  const metrics = {};
  for (const bench of result.benchmarks) {
    if (!bench || typeof bench !== 'object') {
      throw new Error(`${label}: tools/perf benchmark must be an object`);
    }
    if (typeof bench.name !== 'string' || bench.name.length === 0) {
      throw new Error(`${label}: tools/perf benchmark missing name`);
    }
    if (typeof bench.unit !== 'string' || bench.unit.length === 0) {
      throw new Error(`${label}: tools/perf benchmark ${bench.name} missing unit`);
    }

    let samples = bench.samples;
    if (!Array.isArray(samples) || samples.length === 0) {
      const medianFromStats = bench.stats?.median;
      if (typeof medianFromStats === 'number' && Number.isFinite(medianFromStats)) {
        samples = [medianFromStats];
      } else {
        throw new Error(`${label}: tools/perf benchmark ${bench.name} missing samples`);
      }
    }

    metrics[bench.name] = {
      unit: bench.unit,
      better: inferBetter(bench.name, bench.unit),
      samples,
    };
  }

  const perfMeta = result.meta && typeof result.meta === 'object' ? result.meta : {};
  const recordedAt =
    typeof perfMeta.collectedAt === 'string'
      ? perfMeta.collectedAt
      : typeof perfMeta.generatedAt === 'string'
        ? perfMeta.generatedAt
        : undefined;

  return {
    schemaVersion: RESULTS_SCHEMA_VERSION,
    meta: {
      recordedAt,
      gitSha: perfMeta.gitSha,
      gitRef: perfMeta.gitRef,
      nodeVersion: perfMeta.nodeVersion,
      source: 'tools/perf',
    },
    scenarios: {
      [scenarioId]: {
        metrics,
      },
    },
  };
}

function normalizeResultsFile(label, data) {
  if (!data || typeof data !== 'object') {
    throw new Error(`${label}: expected an object`);
  }

  if (data.schemaVersion === RESULTS_SCHEMA_VERSION && data.scenarios && typeof data.scenarios === 'object') {
    return data;
  }

  if (typeof data.scenarioId === 'string' && Array.isArray(data.metrics)) {
    return normalizeScenarioRunnerReport(label, data);
  }

  if (data.meta && Array.isArray(data.benchmarks)) {
    return normalizePerfToolResult(label, data);
  }

  throw new Error(
    `${label}: unsupported benchmark result format (expected {schemaVersion:1, scenarios}, scenario runner report.json, or tools/perf {meta, benchmarks})`,
  );
}

function validateResultsFile(label, data) {
  if (!data || typeof data !== 'object') {
    throw new Error(`${label}: expected an object`);
  }
  if (data.schemaVersion !== RESULTS_SCHEMA_VERSION) {
    throw new Error(
      `${label}: unsupported schemaVersion ${String(data.schemaVersion)} (expected ${RESULTS_SCHEMA_VERSION})`,
    );
  }
  if (!data.scenarios || typeof data.scenarios !== 'object') {
    throw new Error(`${label}: missing scenarios`);
  }
}

function validateThresholdsFile(data) {
  if (!data || typeof data !== 'object') {
    throw new Error('thresholds: expected an object');
  }
  if (data.schemaVersion !== THRESHOLDS_SCHEMA_VERSION) {
    throw new Error(
      `thresholds: unsupported schemaVersion ${String(data.schemaVersion)} (expected ${THRESHOLDS_SCHEMA_VERSION})`,
    );
  }
  if (!data.profiles || typeof data.profiles !== 'object') {
    throw new Error('thresholds: missing profiles');
  }
}

function pickThresholdProfile(thresholds, profileName) {
  const profile = thresholds.profiles?.[profileName];
  if (!profile) {
    const available = Object.keys(thresholds.profiles ?? {}).sort();
    throw new Error(
      `Unknown threshold profile "${profileName}". Available profiles: ${available.join(', ') || '(none)'}`,
    );
  }
  return profile;
}

function mergeThresholdRules(...rules) {
  const merged = {};
  for (const rule of rules) {
    if (!rule) continue;
    for (const [key, value] of Object.entries(rule)) {
      if (value === undefined) continue;
      merged[key] = value;
    }
  }
  return merged;
}

function resolveThresholdRule(profile, scenarioName, metricName) {
  const defaultRule = profile.default;
  const metricRule = profile.metrics?.[metricName];
  const scenarioRule = profile.scenarios?.[scenarioName]?.metrics?.[metricName];
  return mergeThresholdRules(defaultRule, metricRule, scenarioRule);
}

function metricStats(metric) {
  if (!metric || typeof metric !== 'object') {
    throw new Error('metricStats: metric must be an object');
  }
  if (!Array.isArray(metric.samples) || metric.samples.length === 0) {
    throw new Error('metricStats: samples must be a non-empty array');
  }
  const med = median(metric.samples);
  const cv = coefficientOfVariation(metric.samples);
  return {
    median: med,
    cv,
    sampleCount: metric.samples.length,
  };
}

function computeDeltaPct(baselineMedian, currentMedian) {
  if (baselineMedian === 0) return null;
  return ((currentMedian - baselineMedian) / baselineMedian) * 100;
}

function computeRegressionPct(better, baselineMedian, currentMedian) {
  if (baselineMedian === 0) return null;
  if (better === 'higher') {
    return currentMedian < baselineMedian
      ? (baselineMedian - currentMedian) / baselineMedian
      : 0;
  }
  if (better === 'lower') {
    return currentMedian > baselineMedian
      ? (currentMedian - baselineMedian) / baselineMedian
      : 0;
  }
  throw new Error(`Unsupported metric better="${better}"`);
}

function compareMetric({
  scenarioName,
  metricName,
  baselineMetric,
  currentMetric,
  thresholdRule,
}) {
  const warnings = [];

  if (!baselineMetric) {
    return {
      scenario: scenarioName,
      metric: metricName,
      status: 'missing_baseline',
      warnings: [`Missing baseline metric "${scenarioName}.${metricName}"`],
    };
  }

  if (!currentMetric) {
    return {
      scenario: scenarioName,
      metric: metricName,
      status: 'missing_current',
      warnings: [`Missing current metric "${scenarioName}.${metricName}"`],
    };
  }

  const baselineStats = metricStats(baselineMetric);
  const currentStats = metricStats(currentMetric);

  const better = baselineMetric.better ?? currentMetric.better;
  const unit = baselineMetric.unit ?? currentMetric.unit;

  if (baselineMetric.better && currentMetric.better && baselineMetric.better !== currentMetric.better) {
    warnings.push(
      `Better-direction mismatch for "${scenarioName}.${metricName}": baseline=${baselineMetric.better} current=${currentMetric.better}`,
    );
  }
  if (baselineMetric.unit && currentMetric.unit && baselineMetric.unit !== currentMetric.unit) {
    warnings.push(
      `Unit mismatch for "${scenarioName}.${metricName}": baseline=${baselineMetric.unit} current=${currentMetric.unit}`,
    );
  }

  const deltaAbs = currentStats.median - baselineStats.median;
  const deltaPct = computeDeltaPct(baselineStats.median, currentStats.median);
  const regressionPct = computeRegressionPct(better, baselineStats.median, currentStats.median);

  const breaches = [];
  if (thresholdRule?.maxValue !== undefined && currentStats.median > thresholdRule.maxValue) {
    breaches.push({
      type: 'maxValue',
      message: `value ${currentStats.median} > max ${thresholdRule.maxValue}`,
    });
  }
  if (thresholdRule?.minValue !== undefined && currentStats.median < thresholdRule.minValue) {
    breaches.push({
      type: 'minValue',
      message: `value ${currentStats.median} < min ${thresholdRule.minValue}`,
    });
  }
  if (thresholdRule?.maxRegressionPct !== undefined) {
    if (regressionPct === null) {
      warnings.push(
        `Cannot compute regression_pct for "${scenarioName}.${metricName}" because baseline median is 0`,
      );
    } else if (regressionPct > thresholdRule.maxRegressionPct) {
      breaches.push({
        type: 'maxRegressionPct',
        message: `regression ${(regressionPct * 100).toFixed(2)}% > allowed ${(thresholdRule.maxRegressionPct * 100).toFixed(2)}%`,
      });
    }
  }

  const informational = Boolean(thresholdRule?.informational);
  const status = breaches.length === 0 ? 'ok' : informational ? 'informational_regression' : 'regression';

  const cvWarnThreshold = thresholdRule?.varianceCvWarn;
  const expectedCvMax = baselineMetric.expectedCvMax;
  const varianceWarnings = [];
  if (currentStats.cv !== null) {
    if (cvWarnThreshold !== undefined && currentStats.cv > cvWarnThreshold) {
      varianceWarnings.push(`cv ${(currentStats.cv * 100).toFixed(2)}% > warn ${(cvWarnThreshold * 100).toFixed(2)}%`);
    }
    if (expectedCvMax !== undefined && currentStats.cv > expectedCvMax) {
      varianceWarnings.push(
        `cv ${(currentStats.cv * 100).toFixed(2)}% > baseline expected ${(expectedCvMax * 100).toFixed(2)}%`,
      );
    }
  }

  return {
    scenario: scenarioName,
    metric: metricName,
    unit,
    better,
    baseline: {
      median: baselineStats.median,
      sampleCount: baselineStats.sampleCount,
      cv: baselineStats.cv,
    },
    current: {
      median: currentStats.median,
      sampleCount: currentStats.sampleCount,
      cv: currentStats.cv,
    },
    deltaAbs,
    deltaPct,
    regressionPct,
    threshold: thresholdRule ?? {},
    breaches,
    informational,
    status,
    varianceWarnings,
    warnings,
  };
}

function compareResults({ baseline, current, thresholds, profileName }) {
  const normalizedBaseline = normalizeResultsFile('baseline', baseline);
  const normalizedCurrent = normalizeResultsFile('current', current);

  validateResultsFile('baseline', normalizedBaseline);
  validateResultsFile('current', normalizedCurrent);
  validateThresholdsFile(thresholds);
  const profile = pickThresholdProfile(thresholds, profileName);

  const scenarios = new Set([
    ...Object.keys(normalizedBaseline.scenarios ?? {}),
    ...Object.keys(normalizedCurrent.scenarios ?? {}),
  ]);

  const comparisons = [];
  const warnings = [];

  for (const scenarioName of [...scenarios].sort()) {
    const baselineScenario = normalizedBaseline.scenarios?.[scenarioName];
    const currentScenario = normalizedCurrent.scenarios?.[scenarioName];

    const metricNames = new Set([
      ...Object.keys(baselineScenario?.metrics ?? {}),
      ...Object.keys(currentScenario?.metrics ?? {}),
    ]);

    if (!baselineScenario) {
      warnings.push(`Scenario "${scenarioName}" is present in current results but missing from baseline`);
    }
    if (!currentScenario) {
      warnings.push(`Scenario "${scenarioName}" is present in baseline but missing from current results`);
    }

    for (const metricName of [...metricNames].sort()) {
      const baselineMetric = baselineScenario?.metrics?.[metricName];
      const currentMetric = currentScenario?.metrics?.[metricName];
      const thresholdRule = resolveThresholdRule(profile, scenarioName, metricName);
      const comparison = compareMetric({
        scenarioName,
        metricName,
        baselineMetric,
        currentMetric,
        thresholdRule,
      });

      comparisons.push(comparison);
      warnings.push(...(comparison.warnings ?? []));
    }
  }

  let regressionCount = 0;
  let informationalRegressionCount = 0;
  let missingCount = 0;
  let varianceWarningCount = 0;

  for (const comparison of comparisons) {
    if (comparison.status === 'regression') regressionCount += 1;
    if (comparison.status === 'informational_regression') informationalRegressionCount += 1;
    if (comparison.status === 'missing_baseline' || comparison.status === 'missing_current') missingCount += 1;
    if (comparison.varianceWarnings && comparison.varianceWarnings.length > 0) varianceWarningCount += 1;
  }

  return {
    schemaVersion: 1,
    profile: profileName,
    baselineMeta: normalizedBaseline.meta ?? {},
    currentMeta: normalizedCurrent.meta ?? {},
    summary: {
      total: comparisons.length,
      regressions: regressionCount,
      informationalRegressions: informationalRegressionCount,
      missing: missingCount,
      varianceWarnings: varianceWarningCount,
      warnings: warnings.length,
    },
    warnings,
    comparisons,
  };
}

module.exports = {
  RESULTS_SCHEMA_VERSION,
  THRESHOLDS_SCHEMA_VERSION,
  compareResults,
  computeDeltaPct,
  computeRegressionPct,
  mergeThresholdRules,
  normalizeResultsFile,
  pickThresholdProfile,
  resolveThresholdRule,
};
