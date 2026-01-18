function normalizeCap(max) {
  return typeof max === "number" && Number.isFinite(max) && max >= 0 ? max : null;
}

export function socketWritableLengthOrOverflow(socket, max) {
  const cap = normalizeCap(max);
  if (cap === null) return 1;
  try {
    const n = socket?.writableLength;
    return typeof n === "number" && Number.isFinite(n) && n >= 0 ? n : cap + 1;
  } catch {
    return cap + 1;
  }
}

export function socketWritableLengthExceedsCap(socket, max) {
  const cap = normalizeCap(max);
  if (cap === null) return true;
  try {
    const n = socket?.writableLength;
    if (typeof n !== "number" || !Number.isFinite(n) || n < 0) return true;
    return n > cap;
  } catch {
    return true;
  }
}

