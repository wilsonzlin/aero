import { describe, expect, it } from "vitest";

import { RuntimeDiskClient } from "./runtime_disk_client";

class MockWorker {
  calls: Array<{ msg: any; transfer?: any[] }> = [];
  lastMessage: any;
  lastTransfer: readonly any[] | undefined;
  throwOnNextPostMessage: boolean | undefined;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  onmessage: ((event: any) => void) | null = null;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  postMessage(msg: any, transfer?: any[]): void {
    this.calls.push({ msg, transfer });
    if (this.throwOnNextPostMessage) {
      this.throwOnNextPostMessage = false;
      throw new DOMException("Cannot transfer object of unsupported type.", "DataCloneError");
    }
    this.lastMessage = msg;
    this.lastTransfer = transfer;
  }

  terminate(): void {
    // no-op
  }

  emit(data: unknown): void {
    this.onmessage?.({ data });
  }
}

describe("RuntimeDiskClient.write", () => {
  it("transfers standalone ArrayBuffer-backed Uint8Array without copying", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const buffer = new ArrayBuffer(4);
    const data = new Uint8Array(buffer);
    data.set([1, 2, 3, 4]);

    const p = client.write(1, 2, data);

    expect(w.lastMessage.op).toBe("write");
    expect(w.lastMessage.payload.handle).toBe(1);
    expect(w.lastMessage.payload.lba).toBe(2);
    expect(w.lastMessage.payload.data).toBe(data);
    expect(w.lastTransfer).toHaveLength(1);
    expect(w.lastTransfer?.[0]).toBe(buffer);

    w.emit({ type: "response", requestId: 1, ok: true, result: { ok: true } });
    await p;
    client.close();
  });

  it("copies SharedArrayBuffer-backed Uint8Array before transferring", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const shared = new SharedArrayBuffer(4);
    const data = new Uint8Array(shared);
    data.set([5, 6, 7, 8]);

    const p = client.write(1, 0, data);

    const payloadData = w.lastMessage.payload.data as Uint8Array;
    expect(payloadData).not.toBe(data);
    expect(payloadData.buffer).not.toBe(shared);
    expect(payloadData.buffer instanceof ArrayBuffer).toBe(true);
    expect(w.lastTransfer).toHaveLength(1);
    expect(w.lastTransfer?.[0]).toBe(payloadData.buffer);

    w.emit({ type: "response", requestId: 1, ok: true, result: { ok: true } });
    await p;
    client.close();
  });

  it("copies ArrayBuffer-backed subrange views before transferring", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const buffer = new ArrayBuffer(8);
    const full = new Uint8Array(buffer);
    for (let i = 0; i < full.length; i++) full[i] = i + 1;

    const sub = full.subarray(2, 6);
    expect(sub.byteOffset).toBe(2);
    expect(sub.byteLength).toBe(4);

    const p = client.write(1, 0, sub);

    const payloadData = w.lastMessage.payload.data as Uint8Array;
    expect(payloadData).not.toBe(sub);
    expect(payloadData.buffer).not.toBe(buffer);
    expect(Array.from(payloadData)).toEqual(Array.from(sub));
    expect(w.lastTransfer).toHaveLength(1);
    expect(w.lastTransfer?.[0]).toBe(payloadData.buffer);

    w.emit({ type: "response", requestId: 1, ok: true, result: { ok: true } });
    await p;
    client.close();
  });

  it("falls back to copying when direct-transfer postMessage throws DataCloneError", async () => {
    const w = new MockWorker();
    w.throwOnNextPostMessage = true;
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const buffer = new ArrayBuffer(4);
    const data = new Uint8Array(buffer);
    data.set([1, 2, 3, 4]);

    const p = client.write(1, 2, data);
    // Allow the `.catch()` fallback to run.
    await Promise.resolve();
    await Promise.resolve();

    expect(w.calls).toHaveLength(2);
    expect(w.calls[0]?.transfer).toHaveLength(1);
    expect(w.calls[0]?.transfer?.[0]).toBe(buffer);

    const secondMsg = w.calls[1]?.msg as any;
    const secondTransfer = w.calls[1]?.transfer;
    expect(secondMsg.requestId).toBe(2);
    expect(secondMsg.op).toBe("write");

    const payloadData = secondMsg.payload.data as Uint8Array;
    expect(payloadData).not.toBe(data);
    expect(secondTransfer).toHaveLength(1);
    expect(secondTransfer?.[0]).toBe(payloadData.buffer);

    // Ensure we don't leak the failed request's pending entry.
    expect(((client as any).pending as Map<number, unknown>).size).toBe(1);

    w.emit({ type: "response", requestId: 2, ok: true, result: { ok: true } });
    await p;
    client.close();
  });
});

describe("RuntimeDiskClient.restoreFromSnapshot", () => {
  it("transfers standalone ArrayBuffer-backed snapshots without copying", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const buffer = new ArrayBuffer(4);
    const state = new Uint8Array(buffer);
    state.set([1, 2, 3, 4]);

    const p = client.restoreFromSnapshot(state);

    expect(w.lastMessage.op).toBe("restoreFromSnapshot");
    expect(w.lastMessage.payload.state).toBe(state);
    expect(w.lastTransfer).toHaveLength(1);
    expect(w.lastTransfer?.[0]).toBe(buffer);

    w.emit({ type: "response", requestId: 1, ok: true, result: { ok: true } });
    await p;
    client.close();
  });

  it("copies SharedArrayBuffer-backed snapshots before transferring", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const shared = new SharedArrayBuffer(4);
    const state = new Uint8Array(shared);
    state.set([5, 6, 7, 8]);

    const p = client.restoreFromSnapshot(state);

    const payloadState = w.lastMessage.payload.state as Uint8Array;
    expect(payloadState).not.toBe(state);
    expect(payloadState.buffer).not.toBe(shared);
    expect(payloadState.buffer instanceof ArrayBuffer).toBe(true);
    expect(w.lastTransfer).toHaveLength(1);
    expect(w.lastTransfer?.[0]).toBe(payloadState.buffer);

    w.emit({ type: "response", requestId: 1, ok: true, result: { ok: true } });
    await p;
    client.close();
  });

  it("falls back to copying when direct-transfer restoreFromSnapshot throws DataCloneError", async () => {
    const w = new MockWorker();
    w.throwOnNextPostMessage = true;
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const buffer = new ArrayBuffer(4);
    const state = new Uint8Array(buffer);
    state.set([1, 2, 3, 4]);

    const p = client.restoreFromSnapshot(state);
    await Promise.resolve();
    await Promise.resolve();

    expect(w.calls).toHaveLength(2);
    expect(w.calls[0]?.transfer).toHaveLength(1);
    expect(w.calls[0]?.transfer?.[0]).toBe(buffer);

    const secondMsg = w.calls[1]?.msg as any;
    const secondTransfer = w.calls[1]?.transfer;
    expect(secondMsg.requestId).toBe(2);
    expect(secondMsg.op).toBe("restoreFromSnapshot");

    const payloadState = secondMsg.payload.state as Uint8Array;
    expect(payloadState).not.toBe(state);
    expect(secondTransfer).toHaveLength(1);
    expect(secondTransfer?.[0]).toBe(payloadState.buffer);

    w.emit({ type: "response", requestId: 2, ok: true, result: { ok: true } });
    await p;
    client.close();
  });
});

describe("RuntimeDiskClient error handling", () => {
  it("rejects pending requests when the worker errors", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const p = client.openRemote("https://example.invalid/disk.img");
    (w as any).onerror?.({ message: "boom" });

    await expect(p).rejects.toThrow(/boom/);
    expect(((client as any).pending as Map<number, unknown>).size).toBe(0);
    client.close();
  });

  it("rejects pending requests when the worker message deserialization fails", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const p = client.openRemote("https://example.invalid/disk.img");
    (w as any).onmessageerror?.({});

    await expect(p).rejects.toThrow(/deserialization failed/);
    expect(((client as any).pending as Map<number, unknown>).size).toBe(0);
    client.close();
  });

  it("rejects pending requests when the client is closed", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const p = client.openRemote("https://example.invalid/disk.img");
    client.close();

    await expect(p).rejects.toThrow(/closed/);
  });
});
