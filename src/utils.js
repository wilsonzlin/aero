export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function yieldToEventLoop() {
  return new Promise((resolve) => {
    if (typeof setImmediate === 'function') {
      setImmediate(resolve);
      return;
    }
    setTimeout(resolve, 0);
  });
}

export function nowMs() {
  if (globalThis.performance && typeof globalThis.performance.now === 'function') {
    return globalThis.performance.now();
  }
  return Date.now();
}

export function formatBytes(bytes) {
  const abs = Math.abs(bytes);
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let unit = 0;
  let value = abs;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const sign = bytes < 0 ? '-' : '';
  const rounded = value >= 10 ? value.toFixed(0) : value.toFixed(1);
  return `${sign}${rounded} ${units[unit]}`;
}

