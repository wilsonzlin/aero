export function tryGetProp(obj, key) {
  if (!obj || (typeof obj !== 'object' && typeof obj !== 'function')) return undefined;
  try {
    return obj[key];
  } catch {
    return undefined;
  }
}

export function tryGetStringProp(obj, key) {
  const value = tryGetProp(obj, key);
  return typeof value === 'string' ? value : undefined;
}

export function tryGetNumberProp(obj, key) {
  const value = tryGetProp(obj, key);
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}
