export type UnknownMethod = (this: unknown, ...args: unknown[]) => unknown;

export function tryGetMethodBestEffort(obj: unknown, key: PropertyKey): UnknownMethod | null {
  if (obj == null || (typeof obj !== "object" && typeof obj !== "function")) return null;
  try {
    const value = (obj as Record<PropertyKey, unknown>)[key];
    return typeof value === "function" ? (value as UnknownMethod) : null;
  } catch {
    return null;
  }
}

export function callMethodBestEffort(obj: unknown, key: PropertyKey, ...args: unknown[]): boolean {
  const fn = tryGetMethodBestEffort(obj, key);
  if (!fn) return true;
  try {
    fn.apply(obj, args);
    return true;
  } catch {
    return false;
  }
}

export function destroyBestEffort(obj: unknown): void {
  callMethodBestEffort(obj, "destroy");
}

