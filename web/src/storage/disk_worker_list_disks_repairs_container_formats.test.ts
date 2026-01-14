import { afterEach, describe, expect, it, vi } from "vitest";

import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";
import { METADATA_VERSION, opfsGetDisksDir, opfsReadState, opfsWriteState, type DiskManagerState } from "./metadata";

let restoreOpfs: (() => void) | null = null;
let hadOriginalSelf = false;
let originalSelf: unknown = undefined;

afterEach(() => {
  restoreOpfs?.();
  restoreOpfs = null;

  if (!hadOriginalSelf) {
    Reflect.deleteProperty(globalThis as unknown as { self?: unknown }, "self");
  } else {
    (globalThis as unknown as { self?: unknown }).self = originalSelf;
  }
  hadOriginalSelf = false;
  originalSelf = undefined;

  vi.clearAllMocks();
  vi.resetModules();
});

function toArrayBufferBytes(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // `FileSystemWritableFileStream.write()` expects an ArrayBuffer-backed view, but under
  // `ES2024.SharedMemory` libs TypeScript treats `Uint8Array` as potentially backed by
  // `SharedArrayBuffer`. Copy when needed to keep types (and spec compliance) happy.
  return bytes.buffer instanceof ArrayBuffer ? (bytes as Uint8Array<ArrayBuffer>) : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
}

async function listDisksWithFixture(setup: () => Promise<void>): Promise<any> {
  vi.resetModules();

  const root = new MemoryDirectoryHandle("root");
  restoreOpfs = installMemoryOpfs(root).restore;

  hadOriginalSelf = Object.prototype.hasOwnProperty.call(globalThis, "self");
  originalSelf = (globalThis as unknown as { self?: unknown }).self;

  const requestId = 1;
  let resolveResponse: ((msg: any) => void) | null = null;
  const response = new Promise<any>((resolve) => {
    resolveResponse = resolve;
  });

  const workerScope: any = {
    postMessage(msg: any) {
      if (msg?.type === "response" && msg.requestId === requestId) {
        resolveResponse?.(msg);
      }
    },
  };
  (globalThis as unknown as { self?: unknown }).self = workerScope;

  await setup();

  await import("./disk_worker.ts");

  workerScope.onmessage?.({
    data: {
      type: "request",
      requestId,
      backend: "opfs",
      op: "list_disks",
      payload: {},
    },
  });

  return await response;
}

describe("disk_worker list_disks repairs legacy container misclassifications", () => {
  it("repairs qcow2 files that were imported as raw", async () => {
    const id = "qcow2_disk";
    const fileName = `${id}.img`;
    const qcow2 = new Uint8Array(72);
    qcow2.set([0x51, 0x46, 0x49, 0xfb], 0); // "QFI\xfb"
    new DataView(qcow2.buffer).setUint32(4, 3, false);

    const resp = await listDisksWithFixture(async () => {
      const disksDir = await opfsGetDisksDir();
      const fh = await disksDir.getFileHandle(fileName, { create: true });
      const w = await fh.createWritable({ keepExistingData: false });
      await w.write(toArrayBufferBytes(qcow2));
      await w.close();

      const state: DiskManagerState = {
        version: METADATA_VERSION,
        disks: {
          [id]: {
            source: "local",
            id,
            name: "qcow2",
            backend: "opfs",
            kind: "hdd",
            // Legacy import path inferred from `.img` extension.
            format: "raw",
            fileName,
            sizeBytes: qcow2.byteLength,
            createdAtMs: Date.now(),
          },
        },
        mounts: {},
      };
      await opfsWriteState(state);
    });

    expect(resp.ok).toBe(true);
    expect(resp.result[0].format).toBe("qcow2");
    expect(resp.result[0].kind).toBe("hdd");

    const repaired = await opfsReadState();
    expect(repaired.disks[id]?.format).toBe("qcow2");
    expect(repaired.disks[id]?.kind).toBe("hdd");
  });

  it("repairs VHD files that were imported as raw", async () => {
    const id = "vhd_disk";
    const fileName = `${id}.img`;

    const bytes = new Uint8Array(1024);
    const footer = new Uint8Array(bytes.buffer, 512, 512);
    footer.set(new TextEncoder().encode("conectix"), 0);
    const view = new DataView(bytes.buffer, 512, 512);
    view.setUint32(12, 0x0001_0000, false);
    view.setBigUint64(16, 0xffff_ffff_ffff_ffffn, false);
    view.setBigUint64(48, 512n, false);
    view.setUint32(60, 2, false);

    const resp = await listDisksWithFixture(async () => {
      const disksDir = await opfsGetDisksDir();
      const fh = await disksDir.getFileHandle(fileName, { create: true });
      const w = await fh.createWritable({ keepExistingData: false });
      await w.write(toArrayBufferBytes(bytes));
      await w.close();

      const state: DiskManagerState = {
        version: METADATA_VERSION,
        disks: {
          [id]: {
            source: "local",
            id,
            name: "vhd",
            backend: "opfs",
            kind: "hdd",
            format: "raw",
            fileName,
            sizeBytes: bytes.byteLength,
            createdAtMs: Date.now(),
          },
        },
        mounts: {},
      };
      await opfsWriteState(state);
    });

    expect(resp.ok).toBe(true);
    expect(resp.result[0].format).toBe("vhd");
    expect(resp.result[0].kind).toBe("hdd");

    const repaired = await opfsReadState();
    expect(repaired.disks[id]?.format).toBe("vhd");
    expect(repaired.disks[id]?.kind).toBe("hdd");
  });

  it("repairs ISO images that were imported as raw HDDs", async () => {
    const id = "iso_disk";
    const fileName = `${id}.img`;

    const bytes = new Uint8Array(512 * 65);
    bytes.set(new TextEncoder().encode("CD001"), 0x8001);

    const resp = await listDisksWithFixture(async () => {
      const disksDir = await opfsGetDisksDir();
      const fh = await disksDir.getFileHandle(fileName, { create: true });
      const w = await fh.createWritable({ keepExistingData: false });
      await w.write(toArrayBufferBytes(bytes));
      await w.close();

      const state: DiskManagerState = {
        version: METADATA_VERSION,
        disks: {
          [id]: {
            source: "local",
            id,
            name: "iso",
            backend: "opfs",
            kind: "hdd",
            format: "raw",
            fileName,
            sizeBytes: bytes.byteLength,
            createdAtMs: Date.now(),
          },
        },
        mounts: {},
      };
      await opfsWriteState(state);
    });

    expect(resp.ok).toBe(true);
    expect(resp.result[0].format).toBe("iso");
    expect(resp.result[0].kind).toBe("cd");

    const repaired = await opfsReadState();
    expect(repaired.disks[id]?.format).toBe("iso");
    expect(repaired.disks[id]?.kind).toBe("cd");
  });
});

