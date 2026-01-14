import { afterEach, describe, expect, it } from "vitest";

import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";
import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";
import type { RuntimeDiskRequestMessage } from "./runtime_disk_protocol";

let restoreOpfs: (() => void) | null = null;

afterEach(() => {
  restoreOpfs?.();
  restoreOpfs = null;
});

describe("RuntimeDiskWorker OPFS raw open container sniffing", () => {
  it("refuses to open qcow2 bytes as a raw disk", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const imagesDir = await root.getDirectoryHandle("images", { create: true });
    const baseHandle = await imagesDir.getFileHandle("bad.img", { create: true });
    const sync = await baseHandle.createSyncAccessHandle();
    const qcow2 = new Uint8Array(72);
    qcow2.set([0x51, 0x46, 0x49, 0xfb], 0); // "QFI\xfb"
    new DataView(qcow2.buffer).setUint32(4, 3, false);
    sync.truncate(qcow2.byteLength);
    sync.write(qcow2, { at: 0 });
    sync.close();

    const meta: DiskImageMetadata = {
      source: "local",
      id: "disk1",
      name: "bad.img",
      backend: "opfs",
      kind: "hdd",
      // Intentionally wrong: legacy metadata might treat this as raw due to `.img` extension.
      format: "raw",
      fileName: "bad.img",
      opfsDirectory: "images",
      sizeBytes: qcow2.byteLength,
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
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const imagesDir = await root.getDirectoryHandle("images", { create: true });
    const baseHandle = await imagesDir.getFileHandle("bad.img", { create: true });
    const sync = await baseHandle.createSyncAccessHandle();
    const aerosparMagic = new TextEncoder().encode("AEROSPAR");
    sync.truncate(aerosparMagic.byteLength);
    sync.write(aerosparMagic, { at: 0 });
    sync.close();

    const meta: DiskImageMetadata = {
      source: "local",
      id: "disk1",
      name: "bad.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "bad.img",
      opfsDirectory: "images",
      sizeBytes: aerosparMagic.byteLength,
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

