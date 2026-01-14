import { afterEach, describe, expect, it, vi } from "vitest";

import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

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

function toArrayBufferUint8(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // Newer TS libdefs model typed arrays as `Uint8Array<ArrayBufferLike>`, but `File`/`Blob` inputs
  // are typed to accept only `ArrayBuffer`-backed views. Ensure the backing store is transferable.
  return bytes.buffer instanceof ArrayBuffer ? (bytes as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(bytes);
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

function toArrayBufferUint8(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // `BlobPart` types only accept ArrayBuffer-backed views; `Uint8Array` is generic over
  // `ArrayBufferLike` and may be backed by `SharedArrayBuffer`. Copy when needed so TypeScript
  // (and spec compliance) are happy.
  return bytes.buffer instanceof ArrayBuffer ? (bytes as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(bytes);
}

async function sendImportFile(payload: any, backend: "opfs" | "idb" = "opfs"): Promise<any> {
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

  await import("./disk_worker.ts");

  workerScope.onmessage?.({
    data: {
      type: "request",
      requestId,
      backend,
      op: "import_file",
      payload,
    },
  });

  return await response;
}

describe("disk_worker import_file aerospar handling", () => {
  it("uses logical aerospar disk size (not file size) for sizeBytes", async () => {
    const diskSizeBytes = 1024 * 1024;
    const bytes = makeAerosparBytes({ diskSizeBytes, blockSizeBytes: 4096 });
    expect(bytes.byteLength).toBeLessThan(diskSizeBytes);

    const file = new File([toArrayBufferUint8(bytes)], "base.aerospar");
    const resp = await sendImportFile({ file });
    expect(resp.ok).toBe(true);
    expect(resp.result.format).toBe("aerospar");
    expect(resp.result.kind).toBe("hdd");
    expect(resp.result.sizeBytes).toBe(diskSizeBytes);
    expect(String(resp.result.fileName)).toMatch(/\.aerospar$/);
  });

  it("detects aerospar content even when the filename suggests raw", async () => {
    const bytes = makeAerosparBytes({ diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096 });
    const file = new File([toArrayBufferUint8(bytes)], "mislabeled.img");
    const resp = await sendImportFile({ file });
    expect(resp.ok).toBe(true);
    expect(resp.result.format).toBe("aerospar");
    expect(resp.result.kind).toBe("hdd");
    expect(resp.result.sourceFileName).toBe("mislabeled.img");
    expect(String(resp.result.fileName)).toMatch(/\.aerospar$/);
  });

  it("rejects explicit aerospar imports when the header is missing", async () => {
    const file = new File([toArrayBufferUint8(new Uint8Array([1, 2, 3, 4]))], "not-aerospar.img");
    const resp = await sendImportFile({ file, format: "aerospar" });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/aerospar/i);
  });

  it("rejects aerospar imports on the IndexedDB backend", async () => {
    const bytes = makeAerosparBytes({ diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096 });
    const file = new File([toArrayBufferUint8(bytes)], "disk.aerospar");
    const resp = await sendImportFile({ file }, "idb");
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/opfs/i);
  });

  it("rejects qcow2 imports on the IndexedDB backend", async () => {
    const file = new File([new Uint8Array([1, 2, 3, 4])], "disk.qcow2");
    const resp = await sendImportFile({ file }, "idb");
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/qcow2/i);
    expect(String(resp.error?.message ?? "")).toMatch(/indexeddb/i);
  });

  it("rejects CD imports when format is not ISO", async () => {
    const file = new File([new Uint8Array(512)], "disk.img");
    const resp = await sendImportFile({ file, kind: "cd", format: "raw" });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/cd/i);
    expect(String(resp.error?.message ?? "")).toMatch(/iso/i);
  });

  it("rejects HDD imports when format is ISO", async () => {
    const file = new File([new Uint8Array(512)], "disk.iso");
    const resp = await sendImportFile({ file, kind: "hdd", format: "iso" });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/hdd/i);
    expect(String(resp.error?.message ?? "")).toMatch(/iso/i);
  });

  it("rejects empty files", async () => {
    const file = new File([new Uint8Array()], "empty.img");
    const resp = await sendImportFile({ file });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/file size/i);
  });

  it("rejects raw/iso images that are not sector-aligned", async () => {
    const file = new File([new Uint8Array(513)], "unaligned.img");
    const resp = await sendImportFile({ file });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/multiple of 512/i);
  });
});
