import { describe, expect, it } from "vitest";

import { RuntimeDiskClient } from "./runtime_disk_client";

class MockWorker {
  calls: Array<{ msg: any; transfer?: any[] }> = [];
  lastMessage: any;
  lastTransfer: readonly any[] | undefined;
  throwOnNextPostMessage: boolean | undefined;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  onmessage: ((event: any) => void) | null = null;
  onerror: ((event: { message?: string }) => void) | null = null;
  onmessageerror: ((event: unknown) => void) | null = null;

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

    const secondCall = w.calls[1];
    if (!secondCall) throw new Error("Expected second postMessage call.");
    const secondMsg = secondCall.msg as { requestId: number; op: string; payload: { data: Uint8Array } };
    const secondTransfer = secondCall.transfer;
    expect(secondMsg.requestId).toBe(2);
    expect(secondMsg.op).toBe("write");

    const payloadData = secondMsg.payload.data as Uint8Array;
    expect(payloadData).not.toBe(data);
    expect(secondTransfer).toHaveLength(1);
    expect(secondTransfer?.[0]).toBe(payloadData.buffer);

    // Ensure we don't leak the failed request's pending entry.
    expect((client as unknown as { pending: Map<number, unknown> }).pending.size).toBe(1);

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

    const secondCall = w.calls[1];
    if (!secondCall) throw new Error("Expected second postMessage call.");
    const secondMsg = secondCall.msg as { requestId: number; op: string; payload: { state: Uint8Array } };
    const secondTransfer = secondCall.transfer;
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
    w.onerror?.({ message: "boom" });

    await expect(p).rejects.toThrow(/boom/);
    expect((client as unknown as { pending: Map<number, unknown> }).pending.size).toBe(0);
    client.close();
  });

  it("rejects pending requests when the worker message deserialization fails", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const p = client.openRemote("https://example.invalid/disk.img");
    w.onmessageerror?.({});

    await expect(p).rejects.toThrow(/deserialization failed/);
    expect((client as unknown as { pending: Map<number, unknown> }).pending.size).toBe(0);
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

describe("RuntimeDiskClient zero-copy ops", () => {
  it("serializes readInto() with SharedArrayBuffer dest and no transfer list", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const sab = new SharedArrayBuffer(16);
    const p = client.readInto(3, 4, 8, { sab, offsetBytes: 2 });

    expect(w.lastMessage.op).toBe("readInto");
    expect(w.lastMessage.payload).toEqual({
      handle: 3,
      lba: 4,
      byteLength: 8,
      dest: { sab, offsetBytes: 2 },
    });
    expect(w.lastTransfer).toHaveLength(0);

    w.emit({ type: "response", requestId: 1, ok: true, result: { ok: true } });
    await p;
    client.close();
  });

  it("serializes writeFrom() with SharedArrayBuffer src and no transfer list", async () => {
    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const sab = new SharedArrayBuffer(16);
    const p = client.writeFrom(7, 1, { sab, offsetBytes: 4, byteLength: 12 });

    expect(w.lastMessage.op).toBe("writeFrom");
    expect(w.lastMessage.payload).toEqual({
      handle: 7,
      lba: 1,
      src: { sab, offsetBytes: 4, byteLength: 12 },
    });
    expect(w.lastTransfer).toHaveLength(0);

    w.emit({ type: "response", requestId: 1, ok: true, result: { ok: true } });
    await p;
    client.close();
  });
});

describe("RuntimeDiskClient message validation", () => {
  it("does not accept response messages with type inherited from Object.prototype", async () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "type");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const w = new MockWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    try {
      Object.defineProperty(Object.prototype, "type", { value: "response", configurable: true });

      const p = client.openRemote("https://example.invalid/disk.img");

      // Missing `type` must not be satisfied by prototype pollution.
      w.emit({ requestId: 1, ok: true, result: { handle: 7, sectorSize: 512, capacityBytes: 0, readOnly: true } });

      const raced = await Promise.race([p.then(() => "resolved", () => "rejected"), Promise.resolve("pending")]);
      expect(raced).toBe("pending");

      // A valid response should still resolve the request.
      w.emit({ type: "response", requestId: 1, ok: true, result: { handle: 7, sectorSize: 512, capacityBytes: 0, readOnly: true } });
      await expect(p).resolves.toMatchObject({ handle: 7 });
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "type", existing);
      else Reflect.deleteProperty(Object.prototype, "type");
      client.close();
    }
  });
});
