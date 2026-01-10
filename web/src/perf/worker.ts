import { perf } from "./perf";

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

export function installWorkerPerfHandlers(): Promise<void> {
  return new Promise((resolve) => {
    const onMessage = (event: MessageEvent) => {
      const data = event.data as WorkerConnectMessage | undefined;
      if (!data || data.type !== "aero:perf:connect") return;

      globalThis.removeEventListener("message", onMessage);

      const port = perf.applyWorkerTraceConnect(data);
      perf.applyWorkerTraceConfig({ type: "aero:perf:trace-config", ...data.traceConfig });
      port.start();

      port.addEventListener("message", (portEvent) => {
        const msg = portEvent.data as WorkerTraceConfigMessage | WorkerTraceExportRequestMessage | undefined;
        if (!msg) return;

        if (msg.type === "aero:perf:trace-config") {
          perf.applyWorkerTraceConfig(msg);
          return;
        }

        if (msg.type === "aero:perf:trace-export") {
          const events = perf.exportLocalTraceEvents();
          port.postMessage({
            type: "aero:perf:trace-export-result",
            requestId: msg.requestId,
            tid: perf.getThreadId(),
            threadName: perf.getThreadName(),
            events,
            droppedRecords: perf.getLocalDroppedRecords(),
            droppedStrings: perf.getLocalDroppedStrings(),
          });
        }
      });

      resolve();
    };

    globalThis.addEventListener("message", onMessage);
  });
}
