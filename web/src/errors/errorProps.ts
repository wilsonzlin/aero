export function tryGetErrorCode(err: unknown): string | undefined {
  if (!err || typeof err !== "object") return undefined;
  try {
    const code = (err as { code?: unknown }).code;
    return typeof code === "string" ? code : undefined;
  } catch {
    return undefined;
  }
}

export function tryGetErrorCause(err: unknown): unknown | undefined {
  if (!err || typeof err !== "object") return undefined;
  try {
    return (err as { cause?: unknown }).cause;
  } catch {
    return undefined;
  }
}

export function isInstanceOf(value: unknown, ctor: unknown): boolean {
  if (!value || (typeof value !== "object" && typeof value !== "function")) return false;
  if (typeof ctor !== "function") return false;
  try {
    // eslint-disable-next-line @typescript-eslint/no-unsafe-member-access
    return value instanceof (ctor as new (...args: unknown[]) => unknown);
  } catch {
    return false;
  }
}

export function isErrorInstance(value: unknown): value is Error {
  return isInstanceOf(value, Error);
}
