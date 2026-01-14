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

export function withStubbedWindow<T>(run: (win: any) => T): T {
  const original = (globalThis as any).window;
  const win = {
    addEventListener: () => {},
    removeEventListener: () => {},
    setInterval: () => 1,
    clearInterval: () => {},
  };
  (globalThis as any).window = win;
  const restore = () => {
    (globalThis as any).window = original;
  };
  try {
    const result = run(win);
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

export function withStubbedDom<T>(run: (ctx: { window: any; document: any }) => T): T {
  return withStubbedWindow((win) => withStubbedDocument((doc) => run({ window: win, document: doc })));
}

export function makeCanvasStub(overrides: any = {}): HTMLCanvasElement {
  return {
    tabIndex: 0,
    addEventListener: () => {},
    removeEventListener: () => {},
    focus: () => {},
    ...overrides,
  } as unknown as HTMLCanvasElement;
}

export function decodePackedBytes(packed: number, len: number): number[] {
  const out: number[] = [];
  const p = packed >>> 0;
  for (let i = 0; i < len; i++) {
    out.push((p >>> (i * 8)) & 0xff);
  }
  return out;
}

export type DecodedInputBatchEvent = Readonly<{
  type: number;
  timestampUs: number;
  a: number;
  b: number;
}>;

export function decodeInputBatchEvents(buffer: ArrayBuffer): DecodedInputBatchEvent[] {
  const words = new Int32Array(buffer);
  const count = words[0] >>> 0;
  const base = 2;
  const out: DecodedInputBatchEvent[] = [];
  for (let i = 0; i < count; i++) {
    const o = base + i * 4;
    out.push({
      type: words[o]! >>> 0,
      timestampUs: words[o + 1]! >>> 0,
      a: words[o + 2]! | 0,
      b: words[o + 3]! | 0,
    });
  }
  return out;
}
