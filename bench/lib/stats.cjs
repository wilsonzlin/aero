'use strict';

function median(samples) {
  if (!Array.isArray(samples) || samples.length === 0) {
    throw new Error('median() requires a non-empty array');
  }
  const sorted = [...samples].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 1) {
    return sorted[mid];
  }
  return (sorted[mid - 1] + sorted[mid]) / 2;
}

function mean(samples) {
  if (!Array.isArray(samples) || samples.length === 0) {
    throw new Error('mean() requires a non-empty array');
  }
  let sum = 0;
  for (const value of samples) sum += value;
  return sum / samples.length;
}

function stddev(samples) {
  if (!Array.isArray(samples) || samples.length === 0) {
    throw new Error('stddev() requires a non-empty array');
  }
  if (samples.length === 1) return 0;
  const avg = mean(samples);
  let acc = 0;
  for (const value of samples) {
    const diff = value - avg;
    acc += diff * diff;
  }
  return Math.sqrt(acc / (samples.length - 1));
}

function coefficientOfVariation(samples) {
  const avg = mean(samples);
  if (avg === 0) return null;
  return stddev(samples) / avg;
}

module.exports = {
  coefficientOfVariation,
  mean,
  median,
  stddev,
};
