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

function alignUpBigInt(value: bigint, alignment: bigint): bigint {
  if (alignment <= 0n) return value;
  return ((value + alignment - 1n) / alignment) * alignment;
}

function makeAerosparBytes(options: { diskSizeBytes: number; blockSizeBytes: number }): Uint8Array<ArrayBuffer> {
  const { diskSizeBytes, blockSizeBytes } = options;
  const blockSizeBig = BigInt(blockSizeBytes);
  const tableEntries = (BigInt(diskSizeBytes) + blockSizeBig - 1n) / blockSizeBig;
  const dataOffset = alignUpBigInt(64n + tableEntries * 8n, blockSizeBig);
  const fileSize = Number(dataOffset); // allocatedBlocks=0 so file only needs to cover the data offset
  const out = new Uint8Array(fileSize) as Uint8Array<ArrayBuffer>;
  out.set(new TextEncoder().encode("AEROSPAR"), 0);

  const view = new DataView(out.buffer, out.byteOffset, out.byteLength);
  view.setUint32(8, 1, true); // version
  view.setUint32(12, 64, true); // header_size
  view.setUint32(16, blockSizeBytes, true); // block_size_bytes
  view.setUint32(20, 0, true); // reserved
  view.setBigUint64(24, BigInt(diskSizeBytes), true); // disk_size_bytes
  view.setBigUint64(32, 64n, true); // table_offset
  view.setBigUint64(40, tableEntries, true); // table_entries
  view.setBigUint64(48, dataOffset, true); // data_offset
  view.setBigUint64(56, 0n, true); // allocated_blocks

  return out;
}

function toArrayBufferBytes(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // `FileSystemWritableFileStream.write()` expects an ArrayBuffer-backed view, but under
  // `ES2024.SharedMemory` libs TypeScript treats `Uint8Array` as potentially backed by
  // `SharedArrayBuffer`. Copy when needed to keep types (and spec compliance) happy.
  return bytes.buffer instanceof ArrayBuffer ? (bytes as Uint8Array<ArrayBuffer>) : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
}

async function sendListDisks(): Promise<any> {
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

  // Pre-seed OPFS with a sparse aerospar file + metadata that has the *wrong* sizeBytes (file length).
  const id = "disk1";
  const fileName = `${id}.aerospar`;
  const format = "aerospar";
  const logicalSize = 1024 * 1024;
  const bytes = makeAerosparBytes({ diskSizeBytes: logicalSize, blockSizeBytes: 4096 });

  const disksDir = await opfsGetDisksDir();
  const fh = await disksDir.getFileHandle(fileName, { create: true });
  const w = await fh.createWritable({ keepExistingData: false });
  await w.write(toArrayBufferBytes(bytes));
  await w.close();
  const physicalSize = (await fh.getFile()).size;
  expect(physicalSize).toBe(bytes.byteLength);
  expect(physicalSize).toBeLessThan(logicalSize);

  const state: DiskManagerState = {
    version: METADATA_VERSION,
    disks: {
      [id]: {
        source: "local",
        id,
        name: "test",
        backend: "opfs",
        kind: "hdd",
        format,
        fileName,
        // Intentionally wrong: physical file length, not logical disk capacity.
        sizeBytes: physicalSize,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
      },
    },
    mounts: {},
  };
  await opfsWriteState(state);

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

describe("disk_worker list_disks repairs aerospar sizeBytes", () => {
  it("updates metadata to use the logical disk size", async () => {
    const resp = await sendListDisks();
    expect(resp.ok).toBe(true);
    expect(Array.isArray(resp.result)).toBe(true);
    const meta = resp.result[0];
    expect(meta.format).toBe("aerospar");
    expect(meta.sizeBytes).toBe(1024 * 1024);

    const state = await opfsReadState();
    expect(state.disks["disk1"]?.sizeBytes).toBe(1024 * 1024);
  });

  it("repairs mislabeled aerospar files that were imported as raw", async () => {
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

    const id = "disk2";
    const fileName = `${id}.img`;
    const logicalSize = 1024 * 1024;
    const bytes = makeAerosparBytes({ diskSizeBytes: logicalSize, blockSizeBytes: 4096 });

    const disksDir = await opfsGetDisksDir();
    const fh = await disksDir.getFileHandle(fileName, { create: true });
    const w = await fh.createWritable({ keepExistingData: false });
    await w.write(toArrayBufferBytes(bytes));
    await w.close();
    const physicalSize = (await fh.getFile()).size;
    expect(physicalSize).toBeLessThan(logicalSize);

    const state: DiskManagerState = {
      version: METADATA_VERSION,
      disks: {
        [id]: {
          source: "local",
          id,
          name: "test2",
          backend: "opfs",
          kind: "hdd",
          // Intentionally wrong: old import path inferred from `.img` extension.
          format: "raw",
          fileName,
          sizeBytes: physicalSize,
          createdAtMs: Date.now(),
          lastUsedAtMs: undefined,
        },
      },
      mounts: {},
    };
    await opfsWriteState(state);

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

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(Array.isArray(resp.result)).toBe(true);
    expect(resp.result[0].format).toBe("aerospar");
    expect(resp.result[0].sizeBytes).toBe(logicalSize);

    const repaired = await opfsReadState();
    expect(repaired.disks[id]?.format).toBe("aerospar");
    expect(repaired.disks[id]?.sizeBytes).toBe(logicalSize);
  });

  it("treats truncated aerospar magic as aerospar to avoid leaking header bytes as raw", async () => {
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

    const id = "disk3";
    const fileName = `${id}.img`;

    // Create a file that begins with the aerospar magic but is too small to contain a full header.
    const bytes = new TextEncoder().encode("AEROSPAR"); // 8 bytes
    const disksDir = await opfsGetDisksDir();
    const fh = await disksDir.getFileHandle(fileName, { create: true });
    const w = await fh.createWritable({ keepExistingData: false });
    await w.write(bytes);
    await w.close();

    const state: DiskManagerState = {
      version: METADATA_VERSION,
      disks: {
        [id]: {
          source: "local",
          id,
          name: "truncated",
          backend: "opfs",
          kind: "hdd",
          // Intentionally wrong: old metadata might treat this as raw because the extension is `.img`.
          format: "raw",
          fileName,
          sizeBytes: bytes.byteLength,
          createdAtMs: Date.now(),
          lastUsedAtMs: undefined,
        },
      },
      mounts: {},
    };
    await opfsWriteState(state);

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

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(resp.result[0].format).toBe("aerospar");

    const repaired = await opfsReadState();
    expect(repaired.disks[id]?.format).toBe("aerospar");
  });
});
