import { describe, expect, it } from "vitest";

import { RuntimeDiskClient } from "./runtime_disk_client";
import { RuntimeDiskWorker, type OpenDiskFn } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";
import { normalizeDiskOpenSpec, type DiskOpenSpec } from "./runtime_disk_protocol";

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
  const dummyLocalMeta: DiskImageMetadata = {
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

  it("does not treat inherited kind as a DiskOpenSpec (prototype pollution)", () => {
    const kindExisting = Object.getOwnPropertyDescriptor(Object.prototype, "kind");
    if (kindExisting && kindExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "kind", { value: "remote", configurable: true, writable: true });
      const normalized = normalizeDiskOpenSpec({} as any);
      expect(normalized.kind).toBe("local");
    } finally {
      if (kindExisting) Object.defineProperty(Object.prototype, "kind", kindExisting);
      else delete (Object.prototype as any).kind;
    }
  });

  it("serializes local open() as DiskOpenSpec(kind=local)", async () => {
    const meta = dummyLocalMeta;

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
    const spec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "hdd",
        format: "raw",
        url: "https://example.invalid/disk.img?token=secret",
        credentials: "include",
        cacheKey: "win7-sp1-x64.sha256-deadbeef",
      },
    } satisfies DiskOpenSpec;
    if (spec.kind !== "remote" || spec.remote.delivery !== "range") {
      throw new Error("expected a range remote disk spec");
    }
    const remote = spec.remote;

    const w = new StubWorker();
    const client = new RuntimeDiskClient(w as unknown as Worker);

    const openPromise = client.open(spec);
    expect(w.lastMessage.op).toBe("open");
    expect(w.lastMessage.payload.spec.kind).toBe("remote");
    expect(w.lastMessage.payload.spec.remote.cacheKey).toBe(remote.cacheKey);
    expect(w.lastMessage.payload.spec.remote.url).toBe(remote.url);

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

  it("readInto/writeFrom operate directly on SharedArrayBuffer-backed ranges (zero-copy)", async () => {
    const posted: any[] = [];

    const sab = new SharedArrayBuffer(4096);
    const expectedReadOffset = 123;
    const expectedWriteOffset = 512;

    let lastWrite: Uint8Array | null = null;
    const disk = {
      sectorSize: 512,
      capacityBytes: 4096,
      async readSectors(lba: number, buffer: Uint8Array) {
        expect(buffer.buffer).toBe(sab);
        expect(buffer.byteOffset).toBe(expectedReadOffset);
        expect(buffer.byteLength).toBe(512);
        for (let i = 0; i < buffer.length; i++) {
          buffer[i] = ((lba * 17 + i) & 0xff) >>> 0;
        }
      },
      async writeSectors(_lba: number, data: Uint8Array) {
        expect(data.buffer).toBe(sab);
        expect(data.byteOffset).toBe(expectedWriteOffset);
        expect(data.byteLength).toBe(512);
        lastWrite = data.slice();
      },
      async flush() {},
    };

    const openDisk: OpenDiskFn = async () => ({ disk, readOnly: false, backendSnapshot: null });
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    await worker.handleMessage({ type: "request", requestId: 1, op: "open", payload: { spec: { kind: "local", meta: dummyLocalMeta } } });
    const opened = posted.shift();
    expect(opened.ok).toBe(true);
    const handle = opened.result.handle as number;

    // readInto should mutate SAB at the destination offset.
    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "readInto",
      payload: { handle, lba: 2, byteLength: 512, dest: { sab, offsetBytes: expectedReadOffset } },
    });
    const readResp = posted.shift();
    expect(readResp.ok).toBe(true);

    const got = new Uint8Array(sab, expectedReadOffset, 512);
    const expected = new Uint8Array(512);
    for (let i = 0; i < expected.length; i++) expected[i] = ((2 * 17 + i) & 0xff) >>> 0;
    expect(Array.from(got)).toEqual(Array.from(expected));

    // writeFrom should read from SAB at the source offset.
    const src = new Uint8Array(sab, expectedWriteOffset, 512);
    for (let i = 0; i < src.length; i++) src[i] = (255 - (i & 0xff)) & 0xff;

    await worker.handleMessage({
      type: "request",
      requestId: 3,
      op: "writeFrom",
      payload: { handle, lba: 0, src: { sab, offsetBytes: expectedWriteOffset, byteLength: 512 } },
    });
    const writeResp = posted.shift();
    expect(writeResp.ok).toBe(true);
    if (!lastWrite) throw new Error("expected writeSectors to be called");
    expect(Array.from(lastWrite)).toEqual(Array.from(src));
  });

  it("validates readInto payload alignment", async () => {
    const posted: any[] = [];
    const disk = {
      sectorSize: 512,
      capacityBytes: 4096,
      async readSectors() {},
      async writeSectors() {},
      async flush() {},
    };
    const openDisk: OpenDiskFn = async () => ({ disk, readOnly: false, backendSnapshot: null });
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    await worker.handleMessage({ type: "request", requestId: 1, op: "open", payload: { spec: { kind: "local", meta: dummyLocalMeta } } });
    const handle = posted.shift().result.handle as number;

    const sab = new SharedArrayBuffer(1024);
    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "readInto",
      payload: { handle, lba: 0, byteLength: 513, dest: { sab, offsetBytes: 0 } },
    });
    const resp = posted.shift();
    expect(resp.ok).toBe(false);
    expect(String(resp.error.message)).toMatch(/unaligned|multiple/i);
  });

  it("validates writeFrom payload alignment", async () => {
    const posted: any[] = [];
    const disk = {
      sectorSize: 512,
      capacityBytes: 4096,
      async readSectors() {},
      async writeSectors() {},
      async flush() {},
    };
    const openDisk: OpenDiskFn = async () => ({ disk, readOnly: false, backendSnapshot: null });
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    await worker.handleMessage({ type: "request", requestId: 1, op: "open", payload: { spec: { kind: "local", meta: dummyLocalMeta } } });
    const handle = posted.shift().result.handle as number;

    const sab = new SharedArrayBuffer(1024);
    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "writeFrom",
      payload: { handle, lba: 0, src: { sab, offsetBytes: 0, byteLength: 513 } },
    });
    const resp = posted.shift();
    expect(resp.ok).toBe(false);
    expect(String(resp.error.message)).toMatch(/unaligned|multiple/i);
  });

  it("validates readInto SAB bounds + SAB requirement", async () => {
    const posted: any[] = [];
    const disk = {
      sectorSize: 512,
      capacityBytes: 4096,
      async readSectors() {},
      async writeSectors() {},
      async flush() {},
    };
    const openDisk: OpenDiskFn = async () => ({ disk, readOnly: false, backendSnapshot: null });
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta: dummyLocalMeta } },
    });
    const handle = posted.shift().result.handle as number;

    const sab = new SharedArrayBuffer(1024);
    // OOB: 800 + 512 > 1024.
    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "readInto",
      payload: { handle, lba: 0, byteLength: 512, dest: { sab, offsetBytes: 800 } },
    });
    const oobResp = posted.shift();
    expect(oobResp.ok).toBe(false);
    expect(String(oobResp.error.message)).toMatch(/out of bounds/i);

    // Require SAB: reject ArrayBuffer.
    await worker.handleMessage({
      type: "request",
      requestId: 3,
      op: "readInto",
      payload: { handle, lba: 0, byteLength: 512, dest: { sab: new ArrayBuffer(1024) as any, offsetBytes: 0 } },
    } as any);
    const sabResp = posted.shift();
    expect(sabResp.ok).toBe(false);
    expect(String(sabResp.error.message)).toMatch(/SharedArrayBuffer/i);
  });

  it("validates writeFrom SAB bounds + SAB requirement", async () => {
    const posted: any[] = [];
    const disk = {
      sectorSize: 512,
      capacityBytes: 4096,
      async readSectors() {},
      async writeSectors() {},
      async flush() {},
    };
    const openDisk: OpenDiskFn = async () => ({ disk, readOnly: false, backendSnapshot: null });
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta: dummyLocalMeta } },
    });
    const handle = posted.shift().result.handle as number;

    const sab = new SharedArrayBuffer(1024);
    // OOB: 800 + 512 > 1024.
    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "writeFrom",
      payload: { handle, lba: 0, src: { sab, offsetBytes: 800, byteLength: 512 } },
    });
    const oobResp = posted.shift();
    expect(oobResp.ok).toBe(false);
    expect(String(oobResp.error.message)).toMatch(/out of bounds/i);

    // Require SAB: reject ArrayBuffer.
    await worker.handleMessage({
      type: "request",
      requestId: 3,
      op: "writeFrom",
      payload: { handle, lba: 0, src: { sab: new ArrayBuffer(1024) as any, offsetBytes: 0, byteLength: 512 } },
    } as any);
    const sabResp = posted.shift();
    expect(sabResp.ok).toBe(false);
    expect(String(sabResp.error.message)).toMatch(/SharedArrayBuffer/i);
  });

  it("rejects unknown request ops instead of hanging", async () => {
    const posted: any[] = [];
    const disk = {
      sectorSize: 512,
      capacityBytes: 4096,
      async readSectors() {},
      async writeSectors() {},
      async flush() {},
    };
    const openDisk: OpenDiskFn = async () => ({ disk, readOnly: false, backendSnapshot: null });
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    await worker.handleMessage({ type: "request", requestId: 1, op: "nope", payload: {} } as any);
    const resp = posted.shift();
    expect(resp.ok).toBe(false);
    expect(String(resp.error.message)).toMatch(/unsupported runtime disk op/i);
  });
});
