import { describe, expect, it } from "vitest";

import { openRingByKind } from "../ipc/ipc";
import { WorkerCoordinator } from "./coordinator";
import { createIoIpcSab, IO_IPC_NET_RX_QUEUE_KIND, IO_IPC_NET_TX_QUEUE_KIND } from "./shared_layout";

type PostedMessage = { message: any; transfer?: any[] };

class StubWorker {
  readonly posted: PostedMessage[] = [];
  readonly #listeners = new Set<(ev: MessageEvent) => void>();

  postMessage(message: any, transfer?: any[]): void {
    this.posted.push({ message, transfer });
  }

  addEventListener(type: string, listener: any): void {
    if (type !== "message") return;
    this.#listeners.add(listener as (ev: MessageEvent) => void);
  }

  removeEventListener(type: string, listener: any): void {
    if (type !== "message") return;
    this.#listeners.delete(listener as (ev: MessageEvent) => void);
  }

  emitMessage(data: any): void {
    const ev = { data } as MessageEvent;
    for (const listener of Array.from(this.#listeners)) {
      listener(ev);
    }
  }
}

async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

function installReadyWorkers(
  coordinator: WorkerCoordinator,
  workers: { cpu: StubWorker; io: StubWorker; net?: StubWorker },
): void {
  const ioIpc = createIoIpcSab();
  const map: Record<string, unknown> = {
    cpu: { role: "cpu", instanceId: 1, worker: workers.cpu as unknown as Worker, status: { state: "ready" } },
    io: { role: "io", instanceId: 1, worker: workers.io as unknown as Worker, status: { state: "ready" } },
  };
  if (workers.net) {
    map.net = { role: "net", instanceId: 1, worker: workers.net as unknown as Worker, status: { state: "ready" } };
  }
  (coordinator as any).workers = map;
  // Snapshot orchestration resets NET_TX/NET_RX via the shared `ioIpc` segment.
  (coordinator as any).shared = { segments: { ioIpc } };
}

describe("runtime/coordinator (worker VM snapshots)", () => {
  it("orchestrates snapshotSaveToOpfs pause → getCpuState → saveToOpfs → resume", async () => {
    const coordinator = new WorkerCoordinator();
    const cpu = new StubWorker();
    const io = new StubWorker();
    const net = new StubWorker();
    installReadyWorkers(coordinator, { cpu, io, net });

    const promise = coordinator.snapshotSaveToOpfs("state/test.snap");

    expect(cpu.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    expect(io.posted.length).toBe(0);
    expect(net.posted.length).toBe(0);

    cpu.emitMessage({ kind: "vm.snapshot.paused", requestId: cpu.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    // NET pause should not happen until *both* CPU + IO pause acks are received.
    expect(net.posted.length).toBe(0);

    expect(io.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    io.emitMessage({ kind: "vm.snapshot.paused", requestId: io.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(net.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    net.emitMessage({ kind: "vm.snapshot.paused", requestId: net.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(cpu.posted[1]?.message.kind).toBe("vm.snapshot.getCpuState");

    const cpuBuf = new ArrayBuffer(4);
    const mmuBuf = new ArrayBuffer(8);
    cpu.emitMessage({
      kind: "vm.snapshot.cpuState",
      requestId: cpu.posted[1]!.message.requestId,
      ok: true,
      cpu: cpuBuf,
      mmu: mmuBuf,
    });
    await flushMicrotasks();

    expect(io.posted[1]?.message.kind).toBe("vm.snapshot.saveToOpfs");
    expect(io.posted[1]?.message.path).toBe("state/test.snap");
    expect(io.posted[1]?.message.cpu).toBe(cpuBuf);
    expect(io.posted[1]?.message.mmu).toBe(mmuBuf);
    expect(io.posted[1]?.transfer).toEqual([cpuBuf, mmuBuf]);

    io.emitMessage({ kind: "vm.snapshot.saved", requestId: io.posted[1]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(cpu.posted[2]?.message.kind).toBe("vm.snapshot.resume");
    expect(io.posted[2]?.message.kind).toBe("vm.snapshot.resume");
    expect(net.posted.some((m) => m.message.kind === "vm.snapshot.resume")).toBe(false);

    cpu.emitMessage({ kind: "vm.snapshot.resumed", requestId: cpu.posted[2]!.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.resumed", requestId: io.posted[2]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(net.posted[1]?.message.kind).toBe("vm.snapshot.resume");
    net.emitMessage({ kind: "vm.snapshot.resumed", requestId: net.posted[1]!.message.requestId, ok: true });

    await expect(promise).resolves.toBeUndefined();
  });

  it("always resumes workers after snapshotSaveToOpfs errors", async () => {
    const coordinator = new WorkerCoordinator();
    const cpu = new StubWorker();
    const io = new StubWorker();
    const net = new StubWorker();
    installReadyWorkers(coordinator, { cpu, io, net });

    const promise = coordinator.snapshotSaveToOpfs("state/test.snap");

    cpu.emitMessage({ kind: "vm.snapshot.paused", requestId: cpu.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    io.emitMessage({ kind: "vm.snapshot.paused", requestId: io.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    net.emitMessage({ kind: "vm.snapshot.paused", requestId: net.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    const cpuBuf = new ArrayBuffer(4);
    const mmuBuf = new ArrayBuffer(8);
    cpu.emitMessage({
      kind: "vm.snapshot.cpuState",
      requestId: cpu.posted[1]!.message.requestId,
      ok: true,
      cpu: cpuBuf,
      mmu: mmuBuf,
    });
    await flushMicrotasks();

    io.emitMessage({
      kind: "vm.snapshot.saved",
      requestId: io.posted[1]!.message.requestId,
      ok: false,
      error: { name: "Error", message: "disk full" },
    });
    await flushMicrotasks();

    // Even though save failed, the coordinator must attempt to resume all paused workers.
    expect(cpu.posted.some((m) => m.message.kind === "vm.snapshot.resume")).toBe(true);
    expect(io.posted.some((m) => m.message.kind === "vm.snapshot.resume")).toBe(true);
    expect(net.posted.some((m) => m.message.kind === "vm.snapshot.resume")).toBe(false);

    const cpuResume = cpu.posted.find((m) => m.message.kind === "vm.snapshot.resume")!;
    const ioResume = io.posted.find((m) => m.message.kind === "vm.snapshot.resume")!;
    cpu.emitMessage({ kind: "vm.snapshot.resumed", requestId: cpuResume.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.resumed", requestId: ioResume.message.requestId, ok: true });
    await flushMicrotasks();

    // Net resumes after CPU/IO resume (best-effort ordering).
    const netResume = net.posted.find((m) => m.message.kind === "vm.snapshot.resume")!;
    net.emitMessage({ kind: "vm.snapshot.resumed", requestId: netResume.message.requestId, ok: true });

    await expect(promise).rejects.toThrow(/saveToOpfs/i);
  });

  it("orchestrates snapshotRestoreFromOpfs pause → restoreFromOpfs → setCpuState → resume", async () => {
    const coordinator = new WorkerCoordinator();
    const cpu = new StubWorker();
    const io = new StubWorker();
    const net = new StubWorker();
    installReadyWorkers(coordinator, { cpu, io, net });

    const shared = (coordinator as any).shared;
    const txRing = openRingByKind(shared.segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
    const rxRing = openRingByKind(shared.segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);
    txRing.tryPush(new Uint8Array([0xaa]));
    rxRing.tryPush(new Uint8Array([0xbb]));

    const promise = coordinator.snapshotRestoreFromOpfs("state/test.snap");

    expect(cpu.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    cpu.emitMessage({ kind: "vm.snapshot.paused", requestId: cpu.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(io.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    io.emitMessage({ kind: "vm.snapshot.paused", requestId: io.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(net.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    net.emitMessage({ kind: "vm.snapshot.paused", requestId: net.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    // Snapshot boundary must clear NET_TX/NET_RX rings (they are not part of the snapshot file).
    expect(txRing.tryPop()).toBeNull();
    expect(rxRing.tryPop()).toBeNull();

    expect(io.posted[1]?.message.kind).toBe("vm.snapshot.restoreFromOpfs");
    const cpuBuf = new ArrayBuffer(4);
    const mmuBuf = new ArrayBuffer(8);
    io.emitMessage({
      kind: "vm.snapshot.restored",
      requestId: io.posted[1]!.message.requestId,
      ok: true,
      cpu: cpuBuf,
      mmu: mmuBuf,
      devices: [],
    });
    await flushMicrotasks();

    expect(cpu.posted[1]?.message.kind).toBe("vm.snapshot.setCpuState");
    expect(cpu.posted[1]?.message.cpu).toBe(cpuBuf);
    expect(cpu.posted[1]?.message.mmu).toBe(mmuBuf);
    expect(cpu.posted[1]?.transfer).toEqual([cpuBuf, mmuBuf]);

    cpu.emitMessage({ kind: "vm.snapshot.cpuStateSet", requestId: cpu.posted[1]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(cpu.posted[2]?.message.kind).toBe("vm.snapshot.resume");
    expect(io.posted[2]?.message.kind).toBe("vm.snapshot.resume");
    expect(net.posted.some((m) => m.message.kind === "vm.snapshot.resume")).toBe(false);
    cpu.emitMessage({ kind: "vm.snapshot.resumed", requestId: cpu.posted[2]!.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.resumed", requestId: io.posted[2]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(net.posted[1]?.message.kind).toBe("vm.snapshot.resume");
    net.emitMessage({ kind: "vm.snapshot.resumed", requestId: net.posted[1]!.message.requestId, ok: true });

    await expect(promise).resolves.toBeUndefined();
  });

  it("orchestrates snapshotRestoreFromOpfs without a net worker (still resets NET rings)", async () => {
    const coordinator = new WorkerCoordinator();
    const cpu = new StubWorker();
    const io = new StubWorker();
    installReadyWorkers(coordinator, { cpu, io });

    const shared = (coordinator as any).shared;
    const txRing = openRingByKind(shared.segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
    const rxRing = openRingByKind(shared.segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);
    txRing.tryPush(new Uint8Array([0xaa]));
    rxRing.tryPush(new Uint8Array([0xbb]));

    const promise = coordinator.snapshotRestoreFromOpfs("state/test.snap");

    expect(cpu.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    cpu.emitMessage({ kind: "vm.snapshot.paused", requestId: cpu.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(io.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    io.emitMessage({ kind: "vm.snapshot.paused", requestId: io.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    // Snapshot boundary must clear NET_TX/NET_RX rings even when there is no net worker.
    expect(txRing.tryPop()).toBeNull();
    expect(rxRing.tryPop()).toBeNull();

    expect(io.posted[1]?.message.kind).toBe("vm.snapshot.restoreFromOpfs");
    const cpuBuf = new ArrayBuffer(4);
    const mmuBuf = new ArrayBuffer(8);
    io.emitMessage({
      kind: "vm.snapshot.restored",
      requestId: io.posted[1]!.message.requestId,
      ok: true,
      cpu: cpuBuf,
      mmu: mmuBuf,
      devices: [],
    });
    await flushMicrotasks();

    expect(cpu.posted[1]?.message.kind).toBe("vm.snapshot.setCpuState");
    expect(cpu.posted[1]?.message.cpu).toBe(cpuBuf);
    expect(cpu.posted[1]?.message.mmu).toBe(mmuBuf);
    expect(cpu.posted[1]?.transfer).toEqual([cpuBuf, mmuBuf]);

    cpu.emitMessage({ kind: "vm.snapshot.cpuStateSet", requestId: cpu.posted[1]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(cpu.posted[2]?.message.kind).toBe("vm.snapshot.resume");
    expect(io.posted[2]?.message.kind).toBe("vm.snapshot.resume");
    cpu.emitMessage({ kind: "vm.snapshot.resumed", requestId: cpu.posted[2]!.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.resumed", requestId: io.posted[2]!.message.requestId, ok: true });

    await expect(promise).resolves.toBeUndefined();
  });

  it("does not reset NET rings until IO is paused (regression: IO enqueue after drain)", async () => {
    const coordinator = new WorkerCoordinator();
    const cpu = new StubWorker();
    const io = new StubWorker();
    const net = new StubWorker();
    installReadyWorkers(coordinator, { cpu, io, net });

    const ioIpc = (coordinator as any).shared.segments.ioIpc as SharedArrayBuffer;
    const netTxRing = openRingByKind(ioIpc, IO_IPC_NET_TX_QUEUE_KIND);

    // Seed the ring with a frame that must not survive the restore.
    expect(netTxRing.tryPush(new Uint8Array([1, 2, 3, 4]))).toBe(true);

    const promise = coordinator.snapshotRestoreFromOpfs("state/test.snap");

    // CPU pause first.
    cpu.emitMessage({ kind: "vm.snapshot.paused", requestId: cpu.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    // IO pause is requested next, but before we ack it we simulate the IO worker enqueueing
    // into NET_TX. If the coordinator reset/drained NET_TX before IO was actually paused,
    // this frame would survive (stale) across the restore.
    expect(io.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    expect(netTxRing.tryPush(new Uint8Array([9, 9, 9]))).toBe(true);

    io.emitMessage({ kind: "vm.snapshot.paused", requestId: io.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    // NET pause last.
    net.emitMessage({ kind: "vm.snapshot.paused", requestId: net.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    // After all workers are paused, coordinator should have reset NET rings, clearing the
    // frame written while IO was still running.
    expect(netTxRing.tryPop()).toBeNull();

    // Finish restore flow.
    const cpuBuf = new ArrayBuffer(4);
    const mmuBuf = new ArrayBuffer(8);
    io.emitMessage({
      kind: "vm.snapshot.restored",
      requestId: io.posted[1]!.message.requestId,
      ok: true,
      cpu: cpuBuf,
      mmu: mmuBuf,
      devices: [],
    });
    await flushMicrotasks();

    cpu.emitMessage({ kind: "vm.snapshot.cpuStateSet", requestId: cpu.posted[1]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(cpu.posted[2]?.message.kind).toBe("vm.snapshot.resume");
    expect(io.posted[2]?.message.kind).toBe("vm.snapshot.resume");
    expect(net.posted.some((m) => m.message.kind === "vm.snapshot.resume")).toBe(false);
    cpu.emitMessage({ kind: "vm.snapshot.resumed", requestId: cpu.posted[2]!.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.resumed", requestId: io.posted[2]!.message.requestId, ok: true });
    await flushMicrotasks();

    const netResume = net.posted.find((m) => m.message.kind === "vm.snapshot.resume")!;
    net.emitMessage({ kind: "vm.snapshot.resumed", requestId: netResume.message.requestId, ok: true });

    await expect(promise).resolves.toBeUndefined();
  });
});
