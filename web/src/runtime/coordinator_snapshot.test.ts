import { describe, expect, it } from "vitest";

import { WorkerCoordinator } from "./coordinator";

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

function installReadyWorkers(coordinator: WorkerCoordinator, cpu: StubWorker, io: StubWorker): void {
  (coordinator as any).workers = {
    cpu: { role: "cpu", instanceId: 1, worker: cpu as unknown as Worker, status: { state: "ready" } },
    io: { role: "io", instanceId: 1, worker: io as unknown as Worker, status: { state: "ready" } },
  };
}

describe("runtime/coordinator (worker VM snapshots)", () => {
  it("orchestrates snapshotSaveToOpfs pause → getCpuState → saveToOpfs → resume", async () => {
    const coordinator = new WorkerCoordinator();
    const cpu = new StubWorker();
    const io = new StubWorker();
    const net = new StubWorker();
    (coordinator as any).workers = {
      cpu: { role: "cpu", instanceId: 1, worker: cpu as unknown as Worker, status: { state: "ready" } },
      io: { role: "io", instanceId: 1, worker: io as unknown as Worker, status: { state: "ready" } },
      net: { role: "net", instanceId: 1, worker: net as unknown as Worker, status: { state: "ready" } },
    };

    const promise = coordinator.snapshotSaveToOpfs("state/test.snap");

    expect(cpu.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    expect(io.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    expect(net.posted.length).toBe(0);

    cpu.emitMessage({ kind: "vm.snapshot.paused", requestId: cpu.posted[0]!.message.requestId, ok: true });
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
    expect(net.posted[1]?.message.kind).toBe("vm.snapshot.resume");

    cpu.emitMessage({ kind: "vm.snapshot.resumed", requestId: cpu.posted[2]!.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.resumed", requestId: io.posted[2]!.message.requestId, ok: true });
    net.emitMessage({ kind: "vm.snapshot.resumed", requestId: net.posted[1]!.message.requestId, ok: true });

    await expect(promise).resolves.toBeUndefined();
  });

  it("always resumes workers after snapshotSaveToOpfs errors", async () => {
    const coordinator = new WorkerCoordinator();
    const cpu = new StubWorker();
    const io = new StubWorker();
    installReadyWorkers(coordinator, cpu, io);

    const promise = coordinator.snapshotSaveToOpfs("state/test.snap");

    cpu.emitMessage({ kind: "vm.snapshot.paused", requestId: cpu.posted[0]!.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.paused", requestId: io.posted[0]!.message.requestId, ok: true });
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

    // Even though save failed, the coordinator must attempt to resume both workers.
    expect(cpu.posted.some((m) => m.message.kind === "vm.snapshot.resume")).toBe(true);
    expect(io.posted.some((m) => m.message.kind === "vm.snapshot.resume")).toBe(true);

    const cpuResume = cpu.posted.find((m) => m.message.kind === "vm.snapshot.resume")!;
    const ioResume = io.posted.find((m) => m.message.kind === "vm.snapshot.resume")!;
    cpu.emitMessage({ kind: "vm.snapshot.resumed", requestId: cpuResume.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.resumed", requestId: ioResume.message.requestId, ok: true });

    await expect(promise).rejects.toThrow(/saveToOpfs/i);
  });

  it("orchestrates snapshotRestoreFromOpfs pause → restoreFromOpfs → setCpuState → resume", async () => {
    const coordinator = new WorkerCoordinator();
    const cpu = new StubWorker();
    const io = new StubWorker();
    const net = new StubWorker();
    (coordinator as any).workers = {
      cpu: { role: "cpu", instanceId: 1, worker: cpu as unknown as Worker, status: { state: "ready" } },
      io: { role: "io", instanceId: 1, worker: io as unknown as Worker, status: { state: "ready" } },
      net: { role: "net", instanceId: 1, worker: net as unknown as Worker, status: { state: "ready" } },
    };

    const promise = coordinator.snapshotRestoreFromOpfs("state/test.snap");

    expect(cpu.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    expect(io.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    cpu.emitMessage({ kind: "vm.snapshot.paused", requestId: cpu.posted[0]!.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.paused", requestId: io.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

    expect(net.posted[0]?.message.kind).toBe("vm.snapshot.pause");
    net.emitMessage({ kind: "vm.snapshot.paused", requestId: net.posted[0]!.message.requestId, ok: true });
    await flushMicrotasks();

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
    expect(net.posted[1]?.message.kind).toBe("vm.snapshot.resume");
    cpu.emitMessage({ kind: "vm.snapshot.resumed", requestId: cpu.posted[2]!.message.requestId, ok: true });
    io.emitMessage({ kind: "vm.snapshot.resumed", requestId: io.posted[2]!.message.requestId, ok: true });
    net.emitMessage({ kind: "vm.snapshot.resumed", requestId: net.posted[1]!.message.requestId, ok: true });

    await expect(promise).resolves.toBeUndefined();
  });
});
