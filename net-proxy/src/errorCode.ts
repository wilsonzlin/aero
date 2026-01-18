export function tryGetErrorCode(err: unknown): string | undefined {
  if (!err || typeof err !== "object") return undefined;
  try {
    const code = (err as { code?: unknown }).code;
    return typeof code === "string" ? code : undefined;
  } catch {
    return undefined;
  }
}
