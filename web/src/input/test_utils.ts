export function withStubbedDocument<T>(run: (doc: any) => T): T {
  const original = (globalThis as any).document;
  const doc = {
    pointerLockElement: null,
    visibilityState: "visible",
    hasFocus: () => true,
    addEventListener: () => {},
    removeEventListener: () => {},
    exitPointerLock: () => {},
  };
  (globalThis as any).document = doc;
  try {
    return run(doc);
  } finally {
    (globalThis as any).document = original;
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

