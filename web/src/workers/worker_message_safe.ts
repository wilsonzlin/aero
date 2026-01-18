export function isObjectLike(value: unknown): value is object | ((...args: unknown[]) => unknown) {
  if (value == null) return false;
  const t = typeof value;
  return t === "object" || t === "function";
}

export function tryGetPropBestEffort(obj: unknown, key: PropertyKey): unknown {
  if (!isObjectLike(obj)) return undefined;
  try {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (obj as any)[key];
  } catch {
    return undefined;
  }
}

export function hasOwnPropBestEffort(obj: unknown, key: PropertyKey): boolean {
  if (!isObjectLike(obj)) return false;
  try {
    // eslint-disable-next-line no-prototype-builtins
    return Object.prototype.hasOwnProperty.call(obj, key);
  } catch {
    return false;
  }
}

export function tryGetOwnPropBestEffort(obj: unknown, key: PropertyKey): unknown {
  if (!hasOwnPropBestEffort(obj, key)) return undefined;
  return tryGetPropBestEffort(obj, key);
}

export function tryGetFiniteNumberBestEffort(obj: unknown, key: PropertyKey): number | undefined {
  const value = tryGetOwnPropBestEffort(obj, key);
  if (typeof value !== "number") return undefined;
  if (!Number.isFinite(value)) return undefined;
  return value;
}

export function tryGetSafeIntegerBestEffort(obj: unknown, key: PropertyKey): number | undefined {
  const value = tryGetFiniteNumberBestEffort(obj, key);
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value)) return undefined;
  return value;
}

export function tryGetStringBestEffort(obj: unknown, key: PropertyKey): string | undefined {
  const value = tryGetOwnPropBestEffort(obj, key);
  return typeof value === "string" ? value : undefined;
}

export function tryGetStringArrayBestEffort(obj: unknown, key: PropertyKey): string[] | undefined {
  const value = tryGetOwnPropBestEffort(obj, key);
  if (!Array.isArray(value)) return undefined;
  const out: string[] = [];
  for (const el of value) {
    if (typeof el === "string") out.push(el);
  }
  return out;
}

export function isSharedArrayBufferValue(value: unknown): value is SharedArrayBuffer {
  const ctor = (globalThis as { SharedArrayBuffer?: unknown }).SharedArrayBuffer;
  if (typeof ctor !== "function") return false;
  try {
    return value instanceof (ctor as { new (...args: unknown[]): SharedArrayBuffer });
  } catch {
    return false;
  }
}

