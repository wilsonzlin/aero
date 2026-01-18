export function isInstanceOfSafe(value, ctor) {
  if (!value || (typeof value !== 'object' && typeof value !== 'function')) return false;
  if (typeof ctor !== 'function') return false;
  try {
    return value instanceof ctor;
  } catch {
    return false;
  }
}
