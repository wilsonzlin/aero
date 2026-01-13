import { describe, expect, it } from "vitest";

import { RuntimeDiskClient } from "./runtime_disk_client";

class MockWorker {
  lastMessage: any;
  lastTransfer: readonly any[] | undefined;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  onmessage: ((event: any) => void) | null = null;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  postMessage(msg: any, transfer?: any[]): void {
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
});
