/**
 * Recursively collect numeric leaf values into a flat map.
 *
 * This is intentionally generic: app-provided results (e.g. microbench) can
 * evolve without requiring the runner to understand every shape.
 *
 * @param {unknown} value
 * @param {{ prefix?: string, out?: Record<string, number> }} [opts]
 * @returns {Record<string, number>}
 */
export function flattenNumericMetrics(value, opts = {}) {
  const out = opts.out ?? {};
  const prefix = opts.prefix ?? '';

  if (typeof value === 'number' && Number.isFinite(value)) {
    if (prefix) out[prefix] = value;
    return out;
  }

  if (Array.isArray(value)) {
    for (let i = 0; i < value.length; i++) {
      const nextPrefix = prefix ? `${prefix}[${i}]` : `[${i}]`;
      flattenNumericMetrics(value[i], { prefix: nextPrefix, out });
    }
    return out;
  }

  if (value && typeof value === 'object') {
    for (const [k, v] of Object.entries(value)) {
      const nextPrefix = prefix ? `${prefix}.${k}` : k;
      flattenNumericMetrics(v, { prefix: nextPrefix, out });
    }
  }

  return out;
}
