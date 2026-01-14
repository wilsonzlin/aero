import "../../test/fake_indexeddb_auto.ts";

import { afterEach, describe, expect, it } from "vitest";

import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";
import type { RuntimeDiskRequestMessage } from "./runtime_disk_protocol";
import { clearIdb, idbTxDone, openDiskManagerDb } from "./metadata";

afterEach(async () => {
  await clearIdb();
});

async function putIdbChunk(diskId: string, index: number, bytes: Uint8Array): Promise<void> {
  const db = await openDiskManagerDb();
  try {
    const tx = db.transaction(["chunks"], "readwrite");
    tx.objectStore("chunks").put({ id: diskId, index, data: bytes.buffer });
    await idbTxDone(tx);
  } finally {
    db.close();
  }
}

describe("RuntimeDiskWorker IndexedDB raw open container sniffing", () => {
  it("refuses to open qcow2 bytes as a raw disk", async () => {
    const diskId = "disk1";
    const buf = new Uint8Array(512);
    buf.set([0x51, 0x46, 0x49, 0xfb], 0); // "QFI\xfb"
    new DataView(buf.buffer).setUint32(4, 3, false);
    await putIdbChunk(diskId, 0, buf);

    const meta: DiskImageMetadata = {
      source: "local",
      id: diskId,
      name: "bad.img",
      backend: "idb",
      kind: "hdd",
      format: "raw",
      fileName: `${diskId}.img`,
      sizeBytes: 512,
      createdAtMs: 0,
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));
    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta }, mode: "direct" },
    } satisfies RuntimeDiskRequestMessage);

    const resp = posted.shift();
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/qcow2/i);
    expect(String(resp.error?.message ?? "")).toMatch(/format mismatch/i);
  });

  it("refuses to open aerospar headers as a raw disk", async () => {
    const diskId = "disk2";
    const buf = new Uint8Array(512);
    buf.set(new TextEncoder().encode("AEROSPAR"), 0);
    new DataView(buf.buffer).setUint32(8, 1, true);
    await putIdbChunk(diskId, 0, buf);

    const meta: DiskImageMetadata = {
      source: "local",
      id: diskId,
      name: "bad.img",
      backend: "idb",
      kind: "hdd",
      format: "raw",
      fileName: `${diskId}.img`,
      sizeBytes: 512,
      createdAtMs: 0,
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));
    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta }, mode: "direct" },
    } satisfies RuntimeDiskRequestMessage);

    const resp = posted.shift();
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/aerospar/i);
    expect(String(resp.error?.message ?? "")).toMatch(/format mismatch/i);
  });
});

