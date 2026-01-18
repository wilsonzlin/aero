export function tryGetErrorCode(err) {
  if (!err || typeof err !== "object") return undefined;
  try {
    const code = err.code;
    return typeof code === "string" ? code : undefined;
  } catch {
    return undefined;
  }
}
