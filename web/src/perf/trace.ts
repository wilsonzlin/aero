export type ChromeTracePhase = "B" | "E" | "C" | "i" | "M";

export type ChromeTraceEvent = {
  name: string;
  cat?: string;
  ph: ChromeTracePhase;
  ts: number;
  pid: number;
  tid: number;
  s?: "t" | "p" | "g";
  args?: Record<string, unknown>;
};

export type ChromeTraceExport = {
  traceEvents: ChromeTraceEvent[];
  otherData?: Record<string, unknown>;
};

type PerformanceLike = {
  now(): number;
  timeOrigin?: number;
};

const RECORD_TYPE_SPAN_BEGIN = 1;
const RECORD_TYPE_SPAN_END = 2;
const RECORD_TYPE_INSTANT = 3;
const RECORD_TYPE_COUNTER = 4;

const INSTANT_SCOPE_THREAD = "t".charCodeAt(0);
const INSTANT_SCOPE_PROCESS = "p".charCodeAt(0);
const INSTANT_SCOPE_GLOBAL = "g".charCodeAt(0);

export type TraceInstantScope = "t" | "p" | "g";

export type TraceRecorderOptions = {
  pid: number;
  tid: number;
  processName: string;
  threadName: string;
  maxRecords?: number;
  maxStrings?: number;
};

const DEFAULT_MAX_RECORDS = 1 << 15;
const DEFAULT_MAX_STRINGS = 1 << 11;

function resolvePerformance(): PerformanceLike | undefined {
  const perf = (globalThis as any).performance as PerformanceLike | undefined;
  if (!perf || typeof perf.now !== "function") return undefined;
  return perf;
}

export function nowEpochUs(): number {
  const perf = resolvePerformance();
  if (!perf) return Date.now() * 1000;
  const originMs = typeof perf.timeOrigin === "number" ? perf.timeOrigin : Date.now();
  return (originMs + perf.now()) * 1000;
}

class TraceStringTable {
  readonly maxStrings: number;
  readonly strings: string[] = ["<unknown>"];
  private readonly ids = new Map<string, number>([["<unknown>", 0]]);
  dropped = 0;

  constructor(maxStrings: number) {
    this.maxStrings = Math.max(1, maxStrings);
  }

  clear(): void {
    this.strings.length = 1;
    this.ids.clear();
    this.ids.set("<unknown>", 0);
    this.dropped = 0;
  }

  intern(value: string): number {
    const existing = this.ids.get(value);
    if (existing !== undefined) return existing;
    if (this.strings.length >= this.maxStrings) {
      this.dropped++;
      return 0;
    }
    const id = this.strings.length;
    this.strings.push(value);
    this.ids.set(value, id);
    return id;
  }

  resolve(id: number): string {
    return this.strings[id] ?? "<unknown>";
  }
}

class TraceRingBuffer {
  readonly maxRecords: number;
  private readonly types: Uint8Array;
  private readonly ts: Float64Array;
  private readonly nameIds: Uint32Array;
  private readonly values: Float64Array;
  private readonly scopes: Uint8Array;
  private readonly args: Array<Record<string, unknown> | undefined>;

  private writeIndex = 0;
  private recordCount = 0;
  droppedRecords = 0;

  constructor(maxRecords: number) {
    this.maxRecords = Math.max(1, maxRecords);
    this.types = new Uint8Array(this.maxRecords);
    this.ts = new Float64Array(this.maxRecords);
    this.nameIds = new Uint32Array(this.maxRecords);
    this.values = new Float64Array(this.maxRecords);
    this.scopes = new Uint8Array(this.maxRecords);
    this.args = new Array(this.maxRecords);
  }

  clear(): void {
    this.writeIndex = 0;
    this.recordCount = 0;
    this.droppedRecords = 0;
    this.args.fill(undefined);
  }

  push(
    type: number,
    tsUs: number,
    nameId: number,
    value = 0,
    scope = 0,
    args?: Record<string, unknown>,
  ): void {
    const index = this.writeIndex;
    this.types[index] = type;
    this.ts[index] = tsUs;
    this.nameIds[index] = nameId;
    this.values[index] = value;
    this.scopes[index] = scope;
    this.args[index] = args;

    this.writeIndex = (index + 1) % this.maxRecords;
    if (this.recordCount === this.maxRecords) {
      this.droppedRecords++;
      return;
    }
    this.recordCount++;
  }

  snapshot(): {
    types: Uint8Array;
    ts: Float64Array;
    nameIds: Uint32Array;
    values: Float64Array;
    scopes: Uint8Array;
    args: Array<Record<string, unknown> | undefined>;
    count: number;
    writeIndex: number;
  } {
    return {
      types: this.types,
      ts: this.ts,
      nameIds: this.nameIds,
      values: this.values,
      scopes: this.scopes,
      args: this.args,
      count: this.recordCount,
      writeIndex: this.writeIndex,
    };
  }
}

function scopeChar(scope: TraceInstantScope): number {
  switch (scope) {
    case "t":
      return INSTANT_SCOPE_THREAD;
    case "p":
      return INSTANT_SCOPE_PROCESS;
    case "g":
      return INSTANT_SCOPE_GLOBAL;
  }
}

export class TraceRecorder {
  pid: number;
  tid: number;
  processName: string;
  threadName: string;

  private enabled = false;
  private sessionStartEpochUs = 0;

  private readonly perf = resolvePerformance();
  private readonly timeOriginUs: number;

  private readonly ring: TraceRingBuffer;
  private readonly strings: TraceStringTable;

  constructor(options: TraceRecorderOptions) {
    this.pid = options.pid;
    this.tid = options.tid;
    this.processName = options.processName;
    this.threadName = options.threadName;

    const maxRecords = options.maxRecords ?? DEFAULT_MAX_RECORDS;
    const maxStrings = options.maxStrings ?? DEFAULT_MAX_STRINGS;
    this.ring = new TraceRingBuffer(maxRecords);
    this.strings = new TraceStringTable(maxStrings);

    if (this.perf) {
      const originMs = typeof this.perf.timeOrigin === "number" ? this.perf.timeOrigin : Date.now();
      this.timeOriginUs = originMs * 1000;
    } else {
      this.timeOriginUs = 0;
    }
  }

  isEnabled(): boolean {
    return this.enabled;
  }

  getSessionStartEpochUs(): number {
    return this.sessionStartEpochUs;
  }

  setIdentity(identity: Partial<Pick<TraceRecorderOptions, "pid" | "tid" | "processName" | "threadName">>): void {
    if (identity.pid !== undefined) this.pid = identity.pid;
    if (identity.tid !== undefined) this.tid = identity.tid;
    if (identity.processName !== undefined) this.processName = identity.processName;
    if (identity.threadName !== undefined) this.threadName = identity.threadName;
  }

  start(sessionStartEpochUs: number, clear = true): void {
    this.enabled = true;
    this.sessionStartEpochUs = sessionStartEpochUs;
    if (clear) {
      this.ring.clear();
      this.strings.clear();
    }
  }

  stop(): void {
    this.enabled = false;
  }

  clear(): void {
    this.ring.clear();
    this.strings.clear();
  }

  private nowRelativeUs(): number {
    if (!this.sessionStartEpochUs) return 0;
    if (!this.perf) return Date.now() * 1000 - this.sessionStartEpochUs;
    return this.timeOriginUs + this.perf.now() * 1000 - this.sessionStartEpochUs;
  }

  spanBegin(name: string): void {
    if (!this.enabled) return;
    const nameId = this.strings.intern(name);
    this.ring.push(RECORD_TYPE_SPAN_BEGIN, this.nowRelativeUs(), nameId);
  }

  spanEnd(name: string): void {
    if (!this.enabled) return;
    const nameId = this.strings.intern(name);
    this.ring.push(RECORD_TYPE_SPAN_END, this.nowRelativeUs(), nameId);
  }

  instant(name: string, scope: TraceInstantScope = "t", args?: Record<string, unknown>): void {
    if (!this.enabled) return;
    const nameId = this.strings.intern(name);
    this.ring.push(RECORD_TYPE_INSTANT, this.nowRelativeUs(), nameId, 0, scopeChar(scope), args);
  }

  counter(counterName: string, value: number): void {
    if (!this.enabled) return;
    const nameId = this.strings.intern(counterName);
    this.ring.push(RECORD_TYPE_COUNTER, this.nowRelativeUs(), nameId, value);
  }

  getDroppedRecords(): number {
    return this.ring.droppedRecords;
  }

  getDroppedStrings(): number {
    return this.strings.dropped;
  }

  exportEvents(cat = "aero"): ChromeTraceEvent[] {
    const snapshot = this.ring.snapshot();
    const start = snapshot.count === this.ring.maxRecords ? snapshot.writeIndex : 0;
    const events: ChromeTraceEvent[] = [];

    for (let i = 0; i < snapshot.count; i++) {
      const idx = (start + i) % this.ring.maxRecords;
      const type = snapshot.types[idx];
      const ts = snapshot.ts[idx];
      const name = this.strings.resolve(snapshot.nameIds[idx]);

      switch (type) {
        case RECORD_TYPE_SPAN_BEGIN:
          events.push({ name, cat, ph: "B", ts, pid: this.pid, tid: this.tid });
          break;
        case RECORD_TYPE_SPAN_END:
          events.push({ name, cat, ph: "E", ts, pid: this.pid, tid: this.tid });
          break;
        case RECORD_TYPE_INSTANT: {
          const scopeByte = snapshot.scopes[idx];
          const scope = scopeByte ? (String.fromCharCode(scopeByte) as TraceInstantScope) : "t";
          const args = snapshot.args[idx];
          const event: ChromeTraceEvent = {
            name,
            cat,
            ph: "i",
            s: scope,
            ts,
            pid: this.pid,
            tid: this.tid,
          };
          if (args && Object.keys(args).length > 0) event.args = args;
          events.push(event);
          break;
        }
        case RECORD_TYPE_COUNTER:
          events.push({
            name: "counters",
            cat,
            ph: "C",
            ts,
            pid: this.pid,
            tid: this.tid,
            args: { [name]: snapshot.values[idx] },
          });
          break;
        default:
          break;
      }
    }

    return events;
  }
}

