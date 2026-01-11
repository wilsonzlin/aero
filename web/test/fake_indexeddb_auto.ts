export {};

type Key = unknown;

type KeyPath = string | string[];

type IndexMeta = { name: string; keyPath: KeyPath; unique: boolean };

type StoreMeta = {
  name: string;
  keyPath: KeyPath | null;
  records: Map<string, { key: Key; value: unknown }>;
  indexes: Map<string, IndexMeta>;
};

type DatabaseMeta = {
  version: number;
  stores: Map<string, StoreMeta>;
};

const databases = new Map<string, DatabaseMeta>();

function keyTypeRank(key: Key): number {
  if (key === null) return 9;
  if (Array.isArray(key)) return 3;
  switch (typeof key) {
    case "number":
      return 1;
    case "string":
      return 2;
    case "boolean":
      return 4;
    default:
      return 8;
  }
}

function encodeKey(key: Key): string {
  if (key === null) return "null";
  if (Array.isArray(key)) return `a:[${key.map(encodeKey).join(",")}]`;
  switch (typeof key) {
    case "number":
      return `n:${Object.is(key, -0) ? "-0" : String(key)}`;
    case "string":
      return `s:${key}`;
    case "boolean":
      return `b:${key ? "1" : "0"}`;
    default:
      return `u:${String(key)}`;
  }
}

function compareKey(a: Key, b: Key): number {
  if (a === b) return 0;
  const ra = keyTypeRank(a);
  const rb = keyTypeRank(b);
  if (ra !== rb) return ra - rb;

  if (typeof a === "number" && typeof b === "number") return a - b;
  if (typeof a === "string" && typeof b === "string") return a < b ? -1 : 1;
  if (typeof a === "boolean" && typeof b === "boolean") return (a ? 1 : 0) - (b ? 1 : 0);

  if (Array.isArray(a) && Array.isArray(b)) {
    const len = Math.min(a.length, b.length);
    for (let i = 0; i < len; i += 1) {
      const c = compareKey(a[i], b[i]);
      if (c !== 0) return c;
    }
    return a.length - b.length;
  }

  // Fallback: string compare.
  const sa = String(a);
  const sb = String(b);
  return sa < sb ? -1 : sa > sb ? 1 : 0;
}

function extractKey(value: unknown, keyPath: KeyPath): Key {
  if (!value || typeof value !== "object") return undefined;
  const obj = value as Record<string, unknown>;
  if (Array.isArray(keyPath)) {
    return keyPath.map((k) => obj[k]);
  }
  return obj[keyPath];
}

class FakeIDBKeyRange {
  readonly lower: Key;
  readonly upper: Key;
  readonly lowerOpen: boolean;
  readonly upperOpen: boolean;

  private constructor(lower: Key, upper: Key, lowerOpen: boolean, upperOpen: boolean) {
    this.lower = lower;
    this.upper = upper;
    this.lowerOpen = lowerOpen;
    this.upperOpen = upperOpen;
  }

  static only(value: Key): FakeIDBKeyRange {
    return new FakeIDBKeyRange(value, value, false, false);
  }

  static bound(lower: Key, upper: Key, lowerOpen = false, upperOpen = false): FakeIDBKeyRange {
    return new FakeIDBKeyRange(lower, upper, lowerOpen, upperOpen);
  }

  includes(key: Key): boolean {
    const lo = compareKey(key, this.lower);
    if (lo < 0 || (lo === 0 && this.lowerOpen)) return false;
    const hi = compareKey(key, this.upper);
    if (hi > 0 || (hi === 0 && this.upperOpen)) return false;
    return true;
  }
}

type Listener = ((event?: unknown) => void) | null | undefined;

class FakeIDBRequest {
  result = undefined;
  error: Error | null = null;
  onsuccess: Listener = null;
  onerror: Listener = null;
  onupgradeneeded: Listener = null;
  onblocked: Listener = null;
  transaction: FakeIDBTransaction | null = null;
}

class FakeDomStringList {
  private readonly stores: Map<string, StoreMeta>;
  constructor(stores: Map<string, StoreMeta>) {
    this.stores = stores;
  }

  contains(name: string): boolean {
    return this.stores.has(name);
  }
}

class FakeIDBTransaction {
  readonly mode: IDBTransactionMode;
  error: Error | null = null;
  oncomplete: Listener = null;
  onabort: Listener = null;
  onerror: Listener = null;

  private pending = 0;
  private completionScheduled = false;
  private finished = false;
  private readonly stores: Map<string, StoreMeta>;
  private readonly completeListeners: Array<() => void> = [];

  constructor(stores: Map<string, StoreMeta>, mode: IDBTransactionMode) {
    this.stores = stores;
    this.mode = mode;
  }

  addCompleteListener(cb: () => void): void {
    this.completeListeners.push(cb);
  }

  objectStore(name: string): FakeIDBObjectStore {
    const store = this.stores.get(name);
    if (!store) throw new Error(`IndexedDB object store not found: ${name}`);
    return new FakeIDBObjectStore(store, this);
  }

  requestStarted(): void {
    this.pending += 1;
  }

  requestFinished(): void {
    this.pending = Math.max(0, this.pending - 1);
    this.scheduleCompletionCheck();
  }

  scheduleCompletionCheck(): void {
    if (this.finished) return;
    if (this.pending !== 0) return;
    if (this.completionScheduled) return;
    this.completionScheduled = true;
    // Real IndexedDB transactions only auto-commit once the current task unwinds,
    // after all queued microtasks (including async/await promise continuations)
    // have had a chance to enqueue additional requests. Using a macrotask here
    // prevents the transaction from completing "too early" between chained awaits.
    setTimeout(() => {
      this.completionScheduled = false;
      if (this.finished) return;
      if (this.pending !== 0) return;
      this.finished = true;
      try {
        this.oncomplete?.();
      } finally {
        for (const cb of this.completeListeners) cb();
      }
    }, 0);
  }
}

type CursorEntry = { key: Key; primaryKey: Key; value: unknown; store: StoreMeta };

class FakeIDBCursorWithValue {
  readonly key: Key;
  readonly primaryKey: Key;
  readonly value: unknown;

  private continued = false;
  private readonly advance: () => void;
  private readonly store: StoreMeta;

  constructor(entry: CursorEntry, advance: () => void) {
    this.key = entry.key;
    this.primaryKey = entry.primaryKey;
    this.value = entry.value;
    this.advance = advance;
    this.store = entry.store;
  }

  continue(): void {
    this.continued = true;
    this.advance();
  }

  wasContinued(): boolean {
    return this.continued;
  }

  delete(): void {
    this.store.records.delete(encodeKey(this.primaryKey));
  }

  update(value: unknown): void {
    const encoded = encodeKey(this.primaryKey);
    if (!this.store.records.has(encoded)) return;
    this.store.records.set(encoded, { key: this.primaryKey, value });
  }
}

function makeCursorRequest(tx: FakeIDBTransaction, entries: CursorEntry[]): FakeIDBRequest {
  const req = new FakeIDBRequest();
  tx.requestStarted();

  let pos = 0;
  let done = false;

  const step = () => {
    if (done) return;

    if (pos >= entries.length) {
      req.result = null;
      done = true;
      try {
        req.onsuccess?.();
      } finally {
        tx.requestFinished();
      }
      return;
    }

    const entry = entries[pos];
    const cursor = new FakeIDBCursorWithValue(entry, () => {
      pos += 1;
      queueMicrotask(step);
    });
    req.result = cursor;
    try {
      req.onsuccess?.();
    } finally {
      if (!cursor.wasContinued()) {
        done = true;
        tx.requestFinished();
      }
    }
  };

  queueMicrotask(step);
  return req;
}

function makeSimpleRequest(tx: FakeIDBTransaction, exec: () => unknown): FakeIDBRequest {
  const req = new FakeIDBRequest();
  tx.requestStarted();
  queueMicrotask(() => {
    try {
      req.result = exec();
      req.onsuccess?.();
    } catch (err) {
      const error = err instanceof Error ? err : new Error(String(err));
      req.error = error;
      tx.error = error;
      req.onerror?.();
      tx.onerror?.();
      tx.onabort?.();
    } finally {
      tx.requestFinished();
    }
  });
  return req;
}

class FakeIDBIndex {
  private readonly store: StoreMeta;
  private readonly tx: FakeIDBTransaction;
  private readonly meta: IndexMeta;

  constructor(store: StoreMeta, tx: FakeIDBTransaction, meta: IndexMeta) {
    this.store = store;
    this.tx = tx;
    this.meta = meta;
  }

  openCursor(range?: FakeIDBKeyRange): FakeIDBRequest {
    const entries: CursorEntry[] = [];
    for (const { key: primaryKey, value } of this.store.records.values()) {
      const idxKey = extractKey(value, this.meta.keyPath);
      if (idxKey === undefined) continue;
      if (range && !range.includes(idxKey)) continue;
      entries.push({ key: idxKey, primaryKey, value, store: this.store });
    }
    entries.sort((a, b) => {
      const c = compareKey(a.key, b.key);
      return c !== 0 ? c : compareKey(a.primaryKey, b.primaryKey);
    });
    return makeCursorRequest(this.tx, entries);
  }
}

class FakeIDBObjectStore {
  private readonly store: StoreMeta;
  private readonly tx: FakeIDBTransaction;

  constructor(store: StoreMeta, tx: FakeIDBTransaction) {
    this.store = store;
    this.tx = tx;
  }

  get(key: Key): FakeIDBRequest {
    return makeSimpleRequest(this.tx, () => {
      return this.store.records.get(encodeKey(key))?.value;
    });
  }

  getAll(): FakeIDBRequest {
    return makeSimpleRequest(this.tx, () => {
      const values: Array<{ key: Key; value: unknown }> = Array.from(this.store.records.values());
      values.sort((a, b) => compareKey(a.key, b.key));
      return values.map((v) => v.value);
    });
  }

  put(value: unknown, key?: Key): FakeIDBRequest {
    return makeSimpleRequest(this.tx, () => {
      let primaryKey = key;
      if (this.store.keyPath) {
        primaryKey = extractKey(value, this.store.keyPath);
      }
      if (primaryKey === undefined) {
        throw new Error("IndexedDB put(): missing key");
      }
      this.store.records.set(encodeKey(primaryKey), { key: primaryKey, value });
      return primaryKey;
    });
  }

  delete(key: Key): FakeIDBRequest {
    return makeSimpleRequest(this.tx, () => {
      this.store.records.delete(encodeKey(key));
      return undefined;
    });
  }

  openCursor(range?: FakeIDBKeyRange): FakeIDBRequest {
    const entries: CursorEntry[] = [];
    for (const { key, value } of this.store.records.values()) {
      if (range && !range.includes(key)) continue;
      entries.push({ key, primaryKey: key, value, store: this.store });
    }
    entries.sort((a, b) => compareKey(a.key, b.key));
    return makeCursorRequest(this.tx, entries);
  }

  createIndex(name: string, keyPath: KeyPath, opts: { unique?: boolean } = {}): void {
    this.store.indexes.set(name, { name, keyPath, unique: !!opts.unique });
  }

  index(name: string): FakeIDBIndex {
    const meta = this.store.indexes.get(name);
    if (!meta) throw new Error(`IndexedDB index not found: ${name}`);
    return new FakeIDBIndex(this.store, this.tx, meta);
  }
}

class FakeIDBDatabase {
  readonly name: string;
  version: number;
  readonly objectStoreNames: FakeDomStringList;

  private readonly meta: DatabaseMeta;
  upgradeTx: FakeIDBTransaction | null = null;

  constructor(name: string, meta: DatabaseMeta) {
    this.name = name;
    this.meta = meta;
    this.version = meta.version;
    this.objectStoreNames = new FakeDomStringList(meta.stores);
  }

  close(): void {}

  createObjectStore(name: string, opts: { keyPath?: KeyPath } = {}): FakeIDBObjectStore {
    const store: StoreMeta = {
      name,
      keyPath: opts.keyPath ?? null,
      records: new Map(),
      indexes: new Map(),
    };
    this.meta.stores.set(name, store);
    return new FakeIDBObjectStore(store, this.upgradeTx ?? new FakeIDBTransaction(new Map([[name, store]]), "versionchange"));
  }

  transaction(storeNames: string | string[], mode: IDBTransactionMode): FakeIDBTransaction {
    const names = Array.isArray(storeNames) ? storeNames : [storeNames];
    const stores = new Map<string, StoreMeta>();
    for (const name of names) {
      const store = this.meta.stores.get(name);
      if (!store) throw new Error(`IndexedDB object store not found: ${name}`);
      stores.set(name, store);
    }
    return new FakeIDBTransaction(stores, mode);
  }
}

class FakeIDBFactory {
  open(name: string, version: number): FakeIDBRequest {
    const req = new FakeIDBRequest();
    queueMicrotask(() => {
      try {
        const existing = databases.get(name);
        const oldVersion = existing?.version ?? 0;
        if (existing && version < existing.version) {
          throw new Error(`IndexedDB version error: requested ${version}, existing ${existing.version}`);
        }

        const meta: DatabaseMeta =
          existing ??
          (() => {
            const created: DatabaseMeta = { version: 0, stores: new Map() };
            databases.set(name, created);
            return created;
          })();

        const needsUpgrade = version > meta.version;
        meta.version = version;

        const db = new FakeIDBDatabase(name, meta);
        req.result = db;

        if (needsUpgrade) {
          const upgradeTx = new FakeIDBTransaction(meta.stores, "versionchange");
          req.transaction = upgradeTx;
          db.upgradeTx = upgradeTx;

          // Ensure upgrades complete after any cursor operations.
          upgradeTx.addCompleteListener(() => {
            db.upgradeTx = null;
            req.transaction = null;
            req.onsuccess?.();
          });

          req.onupgradeneeded?.({ oldVersion });
          upgradeTx.scheduleCompletionCheck();
          return;
        }

        req.onsuccess?.();
      } catch (err) {
        req.error = err instanceof Error ? err : new Error(String(err));
        req.onerror?.();
      }
    });
    return req;
  }

  deleteDatabase(name: string): FakeIDBRequest {
    const req = new FakeIDBRequest();
    queueMicrotask(() => {
      try {
        databases.delete(name);
        req.onsuccess?.();
      } catch (err) {
        req.error = err instanceof Error ? err : new Error(String(err));
        req.onerror?.();
      }
    });
    return req;
  }
}

function installFakeIndexedDb(): void {
  if (typeof (globalThis as any).indexedDB === "undefined") {
    (globalThis as any).indexedDB = new FakeIDBFactory();
  }
  if (typeof (globalThis as any).IDBKeyRange === "undefined") {
    (globalThis as any).IDBKeyRange = FakeIDBKeyRange;
  }
}

installFakeIndexedDb();
