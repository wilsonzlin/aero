import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import { WorkerCoordinator } from "./coordinator";
import { allocateSharedMemorySegments, createSharedMemoryViews } from "./shared_layout";

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

describe("runtime/coordinator", () => {
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

    await expect(promise).resolves.toEqual({
      enabled: true,
      records: 123,
      bytes: 4567,
      droppedRecords: 3,
      droppedBytes: 9,
    });
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

  it("rejects pending net trace status requests when the net worker is terminated", async () => {
    const coordinator = new WorkerCoordinator();
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    const shared = createSharedMemoryViews(segments);
    (coordinator as any).shared = shared;
    (coordinator as any).spawnWorker("net", segments);

    const promise = coordinator.getNetTraceStats(60_000);
    (coordinator as any).terminateWorker("net");

    await expect(promise).rejects.toThrow(/net worker restarted/i);
  });
});
