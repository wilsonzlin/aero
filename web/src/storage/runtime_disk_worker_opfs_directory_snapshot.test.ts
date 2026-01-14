import { afterEach, describe, expect, it } from "vitest";

import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";
import type { RuntimeDiskRequestMessage } from "./runtime_disk_protocol";
import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

let restoreOpfs: (() => void) | null = null;

afterEach(() => {
  restoreOpfs?.();
  restoreOpfs = null;
});

describe("RuntimeDiskWorker snapshot (opfsDirectory)", () => {
  it("restores local OPFS COW disks stored outside the default aero/disks directory", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    // Create a legacy/adopted-style disk image under `images/`.
    const imagesDir = await root.getDirectoryHandle("images", { create: true });
    const baseHandle = await imagesDir.getFileHandle("legacy.img", { create: true });
    const baseSync = await baseHandle.createSyncAccessHandle();
    baseSync.truncate(4096);
    baseSync.close();

    const meta: DiskImageMetadata = {
      source: "local",
      id: "disk1",
      name: "legacy.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "legacy.img",
      opfsDirectory: "images",
      sizeBytes: 4096,
      createdAtMs: 0,
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta }, mode: "cow", overlayBlockSizeBytes: 512 },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);
    const handle = openResp.result.handle as number;

    const written = new Uint8Array(512);
    written.fill(0xab);

    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "write",
      payload: { handle, lba: 0, data: written },
    } satisfies RuntimeDiskRequestMessage);
    expect(posted.shift().ok).toBe(true);

    await worker.handleMessage({
      type: "request",
      requestId: 3,
      op: "prepareSnapshot",
      payload: {},
    } satisfies RuntimeDiskRequestMessage);

    const snapResp = posted.shift();
    expect(snapResp.ok).toBe(true);
    const state = snapResp.result.state as Uint8Array;
    const json = new TextDecoder().decode(state);
    expect(json).toContain('"dirPath":"images"');

    await worker.handleMessage({
      type: "request",
      requestId: 4,
      op: "close",
      payload: { handle },
    } satisfies RuntimeDiskRequestMessage);
    expect(posted.shift().ok).toBe(true);

    // Restore in a new worker and ensure the overlay data is still visible.
    const posted2: any[] = [];
    const worker2 = new RuntimeDiskWorker((msg) => posted2.push(msg));
    await worker2.handleMessage({
      type: "request",
      requestId: 1,
      op: "restoreFromSnapshot",
      payload: { state },
    } satisfies RuntimeDiskRequestMessage);
    expect(posted2.shift().ok).toBe(true);

    await worker2.handleMessage({
      type: "request",
      requestId: 2,
      op: "read",
      payload: { handle, lba: 0, byteLength: 512 },
    } satisfies RuntimeDiskRequestMessage);

    const readResp = posted2.shift();
    expect(readResp.ok).toBe(true);
    expect(Array.from(readResp.result.data as Uint8Array)).toEqual(Array.from(written));
  });
});

