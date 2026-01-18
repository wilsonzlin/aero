export function isInstanceOfSafe(value: unknown, ctor: unknown): boolean {
  if (!value || (typeof value !== 'object' && typeof value !== 'function')) return false;
  if (typeof ctor !== 'function') return false;
  try {
    return value instanceof (ctor as new (...args: unknown[]) => unknown);
  } catch {
    return false;
  }
}

export function tryGetErrorName(err: unknown): unknown {
  if (!err || typeof err !== 'object') return undefined;
  try {
    return (err as { name?: unknown }).name;
  } catch {
    return undefined;
  }
}
