import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import type { DiskImageMetadata } from "../storage/metadata";
import { WorkerCoordinator } from "./coordinator";
import { emptySetBootDisksMessage, type SetBootDisksMessage } from "./boot_disks_protocol";
import { createIoIpcSab, createSharedMemoryViews } from "./shared_layout";
import { allocateHarnessSharedMemorySegments } from "./harness_shared_memory";
import { ErrorCode, MessageType } from "./protocol";

class MockWorker {
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
  }

  terminate(): void {}
}

describe("runtime/coordinator (boot disks forwarding)", () => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const originalWorker = (globalThis as any).Worker as unknown;

  beforeEach(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).Worker = MockWorker;
    vi.spyOn(perf, "registerWorker").mockImplementation(() => 0);
  });

  afterEach(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).Worker = originalWorker as any;
    vi.restoreAllMocks();
  });

  function makeLocalDisk(meta: Omit<Extract<DiskImageMetadata, { source: "local" }>, "source">): DiskImageMetadata {
    return { ...meta, source: "local" };
  }

  function allocateTestSegments() {
    return allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpc: createIoIpcSab(),
      vramBytes: 0,
    });
  }

  it("resends boot disk selection to the CPU worker when vmRuntime=machine and the worker restarts", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    // Manually wire shared memory so we can spawn workers without invoking `start()`.
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = { vmRuntime: "machine" };

    const hdd = makeLocalDisk({
      id: "hdd1",
      name: "disk.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    });
    const cd = makeLocalDisk({
      id: "cd1",
      name: "install.iso",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "install.iso",
      sizeBytes: 2048,
      createdAtMs: 0,
    });

    coordinator.setBootDisks({ hddId: hdd.id, cdId: cd.id }, hdd, cd);

    // Spawn the workers; the coordinator should forward `setBootDisks` to CPU (machine runtime)
    // and *not* forward disk metadata to IO.
    (coordinator as any).spawnWorker("cpu", segments);
    (coordinator as any).spawnWorker("io", segments);

    const cpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as any).workers.io.worker as MockWorker;

    const expectedCpuMessage = {
      ...emptySetBootDisksMessage(),
      mounts: { hddId: "hdd1", cdId: "cd1" },
      hdd,
      cd,
      bootDevice: "cdrom",
    } satisfies SetBootDisksMessage;
    const expectedIoMessage = { ...expectedCpuMessage, hdd: null, cd: null } satisfies SetBootDisksMessage;

    expect(cpuWorker.posted).toContainEqual({
      message: expectedCpuMessage,
      transfer: undefined,
    });

    // IO worker must not receive disk metadata in machine runtime mode (avoid OPFS double-open).
    expect(ioWorker.posted).toContainEqual({
      message: expectedIoMessage,
      transfer: undefined,
    });

    // Simulate the CPU worker being restarted; the replacement instance should inherit the stored selection.
    (coordinator as any).terminateWorker("cpu");
    (coordinator as any).spawnWorker("cpu", segments);

    const restartedCpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(restartedCpuWorker).not.toBe(cpuWorker);
    expect(restartedCpuWorker.posted).toContainEqual({
      message: expectedCpuMessage,
      transfer: undefined,
    });
  });

  it("persists the machine CPU worker boot-device policy across CPU worker restarts", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = { vmRuntime: "machine" };

    const hdd = makeLocalDisk({
      id: "hdd1",
      name: "disk.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    });
    const cd = makeLocalDisk({
      id: "cd1",
      name: "install.iso",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "install.iso",
      sizeBytes: 2048,
      createdAtMs: 0,
    });

    coordinator.setBootDisks({ hddId: hdd.id, cdId: cd.id }, hdd, cd);

    (coordinator as any).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as any).workers.cpu;

    // Machine runtime boots from CD on the first run, then switches to HDD after the guest requests a reset.
    (coordinator as any).onWorkerMessage("cpu", cpuInfo.instanceId, { type: "machineCpu.bootDeviceSelected", bootDevice: "hdd" });

    // CPU worker restarts must preserve the policy so the guest boots from HDD even if the install ISO remains mounted.
    (coordinator as any).terminateWorker("cpu");
    (coordinator as any).spawnWorker("cpu", segments);

    const restartedCpuWorker = (coordinator as any).workers.cpu.worker as MockWorker;
    expect(restartedCpuWorker.posted).toContainEqual({
      message: {
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd1", cdId: "cd1" },
        hdd,
        cd,
        bootDevice: "hdd",
      } satisfies SetBootDisksMessage,
      transfer: undefined,
    });
  });

  it("resends boot disk selection to the IO worker when vmRuntime=legacy and the worker restarts", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).activeConfig = { vmRuntime: "legacy" };

    const hdd = makeLocalDisk({
      id: "hdd1",
      name: "disk.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    });
    const cd = makeLocalDisk({
      id: "cd1",
      name: "install.iso",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "install.iso",
      sizeBytes: 2048,
      createdAtMs: 0,
    });

    coordinator.setBootDisks({ hddId: hdd.id, cdId: cd.id }, hdd, cd);

    (coordinator as any).spawnWorker("io", segments);
    const ioWorker = (coordinator as any).workers.io.worker as MockWorker;

    const expectedIoMessage = {
      ...emptySetBootDisksMessage(),
      mounts: { hddId: "hdd1", cdId: "cd1" },
      hdd,
      cd,
      bootDevice: "cdrom",
    } satisfies SetBootDisksMessage;

    expect(ioWorker.posted).toContainEqual({
      message: expectedIoMessage,
      transfer: undefined,
    });

    (coordinator as any).terminateWorker("io");
    (coordinator as any).spawnWorker("io", segments);

    const restartedIoWorker = (coordinator as any).workers.io.worker as MockWorker;
    expect(restartedIoWorker).not.toBe(ioWorker);
    expect(restartedIoWorker.posted).toContainEqual({
      message: expectedIoMessage,
      transfer: undefined,
    });
  });

  it("does not schedule an automatic restart when boot disks are incompatible (machine runtime)", () => {
    vi.useFakeTimers();
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    // `scheduleFullRestart` is gated on `enableWorkers`; keep it enabled so this test would fail
    // if we accidentally reintroduced the restart loop.
    (coordinator as any).activeConfig = { enableWorkers: true, vmRuntime: "machine" };

    (coordinator as any).spawnWorker("cpu", segments);

    const cpuInfo = (coordinator as any).workers.cpu;
    expect(cpuInfo).toBeTruthy();

    (coordinator as any).onWorkerMessage("cpu", cpuInfo.instanceId, {
      type: MessageType.ERROR,
      role: "cpu",
      message: "machine runtime does not yet support remote streaming disks",
      code: ErrorCode.BOOT_DISKS_INCOMPATIBLE,
    });

    expect(coordinator.getVmState()).toBe("failed");
    expect(coordinator.getPendingFullRestart()).toBeNull();
    vi.useRealTimers();
  });
});
