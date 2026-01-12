import { describe, expect, it } from "vitest";

import {
  AEROGPU_CMD_COPY_TEXTURE2D_SIZE,
  AEROGPU_CMD_CREATE_TEXTURE2D_SIZE,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AEROGPU_CMD_STREAM_MAGIC,
  AEROGPU_CMD_UPLOAD_RESOURCE_SIZE,
  AEROGPU_COPY_FLAG_WRITEBACK_DST,
  AerogpuCmdOpcode,
} from "../../../emulator/protocol/aerogpu/aerogpu_cmd";
import { AerogpuFormat, AEROGPU_ABI_VERSION_U32 } from "../../../emulator/protocol/aerogpu/aerogpu_pci";
import {
  AEROGPU_ALLOC_ENTRY_SIZE,
  AEROGPU_ALLOC_TABLE_HEADER_SIZE,
  AEROGPU_ALLOC_TABLE_MAGIC,
} from "../../../emulator/protocol/aerogpu/aerogpu_ring";

import { createAerogpuCpuExecutorState, decodeAerogpuAllocTable, executeAerogpuCmdStream } from "./aerogpu-acmd-executor";

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

  it("forces alpha=255 for X8 sRGB textures on upload", () => {
    const width = 2;
    const height = 2;

    const uploadPixels = new Uint8Array([
      // row0: [1,2,3,x] [4,5,6,x]
      1, 2, 3, 0, 4, 5, 6, 0,
      // row1: [7,8,9,x] [10,11,12,x]
      7, 8, 9, 0, 10, 11, 12, 0,
    ]);

    const buildCreateUploadStream = (handle: number, format: number): ArrayBuffer => {
      const createSize = AEROGPU_CMD_CREATE_TEXTURE2D_SIZE;
      const uploadSize = AEROGPU_CMD_UPLOAD_RESOURCE_SIZE + uploadPixels.byteLength;
      const streamSize = AEROGPU_CMD_STREAM_HEADER_SIZE + createSize + uploadSize;

      const buf = new ArrayBuffer(streamSize);
      const dv = new DataView(buf);

      dv.setUint32(0, AEROGPU_CMD_STREAM_MAGIC, true);
      dv.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
      dv.setUint32(8, streamSize, true);
      dv.setUint32(12, 0, true); // flags
      dv.setUint32(16, 0, true); // reserved0
      dv.setUint32(20, 0, true); // reserved1

      let off = AEROGPU_CMD_STREAM_HEADER_SIZE;

      // CREATE_TEXTURE2D
      dv.setUint32(off + 0, AerogpuCmdOpcode.CreateTexture2d, true);
      dv.setUint32(off + 4, createSize, true);
      dv.setUint32(off + 8, handle, true); // texture_handle
      dv.setUint32(off + 12, 0, true); // usage_flags
      dv.setUint32(off + 16, format, true);
      dv.setUint32(off + 20, width, true);
      dv.setUint32(off + 24, height, true);
      dv.setUint32(off + 28, 1, true); // mip_levels
      dv.setUint32(off + 32, 1, true); // array_layers
      dv.setUint32(off + 36, 0, true); // row_pitch_bytes
      dv.setUint32(off + 40, 0, true); // backing_alloc_id
      dv.setUint32(off + 44, 0, true); // backing_offset_bytes
      dv.setBigUint64(off + 48, 0n, true); // reserved0

      off += createSize;

      // UPLOAD_RESOURCE
      dv.setUint32(off + 0, AerogpuCmdOpcode.UploadResource, true);
      dv.setUint32(off + 4, uploadSize, true);
      dv.setUint32(off + 8, handle, true); // resource_handle
      dv.setUint32(off + 12, 0, true); // reserved0
      dv.setBigUint64(off + 16, 0n, true); // offset_bytes
      dv.setBigUint64(off + 24, BigInt(uploadPixels.byteLength), true); // size_bytes
      new Uint8Array(buf, off + AEROGPU_CMD_UPLOAD_RESOURCE_SIZE, uploadPixels.byteLength).set(uploadPixels);

      return buf;
    };

    const cases: Array<{ format: number; expectedFirstPixel: number[] }> = [
      // BGRX* reads as RGBA (swap R/B) internally.
      { format: AerogpuFormat.B8G8R8X8UnormSrgb, expectedFirstPixel: [3, 2, 1, 255] },
      { format: AerogpuFormat.R8G8B8X8UnormSrgb, expectedFirstPixel: [1, 2, 3, 255] },
    ];

    for (const { format, expectedFirstPixel } of cases) {
      const state = createAerogpuCpuExecutorState();
      executeAerogpuCmdStream(state, buildCreateUploadStream(1, format), { allocTable: null, guestU8: null });

      const tex = state.textures.get(1);
      expect(tex).toBeTruthy();
      if (!tex) throw new Error("missing texture handle 1");

      expect(Array.from(tex.data.slice(0, 4))).toEqual(expectedFirstPixel);
      for (let i = 3; i < tex.data.length; i += 4) {
        expect(tex.data[i]).toBe(255);
      }
    }
  });

  it("forces alpha=255 for X8 sRGB textures on writeback", () => {
    const width = 2;
    const height = 2;
    const rowPitchBytes = width * 4;

    const uploadPixels = new Uint8Array([
      // row0: [1,2,3,x] [4,5,6,x]
      1, 2, 3, 0, 4, 5, 6, 0,
      // row1: [7,8,9,x] [10,11,12,x]
      7, 8, 9, 0, 10, 11, 12, 0,
    ]);

    const buildStream = (format: number): ArrayBuffer => {
      const createSize = AEROGPU_CMD_CREATE_TEXTURE2D_SIZE;
      const uploadSize = AEROGPU_CMD_UPLOAD_RESOURCE_SIZE + uploadPixels.byteLength;
      const copySize = AEROGPU_CMD_COPY_TEXTURE2D_SIZE;
      const streamSize = AEROGPU_CMD_STREAM_HEADER_SIZE + createSize + uploadSize + createSize + copySize;

      const buf = new ArrayBuffer(streamSize);
      const dv = new DataView(buf);

      dv.setUint32(0, AEROGPU_CMD_STREAM_MAGIC, true);
      dv.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
      dv.setUint32(8, streamSize, true);
      dv.setUint32(12, 0, true); // flags
      dv.setUint32(16, 0, true); // reserved0
      dv.setUint32(20, 0, true); // reserved1

      let off = AEROGPU_CMD_STREAM_HEADER_SIZE;

      // CREATE_TEXTURE2D src (host-owned).
      dv.setUint32(off + 0, AerogpuCmdOpcode.CreateTexture2d, true);
      dv.setUint32(off + 4, createSize, true);
      dv.setUint32(off + 8, 1, true); // texture_handle
      dv.setUint32(off + 12, 0, true); // usage_flags
      dv.setUint32(off + 16, format, true);
      dv.setUint32(off + 20, width, true);
      dv.setUint32(off + 24, height, true);
      dv.setUint32(off + 28, 1, true); // mip_levels
      dv.setUint32(off + 32, 1, true); // array_layers
      dv.setUint32(off + 36, 0, true); // row_pitch_bytes
      dv.setUint32(off + 40, 0, true); // backing_alloc_id
      dv.setUint32(off + 44, 0, true); // backing_offset_bytes
      dv.setBigUint64(off + 48, 0n, true); // reserved0
      off += createSize;

      // UPLOAD_RESOURCE src
      dv.setUint32(off + 0, AerogpuCmdOpcode.UploadResource, true);
      dv.setUint32(off + 4, uploadSize, true);
      dv.setUint32(off + 8, 1, true); // resource_handle
      dv.setUint32(off + 12, 0, true); // reserved0
      dv.setBigUint64(off + 16, 0n, true); // offset_bytes
      dv.setBigUint64(off + 24, BigInt(uploadPixels.byteLength), true); // size_bytes
      new Uint8Array(buf, off + AEROGPU_CMD_UPLOAD_RESOURCE_SIZE, uploadPixels.byteLength).set(uploadPixels);
      off += uploadSize;

      // CREATE_TEXTURE2D dst (guest-backed).
      dv.setUint32(off + 0, AerogpuCmdOpcode.CreateTexture2d, true);
      dv.setUint32(off + 4, createSize, true);
      dv.setUint32(off + 8, 2, true); // texture_handle
      dv.setUint32(off + 12, 0, true); // usage_flags
      dv.setUint32(off + 16, format, true);
      dv.setUint32(off + 20, width, true);
      dv.setUint32(off + 24, height, true);
      dv.setUint32(off + 28, 1, true); // mip_levels
      dv.setUint32(off + 32, 1, true); // array_layers
      dv.setUint32(off + 36, rowPitchBytes, true); // row_pitch_bytes
      dv.setUint32(off + 40, 1, true); // backing_alloc_id
      dv.setUint32(off + 44, 0, true); // backing_offset_bytes
      dv.setBigUint64(off + 48, 0n, true); // reserved0
      off += createSize;

      // COPY_TEXTURE2D (WRITEBACK_DST) dst <- src
      dv.setUint32(off + 0, AerogpuCmdOpcode.CopyTexture2d, true);
      dv.setUint32(off + 4, copySize, true);
      dv.setUint32(off + 8, 2, true); // dst_texture
      dv.setUint32(off + 12, 1, true); // src_texture
      dv.setUint32(off + 16, 0, true); // dst_mip_level
      dv.setUint32(off + 20, 0, true); // dst_array_layer
      dv.setUint32(off + 24, 0, true); // src_mip_level
      dv.setUint32(off + 28, 0, true); // src_array_layer
      dv.setUint32(off + 32, 0, true); // dst_x
      dv.setUint32(off + 36, 0, true); // dst_y
      dv.setUint32(off + 40, 0, true); // src_x
      dv.setUint32(off + 44, 0, true); // src_y
      dv.setUint32(off + 48, width, true);
      dv.setUint32(off + 52, height, true);
      dv.setUint32(off + 56, AEROGPU_COPY_FLAG_WRITEBACK_DST, true); // flags
      dv.setUint32(off + 60, 0, true); // reserved0

      return buf;
    };

    const formats = [AerogpuFormat.B8G8R8X8UnormSrgb, AerogpuFormat.R8G8B8X8UnormSrgb];

    for (const format of formats) {
      const state = createAerogpuCpuExecutorState();
      const guestU8 = new Uint8Array(0x1000);
      guestU8.fill(0xee);
      const allocTable = new Map([[1, { gpa: 0, sizeBytes: guestU8.byteLength, flags: 0 }]]);

      executeAerogpuCmdStream(state, buildStream(format), { allocTable, guestU8 });

      const expected = uploadPixels.slice();
      for (let i = 3; i < expected.length; i += 4) expected[i] = 255;

      expect(Array.from(guestU8.slice(0, expected.length))).toEqual(Array.from(expected));
      // Prove we didn't clobber past the end of the backing copy.
      expect(guestU8[expected.length]).toBe(0xee);
    }
  });
});
