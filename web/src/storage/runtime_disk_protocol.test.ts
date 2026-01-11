import { describe, expect, it } from "vitest";

import { RuntimeDiskClient } from "./runtime_disk_client";
import type { DiskImageMetadata } from "./metadata";
import type { DiskOpenSpec } from "./runtime_disk_protocol";

class StubWorker {
  lastMessage: any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  onmessage: ((event: any) => void) | null = null;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  postMessage(msg: any, _transfer?: any[]): void {
    this.lastMessage = msg;
  }

  terminate(): void {
    // no-op
  }

  emit(data: unknown): void {
    this.onmessage?.({ data });
  }
}

describe("runtime disk worker protocol", () => {
  it("serializes local open() as DiskOpenSpec(kind=local)", async () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "disk1",
      name: "disk1",
      backend: "idb",
      kind: "hdd",
      format: "raw",
      fileName: "disk1.img",
      sizeBytes: 2 * 1024 * 1024,
      createdAtMs: 0,
    };

    const w = new StubWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const openPromise = client.open(meta, { mode: "cow", overlayBlockSizeBytes: 1234 });
    expect(w.lastMessage.op).toBe("open");
    expect(w.lastMessage.payload).toEqual({
      spec: { kind: "local", meta },
      mode: "cow",
      overlayBlockSizeBytes: 1234,
    });

    w.emit({
      type: "response",
      requestId: 1,
      ok: true,
      result: { handle: 7, sectorSize: 512, capacityBytes: meta.sizeBytes, readOnly: false },
    });

    const opened = await openPromise;
    expect(opened.handle).toBe(7);
    expect(opened.capacityBytes).toBe(meta.sizeBytes);
    client.close();
  });

  it("serializes remote open() as DiskOpenSpec(kind=remote)", async () => {
    const spec: DiskOpenSpec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "hdd",
        format: "raw",
        url: "https://example.invalid/disk.img?token=secret",
        credentials: "include",
        cacheKey: "win7-sp1-x64.sha256-deadbeef",
      },
    };

    const w = new StubWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const openPromise = client.open(spec);
    expect(w.lastMessage.op).toBe("open");
    expect(w.lastMessage.payload.spec.kind).toBe("remote");
    expect(w.lastMessage.payload.spec.remote.cacheKey).toBe(spec.remote.cacheKey);
    expect(w.lastMessage.payload.spec.remote.url).toBe(spec.remote.url);

    w.emit({
      type: "response",
      requestId: 1,
      ok: true,
      result: { handle: 1, sectorSize: 512, capacityBytes: 4096, readOnly: true },
    });

    const opened = await openPromise;
    expect(opened.readOnly).toBe(true);
    client.close();
  });
});
