import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import { WorkerCoordinator } from "./coordinator";
import { MessageType } from "./protocol";
import { createSharedMemoryViews } from "./shared_layout";
import { allocateHarnessSharedMemorySegments } from "./harness_shared_memory";
import type { DiskImageMetadata } from "../storage/metadata";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
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
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const originalWorker = (globalThis as any).Worker as unknown;
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
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);

    // Wire the shared memory view manually so we can call the private spawnWorker
    // helper without running the full coordinator lifecycle.
    (coordinator as any).shared = shared;

    expect(() => (coordinator as any).spawnWorker("net", segments)).not.toThrow();
    expect((coordinator as any).workers.net).toBeTruthy();
  });

  it("spawns the machine CPU worker entrypoint when vmRuntime=machine", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      vmRuntime: "machine",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };

    (coordinator as any).spawnWorker("cpu", segments);
    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(String(cpuWorker.specifier)).toMatch(/machine_cpu\.worker\.ts/);
  });

  it("spawns the machine CPU worker via start() when vmRuntime=machine", () => {
    const coordinator = new WorkerCoordinator();

    // Avoid kicking off the full worker event loops / wasm precompile in this unit test;
    // we only care about the worker entrypoint selection.
    (coordinator as any).eventLoop = vi.fn(async () => {});
    (coordinator as any).postWorkerInitMessages = vi.fn(async () => {});

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

    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(String(cpuWorker.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("spawns the legacy CPU worker via start() when vmRuntime is omitted", () => {
    const coordinator = new WorkerCoordinator();

    // Avoid kicking off the full worker event loops / wasm precompile in this unit test;
    // we only care about the worker entrypoint selection.
    (coordinator as any).eventLoop = vi.fn(async () => {});
    (coordinator as any).postWorkerInitMessages = vi.fn(async () => {});

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

    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    const specifier = String(cpuWorker.specifier);
    expect(specifier).toMatch(/cpu\.worker\.ts/);
    expect(specifier).not.toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("preserves the machine CPU worker entrypoint across full restarts", () => {
    vi.useFakeTimers();
    const coordinator = new WorkerCoordinator();

    // Stub out heavyweight background tasks; we only care about which worker URL is chosen.
    (coordinator as any).eventLoop = vi.fn(async () => {});
    (coordinator as any).postWorkerInitMessages = vi.fn(async () => {});

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

    const cpuWorkerBefore = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerBefore.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    // Trigger a full restart (the path used for non-restartable worker failures like CPU/IO).
    (coordinator as any).scheduleFullRestart("test_full_restart");
    vi.runAllTimers();

    const cpuWorkerAfter = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(cpuWorkerAfter).not.toBe(cpuWorkerBefore);
    expect(String(cpuWorkerAfter.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
    vi.useRealTimers();
  });

  it("preserves the machine CPU worker entrypoint across restart()", () => {
    const coordinator = new WorkerCoordinator();
    (coordinator as any).eventLoop = vi.fn(async () => {});
    (coordinator as any).postWorkerInitMessages = vi.fn(async () => {});

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

    const cpuWorkerBefore = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerBefore.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.restart();

    const cpuWorkerAfter = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(cpuWorkerAfter).not.toBe(cpuWorkerBefore);
    expect(String(cpuWorkerAfter.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("switches the CPU worker entrypoint when vmRuntime changes via updateConfig()", () => {
    const coordinator = new WorkerCoordinator();
    (coordinator as any).eventLoop = vi.fn(async () => {});
    (coordinator as any).postWorkerInitMessages = vi.fn(async () => {});

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

    const cpuWorkerLegacy = (coordinator as any).workers.cpu.worker as MockWorker;
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

    const cpuWorkerMachine = (coordinator as any).workers.cpu.worker as MockWorker;
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

    const cpuWorkerLegacyAgain = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(cpuWorkerLegacyAgain).not.toBe(cpuWorkerMachine);
    expect(String(cpuWorkerLegacyAgain.specifier)).toMatch(/cpu\.worker\.ts/);
    expect(String(cpuWorkerLegacyAgain.specifier)).not.toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("preserves the machine CPU worker entrypoint across VM reset (shared memory preserved)", () => {
    const coordinator = new WorkerCoordinator();

    // Avoid kicking off the full worker event loops / wasm precompile in this unit test;
    // we only care about the worker entrypoint selection.
    (coordinator as any).eventLoop = vi.fn(async () => {});
    (coordinator as any).postWorkerInitMessages = vi.fn(async () => {});

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

    const cpuWorkerBefore = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerBefore.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.reset("test_reset");

    const cpuWorkerAfter = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(cpuWorkerAfter).not.toBe(cpuWorkerBefore);
    expect(String(cpuWorkerAfter.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.stop();
  });

  it("preserves the machine CPU worker entrypoint when restartWorker('cpu') falls back to restart()", () => {
    const coordinator = new WorkerCoordinator();

    // Avoid kicking off the full worker event loops / wasm precompile in this unit test;
    // we only care about the worker entrypoint selection.
    (coordinator as any).eventLoop = vi.fn(async () => {});
    (coordinator as any).postWorkerInitMessages = vi.fn(async () => {});

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

    const cpuWorkerBefore = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(String(cpuWorkerBefore.specifier)).toMatch(/machine_cpu\.worker\.ts/);

    coordinator.restartWorker("cpu");

    const cpuWorkerAfter = (coordinator as any).workers.cpu.worker as MockWorker;
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
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
      virtioNetMode: "modern",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

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
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
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
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

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
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
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
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

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
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

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
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      vmRuntime: "legacy",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

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
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      vmRuntime: "machine",
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

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
    (coordinator as any).shared = shared;
    // Keep the config consistent with `allocateTestSegments` so we don't trigger a full restart
    // due to a guest memory layout change.
    const guestMemoryMiB = TEST_GUEST_MIB;
    // Older/compat configs may omit vmRuntime; treat that as legacy.
    (coordinator as any).activeConfig = {
      guestMemoryMiB,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

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
    (coordinator as any).shared = shared;
    (coordinator as any).platformFeatures = platformFeatures;
    (coordinator as any).activeConfig = baseConfig;
    (coordinator as any).vmState = "running";
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

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
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      vmRuntime: "legacy",
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

  it("does not treat activeDiskImage as a VM-mode toggle for audio/mic ring routing (deprecated)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: "disk.img",
      vmRuntime: "legacy",
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

    const cpuMic = lastMessageOfType(cpuWorker, "setMicrophoneRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioMic = lastMessageOfType(ioWorker, "setMicrophoneRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuMic?.type).toBe("setMicrophoneRingBuffer");
    expect(cpuMic?.ringBuffer).toBe(micSab);
    expect(ioMic?.type).toBe("setMicrophoneRingBuffer");
    expect(ioMic?.ringBuffer).toBe(null);
  });

  it("forwards audio/mic rings to IO only in legacy VM mode by default (SPSC)", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      vmRuntime: "legacy",
      logLevel: "info",
    };
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

    coordinator.setBootDisks({}, dummyHdd(), null);

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

    // The coordinator suppresses re-sending identical ring-buffer attachments to avoid
    // resetting device state / discarding buffered microphone samples. In VM mode, the
    // CPU worker's mic attachment stays `null`, so a call to `setMicrophoneRingBuffer`
    // may only send a message to the IO worker.
    const cpuMic = lastMessageOfType(cpuWorker, "setMicrophoneRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
    const ioMic = lastMessageOfType(ioWorker, "setMicrophoneRingBuffer") as { ringBuffer?: unknown; type?: unknown } | undefined;
    expect(cpuMic?.type).toBe("setMicrophoneRingBuffer");
    expect(cpuMic?.ringBuffer).toBe(null);
    expect(ioMic?.type).toBe("setMicrophoneRingBuffer");
    expect(ioMic?.ringBuffer).toBe(micSab);
  });

  it.each([null, "disk.img"] as const)(
    "forwards audio/mic rings to CPU only in machine runtime by default (SPSC, activeDiskImage=%s)",
    (activeDiskImage) => {
      const coordinator = new WorkerCoordinator();
      const segments = allocateTestSegments();
      const shared = createSharedMemoryViews(segments);
      (coordinator as any).shared = shared;
      (coordinator as any).activeConfig = {
        guestMemoryMiB: 1,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage,
        vmRuntime: "machine",
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
    },
  );

  it("can re-route audio ring ownership via setAudioRingBufferOwner", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
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

    // Clearing the override should restore the default routing policy (CPU in demo mode).
    coordinator.setAudioRingBufferOwner(null);
    expect((cpuWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(audioSab);
    expect((ioWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(null);
  });

  it("can re-route microphone ring ownership via setMicrophoneRingBufferOwner", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
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

    const micSab = new SharedArrayBuffer(256);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    // Default demo-mode owner is CPU.
    expect((cpuWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(micSab);
    expect((ioWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(null);

    coordinator.setMicrophoneRingBufferOwner("io");

    // Now the CPU must be detached and the IO worker must receive the SAB.
    expect((cpuWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(null);
    expect((ioWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(micSab);

    // Clearing the override should restore the default routing policy (CPU in demo mode).
    coordinator.setMicrophoneRingBufferOwner(null);
    expect((cpuWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(micSab);
    expect((ioWorker.posted.at(-1)?.message as any)?.ringBuffer).toBe(null);
  });

  it("sends net.trace.enable to the net worker when enabling net tracing", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
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
    const segments = allocateTestSegments();
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
    const segments = allocateTestSegments();
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
    const segments = allocateTestSegments();
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

  it("buffers aerogpu.submit until GPU READY and forwards submit_complete fences back to the CPU worker", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("gpu", segments);

    const cpuInfo = (coordinator as any).workers.cpu as { instanceId: number; worker: MockWorker };
    const gpuInfo = (coordinator as any).workers.gpu as { instanceId: number; worker: MockWorker };
    const cpuWorker = cpuInfo.worker;
    const gpuWorker = gpuInfo.worker;
    cpuWorker.posted.length = 0;
    gpuWorker.posted.length = 0;

    // Submit before the GPU worker is READY; coordinator should buffer it.
    (coordinator as any).onWorkerMessage("cpu", cpuInfo.instanceId, {
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
    (coordinator as any).onWorkerMessage("gpu", gpuInfo.instanceId, {
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
    (coordinator as any).onWorkerMessage("gpu", gpuInfo.instanceId, {
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "submit_complete",
      requestId: requestId + 123,
      completedFence: 99n,
    });
    expect(cpuWorker.posted.length).toBe(0);
  });

  it("forces completion of in-flight AeroGPU fences when the GPU worker is terminated", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("gpu", segments);

    const cpuInfo = (coordinator as any).workers.cpu as { instanceId: number; worker: MockWorker };
    const gpuInfo = (coordinator as any).workers.gpu as { instanceId: number; worker: MockWorker };
    const cpuWorker = cpuInfo.worker;
    const gpuWorker = gpuInfo.worker;

    // Bring GPU worker to READY so submissions are sent immediately and tracked as in-flight.
    gpuWorker.onmessage?.({ data: { type: MessageType.READY, role: "gpu" } } as MessageEvent);
    cpuWorker.posted.length = 0;
    gpuWorker.posted.length = 0;

    (coordinator as any).onWorkerMessage("cpu", cpuInfo.instanceId, {
      kind: "aerogpu.submit",
      contextId: 2,
      signalFence: 7n,
      cmdStream: new Uint8Array([9, 9, 9]).buffer,
    });

    expect(lastMessageOfType(gpuWorker, "submit_aerogpu")).toBeTruthy();

    // If the GPU worker is killed before it can send submit_complete, force-complete the fence so
    // the guest won't deadlock.
    (coordinator as any).terminateWorker("gpu");
    expect(cpuWorker.posted).toContainEqual({
      message: { kind: "aerogpu.complete_fence", fence: 7n },
      transfer: undefined,
    });
  });

  it("rejects pending net trace requests when the net worker is terminated", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const promise = coordinator.takeNetTracePcapng(60_000);
    (coordinator as any).terminateWorker("net");

    await expect(promise).rejects.toThrow(/net worker restarted/i);
  });

  it("rejects pending net trace stats requests when the net worker is terminated", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const promise = coordinator.getNetTraceStats(60_000);
    (coordinator as any).terminateWorker("net");

    await expect(promise).rejects.toThrow(/net worker restarted/i);
  });

  it("enforces SPSC ownership when switching audio/mic ring buffer attachments between workers", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
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

  it("does not re-sync audio/mic ring attachments when a non-audio worker reports READY", () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateTestSegments();
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
    (coordinator as any).spawnWorker("net", segments);

    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as any).workers.io.worker as MockWorker;
    const netWorker = (coordinator as any).workers.net.worker as MockWorker;

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

    const audioSab = new SharedArrayBuffer(16);
    coordinator.setAudioRingBuffer(audioSab, 128, 2, 48_000);

    const micSab = new SharedArrayBuffer(16);
    coordinator.setMicrophoneRingBuffer(micSab, 48_000);

    // Simulate the CPU worker being restarted. The replacement instance should inherit the
    // stored SAB attachments when it reports READY, but other workers should not.
    (coordinator as any).terminateWorker("cpu");
    (coordinator as any).spawnWorker("cpu", segments);

    const restartedCpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;

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
    (coordinator as any).shared = shared;
    const baseConfig = {
      guestMemoryMiB: 1,
      vramMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info" as const,
    };
    (coordinator as any).activeConfig = baseConfig;
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as any).workers.io.worker as MockWorker;

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
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = {
      guestMemoryMiB: 1,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    };
    (coordinator as any).vmState = "running";

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
