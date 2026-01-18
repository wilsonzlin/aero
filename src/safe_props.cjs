function tryGetProp(obj, key) {
  if (obj == null || (typeof obj !== "object" && typeof obj !== "function")) return undefined;
  try {
    return obj[key];
  } catch {
    return undefined;
  }
}

function tryGetStringProp(obj, key) {
  const value = tryGetProp(obj, key);
  return typeof value === "string" ? value : undefined;
}

function tryGetNumberProp(obj, key) {
  const value = tryGetProp(obj, key);
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

module.exports = { tryGetProp, tryGetStringProp, tryGetNumberProp };

