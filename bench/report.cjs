'use strict';

function formatNumber(value, { decimals = 2 } = {}) {
  if (value === null || value === undefined || Number.isNaN(value)) return '—';
  if (!Number.isFinite(value)) return String(value);
  if (Math.abs(value) >= 1000) return Math.round(value).toString();
  const rounded = Number(value.toFixed(decimals));
  return rounded.toString();
}

function formatSigned(value, opts) {
  if (value === null || value === undefined || Number.isNaN(value)) return '—';
  const sign = value > 0 ? '+' : '';
  return `${sign}${formatNumber(value, opts)}`;
}

function formatPct(value, { decimals = 2 } = {}) {
  if (value === null || value === undefined || Number.isNaN(value)) return '—';
  const sign = value > 0 ? '+' : '';
  return `${sign}${Number(value.toFixed(decimals))}%`;
}

function renderThresholdSummary(thresholdRule, unit) {
  const parts = [];
  if (thresholdRule.maxRegressionPct !== undefined) {
    parts.push(`≤ ${(thresholdRule.maxRegressionPct * 100).toFixed(0)}% regression`);
  }
  if (thresholdRule.minValue !== undefined) {
    parts.push(`min ${thresholdRule.minValue} ${unit ?? ''}`.trim());
  }
  if (thresholdRule.maxValue !== undefined) {
    parts.push(`max ${thresholdRule.maxValue} ${unit ?? ''}`.trim());
  }
  if (thresholdRule.informational) parts.push('informational');
  return parts.length === 0 ? '—' : parts.join(', ');
}

function statusLabel(status) {
  switch (status) {
    case 'ok':
      return 'OK';
    case 'regression':
      return 'REGRESSION';
    case 'informational_regression':
      return 'INFO';
    case 'missing_baseline':
      return 'MISSING_BASELINE';
    case 'missing_current':
      return 'MISSING_CURRENT';
    default:
      return String(status);
  }
}

function renderMarkdown(compareResult) {
  const lines = [];

  lines.push('# Benchmark regression report');
  lines.push('');
  lines.push(`- Threshold profile: \`${compareResult.profile}\``);
  if (compareResult.baselineMeta?.recordedAt) {
    lines.push(`- Baseline recorded at: \`${compareResult.baselineMeta.recordedAt}\``);
  }
  if (compareResult.currentMeta?.recordedAt) {
    lines.push(`- Current recorded at: \`${compareResult.currentMeta.recordedAt}\``);
  }
  lines.push('');

  lines.push('## Summary');
  lines.push('');
  lines.push(`- Total comparisons: ${compareResult.summary.total}`);
  lines.push(`- Regressions: ${compareResult.summary.regressions}`);
  lines.push(`- Informational regressions: ${compareResult.summary.informationalRegressions}`);
  lines.push(`- Missing metrics: ${compareResult.summary.missing}`);
  lines.push(`- Variance warnings: ${compareResult.summary.varianceWarnings}`);
  lines.push('');

  lines.push('## Details');
  lines.push('');
  lines.push('| Scenario | Metric | Baseline (median) | Current (median) | Δ | Δ% | Threshold | Status |');
  lines.push('| --- | --- | ---: | ---: | ---: | ---: | --- | --- |');

  for (const c of compareResult.comparisons) {
    const baselineValue =
      c.baseline?.median !== undefined ? `${formatNumber(c.baseline.median)} ${c.unit ?? ''}`.trim() : '—';
    const currentValue =
      c.current?.median !== undefined ? `${formatNumber(c.current.median)} ${c.unit ?? ''}`.trim() : '—';
    const deltaValue = c.deltaAbs !== undefined ? `${formatSigned(c.deltaAbs)} ${c.unit ?? ''}`.trim() : '—';
    const deltaPctValue = c.deltaPct !== undefined && c.deltaPct !== null ? formatPct(c.deltaPct) : '—';
    const thresholdSummary = renderThresholdSummary(c.threshold ?? {}, c.unit);
    const status = statusLabel(c.status);

    lines.push(
      `| ${c.scenario} | ${c.metric} | ${baselineValue} | ${currentValue} | ${deltaValue} | ${deltaPctValue} | ${thresholdSummary} | ${status} |`,
    );
  }

  const varianceWarnComparisons = compareResult.comparisons.filter(
    (c) => Array.isArray(c.varianceWarnings) && c.varianceWarnings.length > 0,
  );
  if (varianceWarnComparisons.length > 0) {
    lines.push('');
    lines.push('## Variance warnings');
    lines.push('');
    for (const c of varianceWarnComparisons) {
      lines.push(`- \`${c.scenario}.${c.metric}\`: ${c.varianceWarnings.join('; ')}`);
    }
  }

  if (compareResult.warnings && compareResult.warnings.length > 0) {
    lines.push('');
    lines.push('## Warnings');
    lines.push('');
    for (const warning of compareResult.warnings) {
      lines.push(`- ${warning}`);
    }
  }

  lines.push('');
  return `${lines.join('\n')}\n`;
}

module.exports = {
  renderMarkdown,
};
