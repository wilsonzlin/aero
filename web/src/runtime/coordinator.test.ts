import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import { WorkerCoordinator } from "./coordinator";
import { MessageType } from "./protocol";
import { allocateSharedMemorySegments, createSharedMemoryViews } from "./shared_layout";

class MockWorker {
  // Global postMessage trace to assert coordinator message ordering across workers.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  static globalPosted: Array<{ specifier: string | URL; message: any; transfer?: any[] }> = [];

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  readonly posted: Array<{ message: any; transfer?: any[] }> = [];
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: ErrorEvent) => void) | null = null;
  onmessageerror: ((ev: MessageEvent) => void) | null = null;

  constructor(
    readonly specifier: string | URL,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    readonly options?: any,
  ) {}

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  postMessage(message: any, transfer?: any[]): void {
    this.posted.push({ message, transfer });
    MockWorker.globalPosted.push({ specifier: this.specifier, message, transfer });
  }

  terminate(): void {}
}

describe("runtime/coordinator", () => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const originalWorker = (globalThis as any).Worker as unknown;

  beforeEach(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).Worker = MockWorker;
    MockWorker.globalPosted.length = 0;
    vi.spyOn(perf, "registerWorker").mockImplementation(() => 0);
  });

  afterEach(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).Worker = originalWorker as any;
    vi.restoreAllMocks();
  });

  it("can spawn the net worker role without throwing", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);

    // Wire the shared memory view manually so we can call the private spawnWorker
    // helper without running the full coordinator lifecycle.
    (coordinator as any).shared = shared;

    expect(() => (coordinator as any).spawnWorker("net", segments)).not.toThrow();
    expect((coordinator as any).workers.net).toBeTruthy();
  });

  it("treats net as restartable without requiring a full VM restart", () => {
    const coordinator = new WorkerCoordinator();
    // With `net` marked restartable, this should not call `restart()` (which
    // requires an active config) and should be a no-op when the coordinator
    // isn't running.
    expect(() => coordinator.restartWorker("net")).not.toThrow();
  });

  it("rejects VM start when activeDiskImage is set but OPFS SyncAccessHandle is unavailable", () => {
    const coordinator = new WorkerCoordinator();

    expect(() =>
      coordinator.start(
        {
          guestMemoryMiB: 1,
          enableWorkers: true,
          enableWebGPU: false,
          proxyUrl: null,
          activeDiskImage: "disk.img",
          logLevel: "info",
        },
        {
          platformFeatures: {
            crossOriginIsolated: true,
            sharedArrayBuffer: true,
            wasmSimd: true,
            wasmThreads: true,
            webgpu: true,
            webusb: false,
            webhid: false,
            webgl2: true,
            opfs: true,
            opfsSyncAccessHandle: false,
            audioWorklet: true,
            offscreenCanvas: true,
            jit_dynamic_wasm: true,
          },
        },
      ),
    ).toThrow(/SyncAccessHandle/i);
  });

  it("stops and emits a fatal event when switching into VM mode via updateConfig without OPFS SyncAccessHandle", () => {
    const coordinator = new WorkerCoordinator();

    const baseConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info" as const,
    };
    const platformFeatures = {
      crossOriginIsolated: true,
      sharedArrayBuffer: true,
      wasmSimd: true,
      wasmThreads: true,
      webgpu: true,
      webusb: false,
      webhid: false,
      webgl2: true,
      opfs: true,
      opfsSyncAccessHandle: false,
      audioWorklet: true,
      offscreenCanvas: true,
      jit_dynamic_wasm: true,
    };

    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    // Manually wire up a running coordinator without invoking `start()` so this
    // unit test stays lightweight (no WASM precompile attempts).
    (coordinator as any).shared = shared;
    (coordinator as any).platformFeatures = platformFeatures;
    (coordinator as any).activeConfig = baseConfig;
    (coordinator as any).vmState = "running";
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

    coordinator.updateConfig({ ...baseConfig, activeDiskImage: "disk.img" });

    expect(coordinator.getVmState()).toBe("failed");
    expect(coordinator.getLastFatalEvent()?.message).toMatch(/SyncAccessHandle/i);

    const statuses = coordinator.getWorkerStatuses();
    expect(statuses.cpu.state).toBe("stopped");
    expect(statuses.io.state).toBe("stopped");
  });

  it("forwards audio/mic rings to CPU only in demo mode by default (SPSC)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as any).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(1024);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    const cpuAudio = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioAudio = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuAudio?.type).toBe("setAudioRingBuffer");
    expect(cpuAudio?.ringBuffer).toBe(audioSab);
    expect(ioAudio?.type).toBe("setAudioRingBuffer");
    expect(ioAudio?.ringBuffer).toBe(null);

    const micSab = new SharedArrayBuffer(256);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    const cpuMic = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioMic = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuMic?.type).toBe("setMicrophoneRingBuffer");
    expect(cpuMic?.ringBuffer).toBe(micSab);
    expect(ioMic?.type).toBe("setMicrophoneRingBuffer");
    expect(ioMic?.ringBuffer).toBe(null);
  });

  it("forwards audio/mic rings to IO only in VM mode by default (SPSC)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: "disk.img",
      logLevel: "info",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as any).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(1024);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    const cpuAudio = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioAudio = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuAudio?.type).toBe("setAudioRingBuffer");
    expect(cpuAudio?.ringBuffer).toBe(null);
    expect(ioAudio?.type).toBe("setAudioRingBuffer");
    expect(ioAudio?.ringBuffer).toBe(audioSab);

    const micSab = new SharedArrayBuffer(256);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    const cpuMic = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioMic = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuMic?.type).toBe("setMicrophoneRingBuffer");
    expect(cpuMic?.ringBuffer).toBe(null);
    expect(ioMic?.type).toBe("setMicrophoneRingBuffer");
    expect(ioMic?.ringBuffer).toBe(micSab);
  });

  it("can re-route audio ring ownership via setAudioRingBufferOwner", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as any).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(1024);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    // Default demo-mode owner is CPU.
    expect((cpuWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(audioSab);
    expect((ioWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(null);

    coordinator.setAudioRingBufferOwner("io");

    // Now the CPU must be detached and the IO worker must receive the SAB.
    expect((cpuWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(null);
    expect((ioWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(audioSab);
  });

  it("sends net.trace.enable to the net worker when enabling net tracing", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const netWorker = (coordinator as any).workers.net.worker as MockWorker;
    coordinator.setNetTraceEnabled(true);
    expect(coordinator.isNetTraceEnabled()).toBe(true);

    expect(netWorker.posted).toContainEqual({ message: { kind: "net.trace.enable" }, transfer: undefined });

    // When the net worker restarts, the coordinator re-applies the stored state once the
    // replacement worker publishes READY.
    netWorker.posted.length = 0;
    netWorker.onmessage?.({ data: { type: MessageType.READY, role: "net" } } as MessageEvent);
    expect(netWorker.posted).toContainEqual({ message: { kind: "net.trace.enable" }, transfer: undefined });
  });

  it("roundtrips net.trace.take_pcapng request/response through the coordinator", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const netInfo = (coordinator as any).workers.net as { instanceId: number; worker: MockWorker };
    const netWorker = netInfo.worker;

    const promise = coordinator.takeNetTracePcapng();

    const lastPosted = netWorker.posted.at(-1)?.message as { kind?: unknown; requestId?: unknown } | undefined;
    expect(lastPosted?.kind).toBe("net.trace.take_pcapng");
    expect(typeof lastPosted?.requestId).toBe("number");
    const requestId = lastPosted!.requestId as number;

    const expectedBytes = new Uint8Array([0x61, 0x65, 0x72, 0x6f]); // "aero"
    (coordinator as any).onWorkerMessage("net", netInfo.instanceId, {
      kind: "net.trace.pcapng",
      requestId,
      bytes: expectedBytes.buffer,
    });

    const actualBytes = await promise;
    expect(actualBytes).toBeInstanceOf(Uint8Array);
    expect(Array.from(actualBytes)).toEqual(Array.from(expectedBytes));
  });

  it("roundtrips net.trace.export_pcapng request/response through the coordinator", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const netInfo = (coordinator as any).workers.net as { instanceId: number; worker: MockWorker };
    const netWorker = netInfo.worker;

    const promise = coordinator.exportNetTracePcapng();

    const lastPosted = netWorker.posted.at(-1)?.message as { kind?: unknown; requestId?: unknown } | undefined;
    expect(lastPosted?.kind).toBe("net.trace.export_pcapng");
    expect(typeof lastPosted?.requestId).toBe("number");
    const requestId = lastPosted!.requestId as number;

    const expectedBytes = new Uint8Array([0x61, 0x65, 0x72, 0x6f]); // "aero"
    (coordinator as any).onWorkerMessage("net", netInfo.instanceId, {
      kind: "net.trace.pcapng",
      requestId,
      bytes: expectedBytes.buffer,
    });

    const actualBytes = await promise;
    expect(actualBytes).toBeInstanceOf(Uint8Array);
    expect(Array.from(actualBytes)).toEqual(Array.from(expectedBytes));
  });

  it("roundtrips net.trace.status request/response through the coordinator", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const netInfo = (coordinator as any).workers.net as { instanceId: number; worker: MockWorker };
    const netWorker = netInfo.worker;

    const promise = coordinator.getNetTraceStats();

    const lastPosted = netWorker.posted.at(-1)?.message as { kind?: unknown; requestId?: unknown } | undefined;
    expect(lastPosted?.kind).toBe("net.trace.status");
    expect(typeof lastPosted?.requestId).toBe("number");
    const requestId = lastPosted!.requestId as number;

    (coordinator as any).onWorkerMessage("net", netInfo.instanceId, {
      kind: "net.trace.status",
      requestId,
      enabled: true,
      records: 123,
      bytes: 4567,
      droppedRecords: 3,
      droppedBytes: 9,
    });

    const stats = await promise;
    expect(stats.kind).toBe("net.trace.status");
    expect(stats.requestId).toBe(requestId);
    expect(stats.enabled).toBe(true);
    expect(stats.records).toBe(123);
    expect(stats.bytes).toBe(4567);
    expect(stats.droppedRecords).toBe(3);
    expect(stats.droppedBytes).toBe(9);
  });

  it("rejects pending net trace requests when the net worker is terminated", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const promise = coordinator.takeNetTracePcapng(60_000);
    (coordinator as any).terminateWorker("net");

    await expect(promise).rejects.toThrow(/net worker restarted/i);
  });

  it("rejects pending net trace stats requests when the net worker is terminated", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const promise = coordinator.getNetTraceStats(60_000);
    (coordinator as any).terminateWorker("net");

    await expect(promise).rejects.toThrow(/net worker restarted/i);
  });

  it("enforces SPSC ownership when switching audio/mic ring buffer attachments between workers", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

    expect(() => coordinator.setAudioRingBufferOwner("both")).toThrow(/violates SPSC constraints/i);
    expect(() => coordinator.setMicrophoneRingBufferOwner("both")).toThrow(/violates SPSC constraints/i);

    const audioSab = new SharedArrayBuffer(16);
    coordinator.setAudioRingBufferOwner("io");
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    MockWorker.globalPosted.length = 0;
    coordinator.setAudioRingBufferOwner("cpu");

    const detachIoAudioIdx = MockWorker.globalPosted.findIndex(
      (entry) =>
        String(entry.specifier).includes("io.worker.ts") &&
        entry.message?.type === "setAudioRingBuffer" &&
        entry.message?.ringBuffer === null,
    );
    const attachCpuAudioIdx = MockWorker.globalPosted.findIndex(
      (entry) =>
        String(entry.specifier).includes("cpu.worker.ts") &&
        entry.message?.type === "setAudioRingBuffer" &&
        entry.message?.ringBuffer === audioSab,
    );
    expect(detachIoAudioIdx).toBeGreaterThanOrEqual(0);
    expect(attachCpuAudioIdx).toBeGreaterThanOrEqual(0);
    expect(detachIoAudioIdx).toBeLessThan(attachCpuAudioIdx);
    expect(
      MockWorker.globalPosted.some(
        (entry) =>
          String(entry.specifier).includes("io.worker.ts") &&
          entry.message?.type === "setAudioRingBuffer" &&
          entry.message?.ringBuffer === audioSab,
      ),
    ).toBe(false);

    const micSab = new SharedArrayBuffer(16);
    coordinator.setMicrophoneRingBufferOwner("io");
    coordinator.setMicrophoneRingBuffer(micSab, 44_100);

    MockWorker.globalPosted.length = 0;
    coordinator.setMicrophoneRingBufferOwner("cpu");

    const detachIoMicIdx = MockWorker.globalPosted.findIndex(
      (entry) =>
        String(entry.specifier).includes("io.worker.ts") &&
        entry.message?.type === "setMicrophoneRingBuffer" &&
        entry.message?.ringBuffer === null,
    );
    const attachCpuMicIdx = MockWorker.globalPosted.findIndex(
      (entry) =>
        String(entry.specifier).includes("cpu.worker.ts") &&
        entry.message?.type === "setMicrophoneRingBuffer" &&
        entry.message?.ringBuffer === micSab,
    );
    expect(detachIoMicIdx).toBeGreaterThanOrEqual(0);
    expect(attachCpuMicIdx).toBeGreaterThanOrEqual(0);
    expect(detachIoMicIdx).toBeLessThan(attachCpuMicIdx);
    expect(
      MockWorker.globalPosted.some(
        (entry) =>
          String(entry.specifier).includes("io.worker.ts") &&
          entry.message?.type === "setMicrophoneRingBuffer" &&
          entry.message?.ringBuffer === micSab,
      ),
    ).toBe(false);
  });
});
