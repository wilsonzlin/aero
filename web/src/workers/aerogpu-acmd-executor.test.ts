import { describe, expect, it } from "vitest";

import { AEROGPU_ABI_VERSION_U32 } from "../../../emulator/protocol/aerogpu/aerogpu_pci";
import {
  AEROGPU_ALLOC_ENTRY_SIZE,
  AEROGPU_ALLOC_TABLE_HEADER_SIZE,
  AEROGPU_ALLOC_TABLE_MAGIC,
} from "../../../emulator/protocol/aerogpu/aerogpu_ring";

import { decodeAerogpuAllocTable } from "./aerogpu-acmd-executor";

describe("workers/aerogpu-acmd-executor", () => {
  it("accepts alloc table entries with gpa=0", () => {
    const sizeBytes = AEROGPU_ALLOC_TABLE_HEADER_SIZE + AEROGPU_ALLOC_ENTRY_SIZE;
    const buf = new ArrayBuffer(sizeBytes);
    const dv = new DataView(buf);

    dv.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
    dv.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
    dv.setUint32(8, sizeBytes, true);
    dv.setUint32(12, 1, true);
    dv.setUint32(16, AEROGPU_ALLOC_ENTRY_SIZE, true);
    dv.setUint32(20, 0, true);

    const entryBase = AEROGPU_ALLOC_TABLE_HEADER_SIZE;
    dv.setUint32(entryBase + 0, 1, true); // alloc_id
    dv.setUint32(entryBase + 4, 0, true); // flags
    dv.setBigUint64(entryBase + 8, 0n, true); // gpa
    dv.setBigUint64(entryBase + 16, 0x1000n, true); // size_bytes
    dv.setBigUint64(entryBase + 24, 0n, true); // reserved0

    const table = decodeAerogpuAllocTable(buf);
    expect(table.get(1)).toEqual({ gpa: 0, sizeBytes: 4096, flags: 0 });
  });
});

