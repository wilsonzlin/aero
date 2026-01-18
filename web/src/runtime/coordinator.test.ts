import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { WorkerCoordinator } from "./coordinator";
import { MessageType } from "./protocol";
import { createSharedMemoryViews } from "./shared_layout";
import { allocateHarnessSharedMemorySegments } from "./harness_shared_memory";
import type { DiskImageMetadata } from "../storage/metadata";
import {
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_LEGACY_TEXT,
  SCANOUT_SOURCE_WDDM,
  publishScanoutState,
  snapshotScanoutState,
} from "../ipc/scanout_state";

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
  const originalWorkerDescriptor = Object.getOwnPropertyDescriptor(globalThis, "Worker");
  const globalWithWorker = globalThis as unknown as { Worker?: unknown };
  const TEST_VRAM_MIB = 1;
  const TEST_GUEST_MIB = 1;
  const allocateTestSegments = () =>
    allocateHarnessSharedMemorySegments({
      // Keep guest RAM aligned with the `guestMemoryMiB: 1` configs used throughout this test
      // suite so `WorkerCoordinator.updateConfig` doesn't spuriously request a restart due to
      // mismatched `guest_size`.
      guestRamBytes: TEST_GUEST_MIB * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: TEST_VRAM_MIB * 1024 * 1024,
    });
  const dummyHdd = (): DiskImageMetadata => ({
    source: "local",
    id: "disk.img",
    name: "disk.img",
    backend: "opfs",
    kind: "hdd",
    format: "raw",
    fileName: "disk.img",
    sizeBytes: 0,
    createdAtMs: 0,
  });
  const lastMessageOfType = (worker: MockWorker, type: string): unknown | undefined => {
    for (let i = worker.posted.length - 1; i >= 0; i -= 1) {
      const msg = worker.posted[i]?.message as { type?: unknown } | undefined;
      if (msg?.type === type) return msg;
    }
    return undefined;
  };

  const lastPostedRingBuffer = (worker: MockWorker): unknown => {
    const msg = worker.posted.at(-1)?.message as unknown;
    if (!msg || typeof msg !== "object") return undefined;
    return (msg as { ringBuffer?: unknown }).ringBuffer;
  };

  type CoordinatorTestHarness = {
    shared: unknown;
    platformFeatures?: unknown;
    activeConfig?: Record<string, unknown>;
    vmState?: string;
    workers: Record<string, { instanceId: number; worker: unknown }>;
    spawnWorker: (role: string, segments: unknown) => void;
    terminateWorker: (role: string) => void;
    onWorkerMessage: (role: string, instanceId: number, message: unknown) => void;
    scheduleFullRestart: (reason: string) => void;
    eventLoop: unknown;
    postWorkerInitMessages: unknown;
    pendingAerogpuSubmissions?: unknown[];
    aerogpuInFlightFencesByRequestId?: Map<number, bigint>;
  };

  beforeEach(() => {
    globalWithWorker.Worker = MockWorker;
    MockWorker.globalPosted.length = 0;
    vi.spyOn(perf, "registerWorker").mockImplementation(() => 0);
  });

  afterEach(() => {
    if (originalWorkerDescriptor) {
      Object.defineProperty(globalThis, "Worker", originalWorkerDescriptor);
    } else {
      Reflect.deleteProperty(globalThis, "Worker");
    }
    vi.restoreAllMocks();
  });

  it("can spawn the net worker role without throwing", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);

    // Wire the shared memory view manually so we can call the private spawnWorker
    // helper without running the full coordinator lifecycle.
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;

    expect(() => (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments)).not.toThrow();
    expect((coordinator as unknown as CoordinatorTestHarness).workers.net).toBeTruthy();
  });

  it("spawns the machine CPU worker entrypoint when vmRuntime=machine", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      vmRuntime: "machine",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(String(cpuWorker.specifier)).toMatch(/machine_cpu\.worker\.ts/);
  });

  it("spawns the legacy CPU worker entrypoint when vmRuntime=legacy", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      vmRuntime: "legacy",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(String(cpuWorker.specifier)).toMatch(/\/cpu\.worker\.ts(\?|$)/);
    expect(String(cpuWorker.specifier)).not.toMatch(/machine_cpu\.worker\.ts/);
  });

  it("spawns the machine CPU worker via start() when vmRuntime=machine", () => {
    const coordinator = new WorkerCoordinator();

    // Avoid kicking off the full worker event loops / wasm precompile in this unit test;
    // we only care about the worker entrypoint selection.
    (coordinator as unknown as CoordinatorTestHarness).eventLoop = vi.fn(async () => {});
    (coordinator as unknown as CoordinatorTestHarness).postWorkerInitMessages = vi.fn(async () => {});

    coordinator.start(
      {
        vmRuntime: "machine",
        guestMemoryMiB: 1,
        vramMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
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
    );

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(String(cpuWorker.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("spawns the legacy CPU worker via start() when vmRuntime is omitted", () => {
    const coordinator = new WorkerCoordinator();

    // Avoid kicking off the full worker event loops / wasm precompile in this unit test;
    // we only care about the worker entrypoint selection.
    (coordinator as unknown as CoordinatorTestHarness).eventLoop = vi.fn(async () => {});
    (coordinator as unknown as CoordinatorTestHarness).postWorkerInitMessages = vi.fn(async () => {});

    coordinator.start(
      {
        guestMemoryMiB: 1,
        vramMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
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
    );

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const specifier = String(cpuWorker.specifier);
    expect(specifier).toMatch(/cpu\.worker\.ts/);
    expect(specifier).not.toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("preserves the machine CPU worker entrypoint across full restarts", () => {
    vi.useFakeTimers();
    const coordinator = new WorkerCoordinator();

    // Stub out heavyweight background tasks; we only care about which worker URL is chosen.
    (coordinator as unknown as CoordinatorTestHarness).eventLoop = vi.fn(async () => {});
    (coordinator as unknown as CoordinatorTestHarness).postWorkerInitMessages = vi.fn(async () => {});

    coordinator.start(
      {
        vmRuntime: "machine",
        guestMemoryMiB: 1,
        vramMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
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
    );

    const cpuWorkerBefore = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerBefore.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    // Trigger a full restart (the path used for non-restartable worker failures like CPU/IO).
    (coordinator as unknown as CoordinatorTestHarness).scheduleFullRestart("test_full_restart");
    vi.runAllTimers();

    const cpuWorkerAfter = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(cpuWorkerAfter).not.toBe(cpuWorkerBefore);
    expect(String(cpuWorkerAfter.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
    vi.useRealTimers();
  });

  it("forwards AeroGPU submit completions from the GPU worker to the CPU worker", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      vmRuntime: "machine",
      guestMemoryMiB: TEST_GUEST_MIB,
      vramMiB: TEST_VRAM_MIB,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);
    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const cpuInstanceId = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.instanceId as number;
    const gpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.gpu.worker as MockWorker;
    const gpuInstanceId = (coordinator as unknown as CoordinatorTestHarness).workers.gpu.instanceId as number;

    // Mark GPU worker READY so submissions are forwarded and requestIds are tracked.
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("gpu", gpuInstanceId, { type: MessageType.READY, role: "gpu" });

    const cmdStream = new ArrayBuffer(16);
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInstanceId, {
      kind: "aerogpu.submit",
      contextId: 7,
      signalFence: 42n,
      cmdStream,
    });

    const submit = lastMessageOfType(gpuWorker, "submit_aerogpu") as { requestId?: unknown } | undefined;
    expect(submit).toBeTruthy();
    const requestId = submit && typeof submit.requestId === "number" ? submit.requestId : -1;
    expect(requestId).toBeGreaterThan(0);

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("gpu", gpuInstanceId, {
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "submit_complete",
      requestId,
      completedFence: 42n,
    });

    expect(
      cpuWorker.posted.some(
        (entry) => {
          const msg = entry.message as unknown;
          if (!msg || typeof msg !== "object") return false;
          const rec = msg as { kind?: unknown; fence?: unknown };
          return rec.kind === "aerogpu.complete_fence" && rec.fence === 42n;
        },
      ),
    ).toBe(true);
  });

  it("buffers AeroGPU submits while the GPU worker is not ready and flushes them on READY", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      vmRuntime: "machine",
      guestMemoryMiB: TEST_GUEST_MIB,
      vramMiB: TEST_VRAM_MIB,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);
    const cpuInstanceId = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.instanceId as number;
    const gpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.gpu.worker as MockWorker;
    const gpuInstanceId = (coordinator as unknown as CoordinatorTestHarness).workers.gpu.instanceId as number;

    const cmdStream = new ArrayBuffer(16);
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInstanceId, {
      kind: "aerogpu.submit",
      contextId: 7,
      signalFence: 123n,
      cmdStream,
    });

    expect(gpuWorker.posted.some((msg) => (msg.message as { type?: unknown }).type === "submit_aerogpu")).toBe(false);

    // Mark the GPU worker as ready; this should flush buffered submissions.
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("gpu", gpuInstanceId, { type: MessageType.READY, role: "gpu" });

    const submit = lastMessageOfType(gpuWorker, "submit_aerogpu") as { signalFence?: unknown; contextId?: unknown } | undefined;
    expect(submit).toBeTruthy();
    expect(submit?.contextId).toBe(7);
    expect(submit?.signalFence).toBe(123n);
  });

  it("preserves the machine CPU worker entrypoint across restart()", () => {
    const coordinator = new WorkerCoordinator();
    (coordinator as unknown as CoordinatorTestHarness).eventLoop = vi.fn(async () => {});
    (coordinator as unknown as CoordinatorTestHarness).postWorkerInitMessages = vi.fn(async () => {});

    coordinator.start(
      {
        vmRuntime: "machine",
        guestMemoryMiB: 1,
        vramMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
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
    );

    const cpuWorkerBefore = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerBefore.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.restart();

    const cpuWorkerAfter = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(cpuWorkerAfter).not.toBe(cpuWorkerBefore);
    expect(String(cpuWorkerAfter.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("switches the CPU worker entrypoint when vmRuntime changes via updateConfig()", () => {
    const coordinator = new WorkerCoordinator();
    (coordinator as unknown as CoordinatorTestHarness).eventLoop = vi.fn(async () => {});
    (coordinator as unknown as CoordinatorTestHarness).postWorkerInitMessages = vi.fn(async () => {});

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
    } as const;

    coordinator.start(
      {
        vmRuntime: "legacy",
        guestMemoryMiB: 1,
        vramMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
        logLevel: "info",
      },
      { platformFeatures },
    );

    const cpuWorkerLegacy = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerLegacy.specifier)).toMatch(/cpu\.worker\.ts/);
    expect(String(cpuWorkerLegacy.specifier)).not.toMatch(/machine_cpu\.worker\.ts/);

    coordinator.updateConfig({
      vmRuntime: "machine",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    });

    const cpuWorkerMachine = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(cpuWorkerMachine).not.toBe(cpuWorkerLegacy);
    expect(String(cpuWorkerMachine.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.updateConfig({
      vmRuntime: "legacy",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    });

    const cpuWorkerLegacyAgain = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(cpuWorkerLegacyAgain).not.toBe(cpuWorkerMachine);
    expect(String(cpuWorkerLegacyAgain.specifier)).toMatch(/cpu\.worker\.ts/);
    expect(String(cpuWorkerLegacyAgain.specifier)).not.toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("preserves the machine CPU worker entrypoint across VM reset (shared memory preserved)", () => {
    const coordinator = new WorkerCoordinator();

    // Avoid kicking off the full worker event loops / wasm precompile in this unit test;
    // we only care about the worker entrypoint selection.
    (coordinator as unknown as CoordinatorTestHarness).eventLoop = vi.fn(async () => {});
    (coordinator as unknown as CoordinatorTestHarness).postWorkerInitMessages = vi.fn(async () => {});

    coordinator.start(
      {
        vmRuntime: "machine",
        guestMemoryMiB: 1,
        vramMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
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
    );

    const cpuWorkerBefore = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerBefore.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.reset("test_reset");

    const cpuWorkerAfter = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(cpuWorkerAfter).not.toBe(cpuWorkerBefore);
    expect(String(cpuWorkerAfter.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("preserves the machine CPU worker entrypoint when restartWorker('cpu') falls back to restart()", () => {
    const coordinator = new WorkerCoordinator();

    // Avoid kicking off the full worker event loops / wasm precompile in this unit test;
    // we only care about the worker entrypoint selection.
    (coordinator as unknown as CoordinatorTestHarness).eventLoop = vi.fn(async () => {});
    (coordinator as unknown as CoordinatorTestHarness).postWorkerInitMessages = vi.fn(async () => {});

    coordinator.start(
      {
        vmRuntime: "machine",
        guestMemoryMiB: 1,
        vramMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
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
    );

    const cpuWorkerBefore = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerBefore.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.restartWorker("cpu");

    const cpuWorkerAfter = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    expect(cpuWorkerAfter).not.toBe(cpuWorkerBefore);
    expect(String(cpuWorkerAfter.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("treats net as restartable without requiring a full VM restart", () => {
    const coordinator = new WorkerCoordinator();
    // With `net` marked restartable, this should not call `restart()` (which
    // requires an active config) and should be a no-op when the coordinator
    // isn't running.
    expect(() => coordinator.restartWorker("net")).not.toThrow();
  });

  it("restarts the VM when virtioNetMode changes (PCI contract change)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
      virtioNetMode: "modern",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const restartSpy = vi.spyOn(coordinator, "restart").mockImplementation(() => {});

    coordinator.updateConfig({
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
      virtioNetMode: "legacy",
    });

    expect(restartSpy).toHaveBeenCalledTimes(1);
  });

  it("restarts the VM when virtioInputMode changes (PCI contract change)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
      virtioNetMode: "modern",
      virtioInputMode: "modern",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const restartSpy = vi.spyOn(coordinator, "restart").mockImplementation(() => {});

    coordinator.updateConfig({
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
      virtioNetMode: "modern",
      virtioInputMode: "legacy",
    });

    expect(restartSpy).toHaveBeenCalledTimes(1);
  });

  it("restarts the VM when virtioSndMode changes (PCI contract change)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
      virtioNetMode: "modern",
      virtioSndMode: "modern",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const restartSpy = vi.spyOn(coordinator, "restart").mockImplementation(() => {});

    coordinator.updateConfig({
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
      virtioNetMode: "modern",
      virtioSndMode: "legacy",
    });

    expect(restartSpy).toHaveBeenCalledTimes(1);
  });

  it("restarts the VM when vramMiB changes (BAR1 VRAM layout change)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const restartSpy = vi.spyOn(coordinator, "restart").mockImplementation(() => {});

    coordinator.updateConfig({
      guestMemoryMiB: 1,
      vramMiB: 2,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    });

    expect(restartSpy).toHaveBeenCalledTimes(1);
  });

  it("restarts the VM when vmRuntime changes (legacy → machine)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      vmRuntime: "legacy",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const restartSpy = vi.spyOn(coordinator, "restart").mockImplementation(() => {});

    coordinator.updateConfig({
      vmRuntime: "machine",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    });

    expect(restartSpy).toHaveBeenCalledTimes(1);
  });

  it("restarts the VM when vmRuntime changes (machine → legacy)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      vmRuntime: "machine",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const restartSpy = vi.spyOn(coordinator, "restart").mockImplementation(() => {});

    coordinator.updateConfig({
      vmRuntime: "legacy",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    });

    expect(restartSpy).toHaveBeenCalledTimes(1);
  });

  it("does not restart when vmRuntime becomes explicit legacy (undefined → legacy) during other config updates", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    // Keep the config consistent with `allocateTestSegments` so we don't trigger a full restart
    // due to a guest memory layout change.
    const guestMemoryMiB = TEST_GUEST_MIB;
    // Older/compat configs may omit vmRuntime; treat that as legacy.
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const restartSpy = vi.spyOn(coordinator, "restart").mockImplementation(() => {});

    // Apply an unrelated update that happens to include an explicit legacy vmRuntime value.
    coordinator.updateConfig({
      vmRuntime: "legacy",
      guestMemoryMiB,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "debug",
    });

    expect(restartSpy).not.toHaveBeenCalled();
  });

  it("allows worker runtime start even without OPFS SyncAccessHandle", () => {
    const coordinator = new WorkerCoordinator();

    expect(() =>
      coordinator.start(
        {
          guestMemoryMiB: 1,
          vramMiB: 1,
          enableWorkers: true,
          enableWebGPU: false,
          proxyUrl: null,
          activeDiskImage: null,
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
    ).not.toThrow();
  });

  it("does not stop when setting boot disks without OPFS SyncAccessHandle", () => {
    const coordinator = new WorkerCoordinator();

    const baseConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
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

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    // Manually wire up a running coordinator without invoking `start()` so this
    // unit test stays lightweight (no WASM precompile attempts).
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).platformFeatures = platformFeatures;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = baseConfig;
    (coordinator as unknown as CoordinatorTestHarness).vmState = "running";
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    coordinator.setBootDisks({}, dummyHdd(), null);

    expect(coordinator.getVmState()).toBe("running");
    expect(coordinator.getLastFatalEvent()).toBeNull();

    const statuses = coordinator.getWorkerStatuses();
    expect(statuses.cpu.state).toBe("starting");
    expect(statuses.io.state).toBe("starting");
  });

  it("forwards audio/mic rings to CPU only in legacy demo mode by default (SPSC)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      // Ensure legacy demo mode is not derived from this deprecated config field.
      activeDiskImage: "ignored.img",
      vmRuntime: "legacy",
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    coordinator.setBootDisks({}, null, null);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(1024);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    const cpuAudio = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioAudio = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuAudio?.type).toBe("setAudioRingBuffer");
    expect(cpuAudio?.ringBuffer).toBe(audioSab);
    expect(ioAudio?.type).toBe("setAudioRingBuffer");
    expect(ioAudio?.ringBuffer).toBe(null);

    // Isolate mic-ring attachment behaviour from any prior audio-ring messages.
    cpuWorker.posted.length = 0;
    ioWorker.posted.length = 0;

    const micSab = new SharedArrayBuffer(256);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    const cpuMic = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuMic?.type).toBe("setMicrophoneRingBuffer");
    expect(cpuMic?.ringBuffer).toBe(micSab);
    // The IO worker must not receive the mic ring buffer in demo mode; it may not
    // receive an explicit detach message if it was already detached.
    expect(
      ioWorker.posted.some((m) => m.message?.type === "setMicrophoneRingBuffer" && m.message?.ringBuffer === micSab),
    ).toBe(false);
    const ioMic = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    if (ioMic) {
      expect(ioMic.type).toBe("setMicrophoneRingBuffer");
      expect(ioMic.ringBuffer).toBe(null);
    }
  });

  it("does not treat activeDiskImage as a VM-mode toggle for audio/mic ring routing (deprecated)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      // Ensure legacy VM mode is not derived from this deprecated config field.
      activeDiskImage: null,
      vmRuntime: "legacy",
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    coordinator.setBootDisks({ hddId: "disk1" }, null, null);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(1024);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    const cpuAudio = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioAudio = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuAudio?.type).toBe("setAudioRingBuffer");
    // Legacy runtime requests VM mode based on the boot disk mounts, not the deprecated `activeDiskImage`
    // config field. Even if disk metadata isn't loaded yet, mounting a boot disk should route SPSC
    // audio to the IO worker (which owns the guest device models).
    expect(cpuAudio?.ringBuffer).toBe(null);
    expect(ioAudio?.type).toBe("setAudioRingBuffer");
    expect(ioAudio?.ringBuffer).toBe(audioSab);

    const micSab = new SharedArrayBuffer(256);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    const ioMic = lastMessageOfType(ioWorker, "setMicrophoneRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(ioMic?.type).toBe("setMicrophoneRingBuffer");
    expect(ioMic?.ringBuffer).toBe(micSab);

    // The CPU worker must not receive the mic ring buffer in legacy VM mode.
    expect(
      cpuWorker.posted.some((m) => m.message?.type === "setMicrophoneRingBuffer" && m.message?.ringBuffer === micSab),
    ).toBe(false);
    const cpuMic = lastMessageOfType(cpuWorker, "setMicrophoneRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
    if (cpuMic) {
      expect(cpuMic.type).toBe("setMicrophoneRingBuffer");
      expect(cpuMic.ringBuffer).toBe(null);
    }
  });

  it("forwards audio/mic rings to IO only in legacy VM mode by default (SPSC)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      vmRuntime: "legacy",
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    coordinator.setBootDisks({}, dummyHdd(), null);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(1024);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    const cpuAudio = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioAudio = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuAudio?.type).toBe("setAudioRingBuffer");
    expect(cpuAudio?.ringBuffer).toBe(null);
    expect(ioAudio?.type).toBe("setAudioRingBuffer");
    expect(ioAudio?.ringBuffer).toBe(audioSab);

    // Isolate mic-ring attachment behaviour from any prior audio-ring messages.
    cpuWorker.posted.length = 0;
    ioWorker.posted.length = 0;

    const micSab = new SharedArrayBuffer(256);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    const ioMic = ioWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(ioMic?.type).toBe("setMicrophoneRingBuffer");
    expect(ioMic?.ringBuffer).toBe(micSab);
    // The CPU worker must not receive the mic ring buffer in VM mode; it may not
    // receive an explicit detach message if it was already detached.
    expect(
      cpuWorker.posted.some((m) => m.message?.type === "setMicrophoneRingBuffer" && m.message?.ringBuffer === micSab),
    ).toBe(false);
    const cpuMic = cpuWorker.posted.at(-1)?.message as { ringBuffer?: unknown; type?: unknown } | undefined;
    if (cpuMic) {
      expect(cpuMic.type).toBe("setMicrophoneRingBuffer");
      expect(cpuMic.ringBuffer).toBe(null);
    }
  });

  it.each([null, "disk.img"] as const)(
    "forwards audio/mic rings to CPU only in machine runtime by default (SPSC, activeDiskImage=%s)",
    (activeDiskImage) => {
      const coordinator = new WorkerCoordinator();
      const segments = allocateTestSegments();
      const shared = createSharedMemoryViews(segments);
      (coordinator as unknown as CoordinatorTestHarness).shared = shared;
      (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
        guestMemoryMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage,
        vmRuntime: "machine",
        logLevel: "info",
      };
      (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
      (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

      const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
      const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

      const audioSab = new SharedArrayBuffer(1024);
      coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

      const cpuAudio = lastMessageOfType(cpuWorker, "setAudioRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
      const ioAudio = lastMessageOfType(ioWorker, "setAudioRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
      expect(cpuAudio?.type).toBe("setAudioRingBuffer");
      expect(cpuAudio?.ringBuffer).toBe(audioSab);
      expect(ioAudio?.type).toBe("setAudioRingBuffer");
      expect(ioAudio?.ringBuffer).toBe(null);

      // Isolate mic-ring attachment behaviour from any prior audio-ring messages.
      cpuWorker.posted.length = 0;
      ioWorker.posted.length = 0;

      const micSab = new SharedArrayBuffer(256);
      coordinator.setMicrophoneRingBuffer(micSab, 48_000);

      const cpuMic = lastMessageOfType(cpuWorker, "setMicrophoneRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
      expect(cpuMic?.type).toBe("setMicrophoneRingBuffer");
      expect(cpuMic?.ringBuffer).toBe(micSab);

      // IO must not receive the mic ring buffer in machine runtime.
      expect(
        ioWorker.posted.some((m) => m.message?.type === "setMicrophoneRingBuffer" && m.message?.ringBuffer === micSab),
      ).toBe(false);
      const ioMic = lastMessageOfType(ioWorker, "setMicrophoneRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
      if (ioMic) {
        expect(ioMic.type).toBe("setMicrophoneRingBuffer");
        expect(ioMic.ringBuffer).toBe(null);
      }
    },
  );

  it("can re-route audio ring ownership via setAudioRingBufferOwner", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(1024);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    // Default demo-mode owner is CPU.
    expect(lastPostedRingBuffer(cpuWorker)).toBe(audioSab);
    expect(lastPostedRingBuffer(ioWorker)).toBe(null);

    coordinator.setAudioRingBufferOwner("io");

    // Now the CPU must be detached and the IO worker must receive the SAB.
    expect(lastPostedRingBuffer(cpuWorker)).toBe(null);
    expect(lastPostedRingBuffer(ioWorker)).toBe(audioSab);

    // Clearing the override should restore the default routing policy (CPU in demo mode).
    coordinator.setAudioRingBufferOwner(null);
    expect(lastPostedRingBuffer(cpuWorker)).toBe(audioSab);
    expect(lastPostedRingBuffer(ioWorker)).toBe(null);
  });

  it("can re-route microphone ring ownership via setMicrophoneRingBufferOwner", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

    const micSab = new SharedArrayBuffer(256);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    // Default demo-mode owner is CPU.
    expect(lastPostedRingBuffer(cpuWorker)).toBe(micSab);
    expect(lastPostedRingBuffer(ioWorker)).toBe(null);

    coordinator.setMicrophoneRingBufferOwner("io");

    // Now the CPU must be detached and the IO worker must receive the SAB.
    expect(lastPostedRingBuffer(cpuWorker)).toBe(null);
    expect(lastPostedRingBuffer(ioWorker)).toBe(micSab);

    // Clearing the override should restore the default routing policy (CPU in demo mode).
    coordinator.setMicrophoneRingBufferOwner(null);
    expect(lastPostedRingBuffer(cpuWorker)).toBe(micSab);
    expect(lastPostedRingBuffer(ioWorker)).toBe(null);
  });

  it("sends net.trace.enable to the net worker when enabling net tracing", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments);

    const netWorker = (coordinator as unknown as CoordinatorTestHarness).workers.net.worker as MockWorker;
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
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments);

    const netInfo = (coordinator as unknown as CoordinatorTestHarness).workers.net as { instanceId: number; worker: MockWorker };
    const netWorker = netInfo.worker;

    const promise = coordinator.takeNetTracePcapng();

    const lastPosted = netWorker.posted.at(-1)?.message as { kind?: unknown; requestId?: unknown } | undefined;
    expect(lastPosted?.kind).toBe("net.trace.take_pcapng");
    expect(typeof lastPosted?.requestId).toBe("number");
    const requestId = lastPosted!.requestId as number;

    const expectedBytes = new Uint8Array([0x61, 0x65, 0x72, 0x6f]); // "aero"
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("net", netInfo.instanceId, {
      kind: "net.trace.pcapng",
      requestId,
      bytes: expectedBytes.buffer,
    });

    const actualBytes = await promise;
    expect(actualBytes).toBeInstanceOf(Uint8Array);
    expect(Array.from(actualBytes)).toEqual(Array.from(expectedBytes));
  });

  it("net trace requests reject with an Error when net.postMessage throws a hostile proxy", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments);

    const netInfo = (coordinator as unknown as CoordinatorTestHarness).workers.net as { instanceId: number; worker: MockWorker };
    const netWorker = netInfo.worker;

    const hostile = new Proxy(
      {},
      {
        getPrototypeOf() {
          throw new Error("boom");
        },
      },
    );

    // Make postMessage throw a value for which `err instanceof Error` would throw.
    netWorker.postMessage = () => {
      throw hostile;
    };

    await expect(coordinator.takeNetTracePcapng()).rejects.toBeInstanceOf(Error);
  });

  it("roundtrips net.trace.export_pcapng request/response through the coordinator", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments);

    const netInfo = (coordinator as unknown as CoordinatorTestHarness).workers.net as { instanceId: number; worker: MockWorker };
    const netWorker = netInfo.worker;

    const promise = coordinator.exportNetTracePcapng();

    const lastPosted = netWorker.posted.at(-1)?.message as { kind?: unknown; requestId?: unknown } | undefined;
    expect(lastPosted?.kind).toBe("net.trace.export_pcapng");
    expect(typeof lastPosted?.requestId).toBe("number");
    const requestId = lastPosted!.requestId as number;

    const expectedBytes = new Uint8Array([0x61, 0x65, 0x72, 0x6f]); // "aero"
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("net", netInfo.instanceId, {
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
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments);

    const netInfo = (coordinator as unknown as CoordinatorTestHarness).workers.net as { instanceId: number; worker: MockWorker };
    const netWorker = netInfo.worker;

    const promise = coordinator.getNetTraceStats();

    const lastPosted = netWorker.posted.at(-1)?.message as { kind?: unknown; requestId?: unknown } | undefined;
    expect(lastPosted?.kind).toBe("net.trace.status");
    expect(typeof lastPosted?.requestId).toBe("number");
    const requestId = lastPosted!.requestId as number;

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("net", netInfo.instanceId, {
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

  it("buffers aerogpu.submit until GPU READY and forwards submit_complete fences back to the CPU worker", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);

    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu as { instanceId: number; worker: MockWorker };
    const gpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.gpu as { instanceId: number; worker: MockWorker };
    const cpuWorker = cpuInfo.worker;
    const gpuWorker = gpuInfo.worker;
    cpuWorker.posted.length = 0;
    gpuWorker.posted.length = 0;

    // Submit before the GPU worker is READY; coordinator should buffer it.
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      kind: "aerogpu.submit",
      contextId: 1,
      engineId: 9,
      flags: 0x1234,
      signalFence: 5n,
      cmdStream: new Uint8Array([1, 2, 3, 4]).buffer,
    });
    expect(lastMessageOfType(gpuWorker, "submit_aerogpu")).toBeUndefined();

    // Mark GPU worker READY; coordinator should flush the buffered submit.
    gpuWorker.onmessage?.({ data: { type: MessageType.READY, role: "gpu" } } as MessageEvent);
    const submitMsg = lastMessageOfType(gpuWorker, "submit_aerogpu") as
      | {
          protocol?: unknown;
          protocolVersion?: unknown;
          requestId?: unknown;
          contextId?: unknown;
          signalFence?: unknown;
          engineId?: unknown;
          flags?: unknown;
        }
      | undefined;
    expect(submitMsg?.protocol).toBe(GPU_PROTOCOL_NAME);
    expect(submitMsg?.protocolVersion).toBe(GPU_PROTOCOL_VERSION);
    expect(submitMsg?.contextId).toBe(1);
    expect(submitMsg?.engineId).toBe(9);
    expect(submitMsg?.flags).toBe(0x1234);
    expect(submitMsg?.signalFence).toBe(5n);
    expect(typeof submitMsg?.requestId).toBe("number");

    // GPU worker reports submit_complete; coordinator should forward the fence completion.
    const requestId = submitMsg!.requestId as number;
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("gpu", gpuInfo.instanceId, {
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "submit_complete",
      requestId,
      completedFence: 5n,
    });

    expect(cpuWorker.posted).toContainEqual({
      message: { kind: "aerogpu.complete_fence", fence: 5n },
      transfer: undefined,
    });

    // Untracked submit_complete messages must be ignored.
    cpuWorker.posted.length = 0;
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("gpu", gpuInfo.instanceId, {
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "submit_complete",
      requestId: requestId + 123,
      completedFence: 99n,
    });
    expect(cpuWorker.posted.length).toBe(0);
  });

  it("forwards AeroGPU allocTable buffers (and includes them in the transfer list)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);

    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu as { instanceId: number; worker: MockWorker };
    const gpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.gpu as { instanceId: number; worker: MockWorker };
    const gpuWorker = gpuInfo.worker;
    gpuWorker.posted.length = 0;

    const cmdStream = new Uint8Array([1, 2, 3, 4]).buffer;
    const allocTable = new Uint8Array([9, 8, 7]).buffer;
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      kind: "aerogpu.submit",
      contextId: 1,
      signalFence: 5n,
      cmdStream,
      allocTable,
    });

    // Not READY yet; buffered.
    expect(lastMessageOfType(gpuWorker, "submit_aerogpu")).toBeUndefined();

    // READY flushes the buffered submit.
    gpuWorker.onmessage?.({ data: { type: MessageType.READY, role: "gpu" } } as MessageEvent);
    const lastPosted = gpuWorker.posted.at(-1) as { message: unknown; transfer?: unknown[] } | undefined;
    expect((lastPosted?.message as { type?: unknown }).type).toBe("submit_aerogpu");

    const submitMsg = lastPosted?.message as { cmdStream?: unknown; allocTable?: unknown } | undefined;
    expect(submitMsg?.cmdStream).toBeInstanceOf(ArrayBuffer);
    expect(submitMsg?.allocTable).toBeInstanceOf(ArrayBuffer);

    // Coordinator should include both buffers in the transfer list.
    expect(Array.isArray(lastPosted?.transfer)).toBe(true);
    expect(lastPosted?.transfer?.length).toBe(2);
    expect(lastPosted?.transfer?.[0]).toBe(cmdStream);
    expect(lastPosted?.transfer?.[1]).toBe(allocTable);
  });

  it("ignores aerogpu.submit messages from non-CPU workers", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const gpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.gpu as { instanceId: number; worker: MockWorker };
    const ioInfo = (coordinator as unknown as CoordinatorTestHarness).workers.io as { instanceId: number; worker: MockWorker };
    const gpuWorker = gpuInfo.worker;

    // Bring GPU worker to READY so a non-CPU submit would immediately forward if not gated.
    gpuWorker.onmessage?.({ data: { type: MessageType.READY, role: "gpu" } } as MessageEvent);
    gpuWorker.posted.length = 0;

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("io", ioInfo.instanceId, {
      kind: "aerogpu.submit",
      contextId: 1,
      signalFence: 5n,
      cmdStream: new Uint8Array([1, 2, 3, 4]).buffer,
    });

    expect(lastMessageOfType(gpuWorker, "submit_aerogpu")).toBeUndefined();
    expect(((coordinator as unknown as CoordinatorTestHarness).pendingAerogpuSubmissions as unknown[]).length).toBe(0);
    expect(((coordinator as unknown as CoordinatorTestHarness).aerogpuInFlightFencesByRequestId as Map<number, bigint>).size).toBe(0);
  });

  it("bounds pending aerogpu.submit queue while GPU is not ready and force-completes dropped fences", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);

    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu as { instanceId: number; worker: MockWorker };
    const cpuWorker = cpuInfo.worker;
    cpuWorker.posted.length = 0;

    const total = 300;
    for (let i = 1; i <= total; i += 1) {
      (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
        kind: "aerogpu.submit",
        contextId: 0,
        signalFence: BigInt(i),
        cmdStream: new Uint8Array([i & 0xff]).buffer,
      });
    }

    const pending = (coordinator as unknown as CoordinatorTestHarness).pendingAerogpuSubmissions as unknown[];
    const pendingLen = pending.length;
    expect(pendingLen).toBeGreaterThan(0);
    expect(pendingLen).toBeLessThanOrEqual(total);

    const dropped = total - pendingLen;
    expect(dropped).toBeGreaterThan(0);

    const completions = cpuWorker.posted
      .map((p) => p.message as { kind?: unknown; fence?: unknown })
      .filter((m) => m.kind === "aerogpu.complete_fence");
    expect(completions).toHaveLength(dropped);

    // Dropped submissions are FIFO; expect the earliest fences to be completed first.
    const completedFences = completions.map((m) => m.fence);
    for (let i = 1; i <= dropped; i += 1) {
      expect(completedFences).toContain(BigInt(i));
    }
  });

  it("forces completion of in-flight AeroGPU fences when the GPU worker is terminated", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);

    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu as { instanceId: number; worker: MockWorker };
    const gpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.gpu as { instanceId: number; worker: MockWorker };
    const cpuWorker = cpuInfo.worker;
    const gpuWorker = gpuInfo.worker;

    // Bring GPU worker to READY so submissions are sent immediately and tracked as in-flight.
    gpuWorker.onmessage?.({ data: { type: MessageType.READY, role: "gpu" } } as MessageEvent);
    cpuWorker.posted.length = 0;
    gpuWorker.posted.length = 0;

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      kind: "aerogpu.submit",
      contextId: 2,
      signalFence: 7n,
      cmdStream: new Uint8Array([9, 9, 9]).buffer,
    });

    expect(lastMessageOfType(gpuWorker, "submit_aerogpu")).toBeTruthy();

    // If the GPU worker is killed before it can send submit_complete, force-complete the fence so
    // the guest won't deadlock.
    (coordinator as unknown as CoordinatorTestHarness).terminateWorker("gpu");
    expect(cpuWorker.posted).toContainEqual({
      message: { kind: "aerogpu.complete_fence", fence: 7n },
      transfer: undefined,
    });
  });

  it("completes AeroGPU fences if posting submit_aerogpu to the GPU worker throws", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);

    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu as { instanceId: number; worker: MockWorker };
    const gpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.gpu as { instanceId: number; worker: MockWorker };
    const cpuWorker = cpuInfo.worker;
    const gpuWorker = gpuInfo.worker;

    // Bring GPU worker to READY so the coordinator attempts to post submits immediately.
    gpuWorker.onmessage?.({ data: { type: MessageType.READY, role: "gpu" } } as MessageEvent);

    cpuWorker.posted.length = 0;
    gpuWorker.posted.length = 0;

    // Simulate a transient postMessage failure (e.g. worker crash / structured clone error).
    gpuWorker.postMessage = () => {
      throw new Error("postMessage failed");
    };

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      kind: "aerogpu.submit",
      contextId: 0,
      signalFence: 42n,
      cmdStream: new Uint8Array([1, 2, 3]).buffer,
    });

    // Coordinator should not strand the guest waiting on a fence completion.
    expect(cpuWorker.posted).toContainEqual({
      message: { kind: "aerogpu.complete_fence", fence: 42n },
      transfer: undefined,
    });

    // Ensure we didn't leak an in-flight tracking entry.
    expect(((coordinator as unknown as CoordinatorTestHarness).aerogpuInFlightFencesByRequestId as Map<number, bigint>).size).toBe(0);
    expect(((coordinator as unknown as CoordinatorTestHarness).pendingAerogpuSubmissions as unknown[]).length).toBe(0);
  });

  it("falls back to posting submit_aerogpu without a transfer list if the transfer list is rejected", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("gpu", segments);

    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu as { instanceId: number; worker: MockWorker };
    const gpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.gpu as { instanceId: number; worker: MockWorker };
    const cpuWorker = cpuInfo.worker;
    const gpuWorker = gpuInfo.worker;

    // Bring GPU worker to READY so the coordinator attempts to post submits immediately.
    gpuWorker.onmessage?.({ data: { type: MessageType.READY, role: "gpu" } } as MessageEvent);

    cpuWorker.posted.length = 0;
    gpuWorker.posted.length = 0;

    const postAttempts: Array<{ hasTransfer: boolean }> = [];
    const originalPostMessage = gpuWorker.postMessage.bind(gpuWorker);
    gpuWorker.postMessage = (message: unknown, transfer?: unknown[]) => {
      postAttempts.push({ hasTransfer: Array.isArray(transfer) && transfer.length > 0 });
      if (transfer && transfer.length > 0) {
        throw new Error("transfer list rejected");
      }
      return originalPostMessage(message, transfer);
    };

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      kind: "aerogpu.submit",
      contextId: 0,
      signalFence: 42n,
      cmdStream: new Uint8Array([1, 2, 3]).buffer,
    });

    expect(postAttempts.length).toBe(2);
    expect(postAttempts[0]?.hasTransfer).toBe(true);
    expect(postAttempts[1]?.hasTransfer).toBe(false);

    const submitMsg = lastMessageOfType(gpuWorker, "submit_aerogpu") as { requestId?: unknown; signalFence?: unknown } | undefined;
    expect(submitMsg?.signalFence).toBe(42n);
    expect(typeof submitMsg?.requestId).toBe("number");

    // Ensure we didn't force-complete the fence just because the transfer-list post failed (we
    // should only force-complete if both transfer and structured clone fail).
    expect(cpuWorker.posted.some((p) => (p.message as { kind?: unknown }).kind === "aerogpu.complete_fence")).toBe(false);

    // GPU worker reports submit_complete; coordinator should forward the fence completion and drop tracking.
    const requestId = submitMsg!.requestId as number;
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("gpu", gpuInfo.instanceId, {
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "submit_complete",
      requestId,
      completedFence: 42n,
    });

    expect(cpuWorker.posted).toContainEqual({
      message: { kind: "aerogpu.complete_fence", fence: 42n },
      transfer: undefined,
    });
    expect(((coordinator as unknown as CoordinatorTestHarness).aerogpuInFlightFencesByRequestId as Map<number, bigint>).size).toBe(0);
  });

  it("rejects pending net trace requests when the net worker is terminated", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments);

    const promise = coordinator.takeNetTracePcapng(60_000);
    (coordinator as unknown as CoordinatorTestHarness).terminateWorker("net");

    await expect(promise).rejects.toThrow(/net worker restarted/i);
  });

  it("rejects pending net trace stats requests when the net worker is terminated", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments);

    const promise = coordinator.getNetTraceStats(60_000);
    (coordinator as unknown as CoordinatorTestHarness).terminateWorker("net");

    await expect(promise).rejects.toThrow(/net worker restarted/i);
  });

  it("enforces SPSC ownership when switching audio/mic ring buffer attachments between workers", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

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

  it("does not re-sync audio/mic ring attachments when a non-audio worker reports READY", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("net", segments);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;
    const netWorker = (coordinator as unknown as CoordinatorTestHarness).workers.net.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(16);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    const micSab = new SharedArrayBuffer(16);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    // Clear any initial attachment messages so we only observe READY-triggered behaviour.
    cpuWorker.posted.length = 0;
    ioWorker.posted.length = 0;

    // Trigger READY for a non-audio worker (net). The coordinator should not re-send
    // mic/audio ring attachment messages to CPU/IO, avoiding unnecessary mic ring flushes.
    netWorker.onmessage?.({ data: { type: MessageType.READY, role: "net" } } as MessageEvent);

    expect(cpuWorker.posted).toEqual([]);
    expect(ioWorker.posted).toEqual([]);
  });

  it("re-syncs audio/mic ring attachments only to the worker that reported READY", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(16);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    const micSab = new SharedArrayBuffer(16);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    // Simulate the CPU worker being restarted. The replacement instance should inherit the
    // stored SAB attachments when it reports READY, but other workers should not.
    (coordinator as unknown as CoordinatorTestHarness).terminateWorker("cpu");
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);

    const restartedCpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;

    // Clear any prior messages so we only observe READY-triggered behaviour.
    restartedCpuWorker.posted.length = 0;
    ioWorker.posted.length = 0;

    restartedCpuWorker.onmessage?.({ data: { type: MessageType.READY, role: "cpu" } } as MessageEvent);

    // Default demo-mode owner is CPU, so the restarted worker should receive the SAB attachments.
    expect(restartedCpuWorker.posted.some((m) => m.message?.type === "setAudioRingBuffer" && m.message?.ringBuffer === audioSab)).toBe(
      true,
    );
    expect(
      restartedCpuWorker.posted.some((m) => m.message?.type === "setMicrophoneRingBuffer" && m.message?.ringBuffer === micSab),
    ).toBe(true);

    // The IO worker did not report READY, so it should not receive redundant attachments.
    expect(ioWorker.posted).toEqual([]);
  });

  it("does not re-send audio/mic ring attachments on unrelated config updates", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    const baseConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info" as const,
    };
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = baseConfig;
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

    const audioSab = new SharedArrayBuffer(16);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);
    const micSab = new SharedArrayBuffer(16);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    // Clear the initial attachment messages and then apply an unrelated config change.
    // The coordinator should not re-send the ring attachments (which can cause device
    // models to reattach + flush/discard microphone samples).
    cpuWorker.posted.length = 0;
    ioWorker.posted.length = 0;

    coordinator.updateConfig({ ...baseConfig, logLevel: "debug" });

    const isRingMsg = (m: { message?: any }) =>
      m.message?.type === "setAudioRingBuffer" || m.message?.type === "setMicrophoneRingBuffer";
    expect(cpuWorker.posted.some(isRingMsg)).toBe(false);
    expect(ioWorker.posted.some(isRingMsg)).toBe(false);
  });

  it("resets scanoutState back to legacy on VM reset (shared memory preserved)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as unknown as CoordinatorTestHarness).vmState = "running";

    const scanout = shared.scanoutStateI32;
    expect(scanout).toBeTruthy();
    if (!scanout) return;

    publishScanoutState(scanout, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: 0,
      basePaddrHi: 0,
      width: 0,
      height: 0,
      pitchBytes: 0,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });
    const before = snapshotScanoutState(scanout);
    expect(before.source).toBe(SCANOUT_SOURCE_WDDM);

    coordinator.reset("test");

    const after = snapshotScanoutState(scanout);
    expect(after.source).toBe(SCANOUT_SOURCE_LEGACY_TEXT);
  });
});
