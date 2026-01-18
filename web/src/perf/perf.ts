import type { ChromeTraceExport, ChromeTraceEvent, TraceInstantScope } from "./trace";
import { nowEpochUs, TraceRecorder } from "./trace";
import { unrefBestEffort } from "../unrefSafe";

type WorkerClient = {
  tid: number;
  threadName: string;
  port: MessagePort;
};

type WorkerConnectMessage = {
  type: "aero:perf:connect";
  pid: number;
  tid: number;
  processName: string;
  threadName: string;
  port: MessagePort;
  traceConfig: {
    enabled: boolean;
    startTimeEpochUs: number;
    clear: boolean;
  };
};

type WorkerTraceConfigMessage = {
  type: "aero:perf:trace-config";
  enabled: boolean;
  startTimeEpochUs: number;
  clear: boolean;
};

type WorkerTraceExportRequestMessage = {
  type: "aero:perf:trace-export";
  requestId: number;
};

type WorkerTraceExportResponseMessage = {
  type: "aero:perf:trace-export-result";
  requestId: number;
  tid: number;
  threadName: string;
  events: ChromeTraceEvent[];
  droppedRecords: number;
  droppedStrings: number;
};

type WorkerExportResult = WorkerTraceExportResponseMessage & { timedOut: boolean };

function isWindowGlobal(): boolean {
  return typeof document !== "undefined";
}

function ensureAeroGlobal(): Record<string, unknown> {
  const g = globalThis as unknown as { aero?: unknown };
  // Be defensive: callers may have set `globalThis.aero` (i.e. `window.aero`) to
  // a non-object value.
  if (!g.aero || typeof g.aero !== "object") {
    (g as { aero: Record<string, unknown> }).aero = {};
  }
  return g.aero as Record<string, unknown>;
}

export class AeroPerf {
  readonly processId = 1;
  readonly processName = "Aero";

  private readonly local: TraceRecorder;
  private traceStartEpochUs = 0;
  traceEnabled = false;

  private nextTid = 2;
  private readonly workers = new Map<Worker, WorkerClient>();
  private exportRequestId = 1;

  constructor() {
    const localThreadName = isWindowGlobal() ? "Main" : "Worker";
    const localTid = isWindowGlobal() ? 1 : 0;
    this.local = new TraceRecorder({
      pid: this.processId,
      tid: localTid,
      processName: this.processName,
      threadName: localThreadName,
    });
  }

  installGlobalApi(): void {
    if (!isWindowGlobal()) return;
    const aero = ensureAeroGlobal();
    const existing = aero.perf;
    if (!existing || typeof existing !== "object") return;
    const perfApi = existing as Record<string, unknown>;

    perfApi.traceStart = this.traceStart.bind(this);
    perfApi.traceStop = this.traceStop.bind(this);
    perfApi.exportTrace = this.exportTrace.bind(this);
    Object.defineProperty(perfApi, "traceEnabled", {
      enumerable: true,
      get: () => this.traceEnabled,
    });
  }

  spanBegin(name: string): void {
    this.local.spanBegin(name);
  }

  spanEnd(name: string): void {
    this.local.spanEnd(name);
  }

  instant(name: string, scope: TraceInstantScope = "t", args?: Record<string, unknown>): void {
    this.local.instant(name, scope, args);
  }

  counter(name: string, value: number): void {
    this.local.counter(name, value);
  }

  span<T>(name: string, fn: () => T): T {
    if (!this.local.isEnabled()) return fn();
    this.local.spanBegin(name);
    try {
      return fn();
    } finally {
      this.local.spanEnd(name);
    }
  }

  async spanAsync<T>(name: string, fn: () => Promise<T>): Promise<T> {
    if (!this.local.isEnabled()) return await fn();
    this.local.spanBegin(name);
    try {
      return await fn();
    } finally {
      this.local.spanEnd(name);
    }
  }

  traceStart(): void {
    this.traceEnabled = true;
    this.traceStartEpochUs = nowEpochUs();
    this.local.start(this.traceStartEpochUs, true);
    this.instant("traceStart", "p", { startTimeEpochUs: this.traceStartEpochUs });

    for (const client of this.workers.values()) {
      client.port.postMessage({
        type: "aero:perf:trace-config",
        enabled: true,
        startTimeEpochUs: this.traceStartEpochUs,
        clear: true,
      } satisfies WorkerTraceConfigMessage);
    }
  }

  traceStop(): void {
    if (this.traceEnabled) this.instant("traceStop", "p");
    this.traceEnabled = false;
    this.local.stop();

    for (const client of this.workers.values()) {
      client.port.postMessage({
        type: "aero:perf:trace-config",
        enabled: false,
        startTimeEpochUs: this.traceStartEpochUs,
        clear: false,
      } satisfies WorkerTraceConfigMessage);
    }
  }

  registerWorker(worker: Worker, opts?: { threadName?: string }): number {
    if (!isWindowGlobal()) throw new Error("registerWorker() must be called on the main thread");
    if (this.workers.has(worker)) return this.workers.get(worker)!.tid;

    const tid = this.nextTid++;
    const threadName = opts?.threadName ?? `Worker ${tid}`;
    const channel = new MessageChannel();
    const port = channel.port1;
    port.start();

    this.workers.set(worker, { tid, threadName, port });

    port.addEventListener("message", (event) => {
      const data = event.data as WorkerTraceExportResponseMessage | undefined;
      if (!data || data.type !== "aero:perf:trace-export-result") return;
      this.pendingExportResponses.get(data.requestId)?.({ ...data, timedOut: false });
    });

    worker.postMessage(
      {
        type: "aero:perf:connect",
        pid: this.processId,
        tid,
        processName: this.processName,
        threadName,
        port: channel.port2,
        traceConfig: {
          enabled: this.traceEnabled,
          startTimeEpochUs: this.traceStartEpochUs,
          clear: this.traceEnabled,
        },
      } satisfies WorkerConnectMessage,
      [channel.port2],
    );

    return tid;
  }

  private readonly pendingExportResponses = new Map<
    number,
    (value: WorkerExportResult) => void
  >();

  private requestWorkerExport(client: WorkerClient, timeoutMs = 2000): Promise<WorkerExportResult> {
    const requestId = this.exportRequestId++;
    return new Promise((resolve) => {
      let settled = false;
      const timeoutId = setTimeout(() => {
        if (settled) return;
        settled = true;
        this.pendingExportResponses.delete(requestId);
        resolve({
          type: "aero:perf:trace-export-result",
          requestId,
          tid: client.tid,
          threadName: client.threadName,
          events: [],
          droppedRecords: 0,
          droppedStrings: 0,
          timedOut: true,
        });
      }, timeoutMs);
      unrefBestEffort(timeoutId);

      this.pendingExportResponses.set(requestId, (value) => {
        if (settled) return;
        settled = true;
        clearTimeout(timeoutId);
        this.pendingExportResponses.delete(requestId);
        resolve(value);
      });

      client.port.postMessage({
        type: "aero:perf:trace-export",
        requestId,
      } satisfies WorkerTraceExportRequestMessage);
    });
  }

  private buildMetadataEvents(): ChromeTraceEvent[] {
    const pid = this.processId;
    const events: ChromeTraceEvent[] = [];

    events.push({
      name: "process_name",
      ph: "M",
      ts: 0,
      pid,
      tid: 0,
      args: { name: this.processName },
    });

    events.push({
      name: "aero_trace_start_time",
      ph: "M",
      ts: 0,
      pid,
      tid: 0,
      args: { startTimeEpochUs: this.traceStartEpochUs },
    });

    events.push({
      name: "thread_name",
      ph: "M",
      ts: 0,
      pid,
      tid: this.local.tid,
      args: { name: this.local.threadName },
    });

    for (const client of this.workers.values()) {
      events.push({
        name: "thread_name",
        ph: "M",
        ts: 0,
        pid,
        tid: client.tid,
        args: { name: client.threadName },
      });
    }

    return events;
  }

  async exportTrace(opts?: { asString?: boolean }): Promise<ChromeTraceExport | string> {
    const workerExports = await Promise.all(
      Array.from(this.workers.values(), (client) => this.requestWorkerExport(client)),
    );

    const metadata = this.buildMetadataEvents();

    const events: ChromeTraceEvent[] = [];
    events.push(...this.local.exportEvents());

    for (const workerExport of workerExports) {
      events.push(...workerExport.events);
    }

    events.sort((a, b) => a.ts - b.ts || a.tid - b.tid);

    const workerExportTimeouts = workerExports.filter((exp) => exp.timedOut).map((exp) => exp.threadName);

    const droppedRecordsByThread: Record<string, number> = {
      [this.local.threadName]: this.local.getDroppedRecords(),
    };
    for (const workerExport of workerExports) {
      droppedRecordsByThread[workerExport.threadName] = workerExport.droppedRecords;
    }

    const droppedStringsByThread: Record<string, number> = {
      [this.local.threadName]: this.local.getDroppedStrings(),
    };
    for (const workerExport of workerExports) {
      droppedStringsByThread[workerExport.threadName] = workerExport.droppedStrings;
    }

    const trace: ChromeTraceExport = {
      traceEvents: [...metadata, ...events],
      otherData: {
        aero: {
          traceEnabled: this.traceEnabled,
          startTimeEpochUs: this.traceStartEpochUs,
          droppedRecordsByThread,
          droppedStringsByThread,
          workerExportTimeouts,
        },
      },
    };

    if (opts?.asString) return JSON.stringify(trace);
    return trace;
  }

  exportLocalTraceEvents(): ChromeTraceEvent[] {
    return this.local.exportEvents();
  }

  getThreadId(): number {
    return this.local.tid;
  }

  getThreadName(): string {
    return this.local.threadName;
  }

  getLocalDroppedRecords(): number {
    return this.local.getDroppedRecords();
  }

  getLocalDroppedStrings(): number {
    return this.local.getDroppedStrings();
  }

  applyWorkerTraceConnect(msg: WorkerConnectMessage): MessagePort {
    this.local.setIdentity({
      pid: msg.pid,
      tid: msg.tid,
      processName: msg.processName,
      threadName: msg.threadName,
    });
    return msg.port;
  }

  applyWorkerTraceConfig(msg: WorkerTraceConfigMessage): void {
    if (msg.enabled) {
      this.traceEnabled = true;
      this.traceStartEpochUs = msg.startTimeEpochUs;
      this.local.start(msg.startTimeEpochUs, msg.clear);
      this.instant("traceStart", "p", { startTimeEpochUs: msg.startTimeEpochUs });
      return;
    }
    if (this.traceEnabled) this.instant("traceStop", "p");
    this.traceEnabled = false;
    this.traceStartEpochUs = msg.startTimeEpochUs;
    this.local.stop();
  }
}

export const perf = new AeroPerf();
