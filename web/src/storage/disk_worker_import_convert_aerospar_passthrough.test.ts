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
  // `Blob`/`File` constructors expect ArrayBuffer-backed views, but under `ES2024.SharedMemory`
  // libs TypeScript treats `Uint8Array` as potentially backed by `SharedArrayBuffer`.
  return bytes.buffer instanceof ArrayBuffer
    ? (bytes as Uint8Array<ArrayBuffer>)
    : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
}

async function sendImportConvert(file: File): Promise<any> {
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
      backend: "opfs",
      op: "import_convert",
      payload: { file },
    },
  });

  return await response;
}

describe("disk_worker import_convert aerospar passthrough", () => {
  it("imports existing aerospar disks without converting", async () => {
    const logicalSize = 1024 * 1024;
    const bytes = makeAerosparBytes({ diskSizeBytes: logicalSize, blockSizeBytes: 4096 });
    const file = new File([toArrayBufferBytes(bytes)], "already.aerospar");

    const resp = await sendImportConvert(file);
    expect(resp.ok).toBe(true);
    expect(resp.result.format).toBe("aerospar");
    expect(resp.result.kind).toBe("hdd");
    expect(resp.result.sizeBytes).toBe(logicalSize);
    expect(String(resp.result.fileName)).toMatch(/\.aerospar$/);
  });
});
