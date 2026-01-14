import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import type { DiskImageMetadata } from "../storage/metadata";
import { WorkerCoordinator } from "./coordinator";
import { emptySetBootDisksMessage, type SetBootDisksMessage } from "./boot_disks_protocol";
import { createSharedMemoryViews } from "./shared_layout";
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
  const originalWorkerDescriptor = Object.getOwnPropertyDescriptor(globalThis, "Worker");
  const globalWithWorker = globalThis as unknown as { Worker?: unknown };

  beforeEach(() => {
    globalWithWorker.Worker = MockWorker;
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

  function makeLocalDisk(meta: Omit<Extract<DiskImageMetadata, { source: "local" }>, "source">): DiskImageMetadata {
    return { ...meta, source: "local" };
  }

  function allocateTestSegments() {
    return allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
  }

  type CoordinatorTestHarness = {
    shared: unknown;
    activeConfig?: Record<string, unknown>;
    workers: Record<string, { instanceId: number; worker: unknown }>;
    spawnWorker: (role: string, segments: unknown) => void;
    terminateWorker: (role: string) => void;
    onWorkerMessage: (role: string, instanceId: number, message: unknown) => void;
  };

  it("resends boot disk selection to the CPU worker when vmRuntime=machine and the worker restarts", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    // Manually wire shared memory so we can spawn workers without invoking `start()`.
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

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
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

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
    (coordinator as unknown as CoordinatorTestHarness).terminateWorker("cpu");
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);

    const restartedCpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
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
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

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

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    // Machine runtime boots from CD on the first run, then switches to HDD after the guest requests a reset.
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, { type: "machineCpu.bootDeviceSelected", bootDevice: "hdd" });

    // CPU worker restarts must preserve the policy so the guest boots from HDD even if the install ISO remains mounted.
    (coordinator as unknown as CoordinatorTestHarness).terminateWorker("cpu");
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);

    const restartedCpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
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

  it("tracks the machine CPU worker's active boot device reports", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    expect(coordinator.getMachineCpuActiveBootDevice()).toBe(null);

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, { type: "machineCpu.bootDeviceActive", bootDevice: "cdrom" });
    expect(coordinator.getMachineCpuActiveBootDevice()).toBe("cdrom");

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, { type: "machineCpu.bootDeviceActive", bootDevice: "hdd" });
    expect(coordinator.getMachineCpuActiveBootDevice()).toBe("hdd");
  });

  it("ignores inherited machine CPU active boot device reports", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    expect(coordinator.getMachineCpuActiveBootDevice()).toBe(null);

    // Inherited bootDevice should be ignored.
    const msg = Object.create({ bootDevice: "cdrom" });
    msg.type = "machineCpu.bootDeviceActive";
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, msg);
    expect(coordinator.getMachineCpuActiveBootDevice()).toBe(null);

    // Inherited type tag should be ignored.
    const msg2 = Object.create({ type: "machineCpu.bootDeviceActive" });
    msg2.bootDevice = "cdrom";
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, msg2);
    expect(coordinator.getMachineCpuActiveBootDevice()).toBe(null);
  });

  it("tracks the machine CPU worker's boot config reports", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    expect(coordinator.getMachineCpuBootConfig()).toBe(null);

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      type: "machineCpu.bootConfig",
      bootDrive: 0x80,
      cdBootDrive: 0xe0,
      bootFromCdIfPresent: true,
    });
    expect(coordinator.getMachineCpuBootConfig()).toEqual({ bootDrive: 0x80, cdBootDrive: 0xe0, bootFromCdIfPresent: true });
  });

  it("ignores invalid machine CPU boot config reports", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    expect(coordinator.getMachineCpuBootConfig()).toBe(null);

    // Out-of-range bootDrive and non-boolean bootFromCdIfPresent should be rejected.
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      type: "machineCpu.bootConfig",
      bootDrive: 0x180,
      cdBootDrive: 0xe0,
      bootFromCdIfPresent: 1,
    });
    expect(coordinator.getMachineCpuBootConfig()).toBe(null);
  });

  it("ignores inherited machine CPU boot config reports", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    const msg = Object.create({ bootDrive: 0x80, cdBootDrive: 0xe0, bootFromCdIfPresent: true });
    msg.type = "machineCpu.bootConfig";
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, msg);

    expect(coordinator.getMachineCpuBootConfig()).toBe(null);
  });

  it("ignores machine CPU boot config reports when the type tag is inherited", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    const msg = Object.create({ type: "machineCpu.bootConfig" });
    msg.bootDrive = 0x80;
    msg.cdBootDrive = 0xe0;
    msg.bootFromCdIfPresent = true;
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, msg);

    expect(coordinator.getMachineCpuBootConfig()).toBe(null);
  });

  it("clears the active boot device when the VM is stopped", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine", enableWorkers: true };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, { type: "machineCpu.bootDeviceActive", bootDevice: "cdrom" });
    expect(coordinator.getMachineCpuActiveBootDevice()).toBe("cdrom");

    coordinator.stop();
    expect(coordinator.getMachineCpuActiveBootDevice()).toBe(null);
  });

  it("clears the machine CPU boot config when the VM is stopped", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine", enableWorkers: true };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      type: "machineCpu.bootConfig",
      bootDrive: 0x80,
      cdBootDrive: 0xe0,
      bootFromCdIfPresent: true,
    });
    expect(coordinator.getMachineCpuBootConfig()).toEqual({ bootDrive: 0x80, cdBootDrive: 0xe0, bootFromCdIfPresent: true });

    coordinator.stop();
    expect(coordinator.getMachineCpuBootConfig()).toBe(null);
  });

  it("clears machine CPU boot debug state when the CPU worker is terminated", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      type: "machineCpu.bootDeviceActive",
      bootDevice: "cdrom",
    });
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      type: "machineCpu.bootConfig",
      bootDrive: 0x80,
      cdBootDrive: 0xe0,
      bootFromCdIfPresent: true,
    });

    expect(coordinator.getMachineCpuActiveBootDevice()).toBe("cdrom");
    expect(coordinator.getMachineCpuBootConfig()).toEqual({ bootDrive: 0x80, cdBootDrive: 0xe0, bootFromCdIfPresent: true });

    (coordinator as unknown as CoordinatorTestHarness).terminateWorker("cpu");

    expect(coordinator.getMachineCpuActiveBootDevice()).toBe(null);
    expect(coordinator.getMachineCpuBootConfig()).toBe(null);
  });

  it("clears machine CPU boot debug state when the boot disk selection changes (machine runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

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
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      type: "machineCpu.bootDeviceActive",
      bootDevice: "cdrom",
    });
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
      type: "machineCpu.bootConfig",
      bootDrive: 0x80,
      cdBootDrive: 0xe0,
      bootFromCdIfPresent: true,
    });
    expect(coordinator.getMachineCpuActiveBootDevice()).toBe("cdrom");
    expect(coordinator.getMachineCpuBootConfig()).toEqual({ bootDrive: 0x80, cdBootDrive: 0xe0, bootFromCdIfPresent: true });

    // Detach the CD (disk selection change triggers machine runtime disk reattachment/reset).
    coordinator.setBootDisks({ hddId: hdd.id }, hdd, null);

    expect(coordinator.getMachineCpuActiveBootDevice()).toBe(null);
    expect(coordinator.getMachineCpuBootConfig()).toBe(null);
  });

  it("switches boot-device policy to HDD when the guest requests a reset and both HDD+CD are present (machine runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { enableWorkers: true, vmRuntime: "machine" };

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
    expect(coordinator.getBootDisks()?.bootDevice).toBe("cdrom");

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as { workers: Record<string, unknown> }).workers.cpu as unknown as Record<string, unknown>;

    // Avoid triggering a full VM reset in this unit test; we only care about the boot-device
    // policy rewrite that happens before reset is invoked.
    const resetSpy = vi.spyOn(coordinator, "reset").mockImplementation(() => {});

    (coordinator as unknown as { handleEvent: (info: unknown, evt: unknown) => void }).handleEvent(cpuInfo, { kind: "resetRequest" });

    expect(resetSpy).toHaveBeenCalledWith("resetRequest");
    expect(coordinator.getBootDisks()?.bootDevice).toBe("hdd");
  });

  it("preserves bootDevice when setBootDisks is called with unchanged disk IDs (machine runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

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

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;
    const cpuWorker = cpuInfo.worker as MockWorker;

    // Simulate the CPU worker switching to HDD boot after the guest rebooted.
    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, { type: "machineCpu.bootDeviceSelected", bootDevice: "hdd" });

    // DiskManager may re-apply the same selection (e.g. after refresh). This must not reset bootDevice back to cdrom.
    const postedBefore = cpuWorker.posted.length;
    coordinator.setBootDisks({ hddId: hdd.id, cdId: cd.id }, hdd, cd);

    expect(coordinator.getBootDisks()?.bootDevice).toBe("hdd");

    // The coordinator may treat this as a no-op (skip re-broadcast), but if it does send another
    // `setBootDisks` message it must preserve the persisted bootDevice policy.
    const newBootDisksMsgs = cpuWorker.posted
      .slice(postedBefore)
      .filter((p) => (p.message as { type?: unknown }).type === "setBootDisks");
    for (const entry of newBootDisksMsgs) {
      expect(entry).toEqual({
        message: {
          ...emptySetBootDisksMessage(),
          mounts: { hddId: "hdd1", cdId: "cd1" },
          hdd,
          cd,
          bootDevice: "hdd",
        } satisfies SetBootDisksMessage,
        transfer: undefined,
      });
    }
  });

  it("preserves disk metadata when mounts are unchanged but setBootDisks is called with null metadata (machine runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

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
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const postedBefore = cpuWorker.posted.length;

    // Simulate a DiskManager refresh where mounts still reference the same disk IDs but metadata is missing/late-loaded.
    coordinator.setBootDisks({ hddId: hdd.id, cdId: cd.id }, null, null);

    expect(coordinator.getBootDisks()?.hdd).toBe(hdd);
    expect(coordinator.getBootDisks()?.cd).toBe(cd);

    // No new boot-disks message should be broadcast (it would otherwise detach disks in the machine CPU worker).
    const newBootDisksMsgs = cpuWorker.posted
      .slice(postedBefore)
      .filter((p) => (p.message as { type?: unknown }).type === "setBootDisks");
    expect(newBootDisksMsgs).toHaveLength(0);
  });

  it("updates cached disk metadata without rebroadcasting when mounts are unchanged (machine runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "machine" };

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
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);
    const cpuWorker = (coordinator as unknown as CoordinatorTestHarness).workers.cpu.worker as MockWorker;
    const postedBefore = cpuWorker.posted.length;

    // Rename the disk (metadata changes, selection does not).
    const renamedHdd = { ...hdd, name: "renamed.img" } satisfies DiskImageMetadata;
    coordinator.setBootDisks({ hddId: hdd.id, cdId: cd.id }, renamedHdd, cd);

    // The coordinator should store the updated metadata in its cached boot-disks selection so UI/debug
    // panels reflect it, but it must not rebroadcast to workers (which would cause reattachment/reset).
    expect(coordinator.getBootDisks()?.hdd?.name).toBe("renamed.img");

    const newBootDisksMsgs = cpuWorker.posted
      .slice(postedBefore)
      .filter((p) => (p.message as { type?: unknown }).type === "setBootDisks");
    expect(newBootDisksMsgs).toHaveLength(0);
  });

  it("preserves disk metadata when mounts are unchanged but setBootDisks is called with null metadata (legacy runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "legacy" };

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
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;
    const postedBefore = ioWorker.posted.length;

    // Simulate a refresh where mounts are stable but disk metadata is missing/late-loaded.
    coordinator.setBootDisks({ hddId: hdd.id, cdId: cd.id }, null, null);

    expect(coordinator.getBootDisks()?.hdd).toBe(hdd);
    expect(coordinator.getBootDisks()?.cd).toBe(cd);

    const newBootDisksMsgs = ioWorker.posted
      .slice(postedBefore)
      .filter((p) => (p.message as { type?: unknown }).type === "setBootDisks");
    expect(newBootDisksMsgs).toHaveLength(0);
  });

  it("updates cached disk metadata without rebroadcasting when mounts are unchanged (legacy runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "legacy" };

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

    coordinator.setBootDisks({ hddId: hdd.id }, hdd, null);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;
    const postedBefore = ioWorker.posted.length;

    // Metadata changes (rename) should be reflected in cached bootDisks but must not trigger a remount.
    const renamedHdd = { ...hdd, name: "renamed.img" } satisfies DiskImageMetadata;
    coordinator.setBootDisks({ hddId: hdd.id }, renamedHdd, null);

    expect(coordinator.getBootDisks()?.hdd?.name).toBe("renamed.img");

    const newBootDisksMsgs = ioWorker.posted
      .slice(postedBefore)
      .filter((p) => (p.message as { type?: unknown }).type === "setBootDisks");
    expect(newBootDisksMsgs).toHaveLength(0);
  });

  it("rebroadcasts boot disks when open-relevant metadata changes (legacy runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "legacy" };

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

    coordinator.setBootDisks({ hddId: hdd.id }, hdd, null);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;
    const postedBefore = ioWorker.posted.length;

    // Disk resize changes the open contract (`sizeBytes` is validated by the runtime disk worker).
    // This should trigger a re-broadcast so the IO worker reopens the disk with the updated size.
    const resizedHdd = { ...hdd, sizeBytes: 2048 } satisfies DiskImageMetadata;
    coordinator.setBootDisks({ hddId: hdd.id }, resizedHdd, null);

    const newBootDisksMsgs = ioWorker.posted
      .slice(postedBefore)
      .filter((p) => (p.message as { type?: unknown }).type === "setBootDisks");
    expect(newBootDisksMsgs).toEqual([
      {
        message: {
          ...emptySetBootDisksMessage(),
          mounts: { hddId: "hdd1" },
          hdd: resizedHdd,
          cd: null,
          bootDevice: "hdd",
        } satisfies SetBootDisksMessage,
        transfer: undefined,
      },
    ]);
  });

  it("rebroadcasts boot disks when remote-disk open metadata changes (legacy runtime)", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "legacy" };

    const remoteHdd: DiskImageMetadata = {
      source: "remote",
      id: "hdd1",
      name: "remote-disk",
      kind: "hdd",
      format: "raw",
      sizeBytes: 1024,
      createdAtMs: 0,
      remote: {
        imageId: "img_1",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.invalid/v1/images/img_1/data" },
        validator: { etag: "\"etag-a\"" },
      },
      cache: {
        chunkSizeBytes: 1024 * 1024,
        backend: "idb",
        fileName: "cache.bin",
        overlayFileName: "overlay.bin",
        overlayBlockSizeBytes: 1024 * 1024,
      },
    };

    coordinator.setBootDisks({ hddId: remoteHdd.id }, remoteHdd, null);
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;
    const postedBefore = ioWorker.posted.length;

    // Changing the remote validator changes the open spec (cache binding), so the IO worker must
    // be reconfigured/reopened with the new validator.
    const updatedRemoteHdd: DiskImageMetadata = {
      ...remoteHdd,
      remote: {
        ...remoteHdd.remote,
        validator: { etag: "\"etag-b\"" },
      },
    };
    coordinator.setBootDisks({ hddId: remoteHdd.id }, updatedRemoteHdd, null);

    const newBootDisksMsgs = ioWorker.posted
      .slice(postedBefore)
      .filter((p) => (p.message as { type?: unknown }).type === "setBootDisks");
    expect(newBootDisksMsgs).toEqual([
      {
        message: {
          ...emptySetBootDisksMessage(),
          mounts: { hddId: "hdd1" },
          hdd: updatedRemoteHdd,
          cd: null,
          bootDevice: "hdd",
        } satisfies SetBootDisksMessage,
        transfer: undefined,
      },
    ]);
  });

  it("resends boot disk selection to the IO worker when vmRuntime=legacy and the worker restarts", () => {
    const coordinator = new WorkerCoordinator();

    const segments = allocateTestSegments();
    const shared = createSharedMemoryViews(segments);
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { vmRuntime: "legacy" };

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

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);
    const ioWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;

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

    (coordinator as unknown as CoordinatorTestHarness).terminateWorker("io");
    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("io", segments);

    const restartedIoWorker = (coordinator as unknown as CoordinatorTestHarness).workers.io.worker as MockWorker;
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
    (coordinator as unknown as CoordinatorTestHarness).shared = shared;
    // `scheduleFullRestart` is gated on `enableWorkers`; keep it enabled so this test would fail
    // if we accidentally reintroduced the restart loop.
    (coordinator as unknown as CoordinatorTestHarness).activeConfig = { enableWorkers: true, vmRuntime: "machine" };

    (coordinator as unknown as CoordinatorTestHarness).spawnWorker("cpu", segments);

    const cpuInfo = (coordinator as unknown as CoordinatorTestHarness).workers.cpu;
    expect(cpuInfo).toBeTruthy();

    (coordinator as unknown as CoordinatorTestHarness).onWorkerMessage("cpu", cpuInfo.instanceId, {
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
