export function withStubbedDocument<T>(run: (doc: any) => T): T {
  const original = (globalThis as any).document;
  const doc = {
    pointerLockElement: null,
    visibilityState: "visible",
    hasFocus: () => true,
    activeElement: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    exitPointerLock: () => {},
  };
  (globalThis as any).document = doc;
  const restore = () => {
    (globalThis as any).document = original;
  };
  try {
    const result = run(doc);
    const then = (result as any)?.then as unknown;
    if (typeof then === "function") {
      return Promise.resolve(result).finally(restore) as unknown as T;
    }
    restore();
    return result;
  } catch (err) {
    restore();
    throw err;
  }
}

export function decodePackedBytes(packed: number, len: number): number[] {
  const out: number[] = [];
  const p = packed >>> 0;
  for (let i = 0; i < len; i++) {
    out.push((p >>> (i * 8)) & 0xff);
  }
  return out;
}
