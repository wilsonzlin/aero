import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_COPY_FLAG_WRITEBACK_DST,
  AerogpuCmdWriter,
  AerogpuSamplerAddressMode,
  AerogpuSamplerFilter,
  AerogpuShaderStage,
} from "../aerogpu/aerogpu_cmd.ts";
import { AEROGPU_ABI_VERSION_U32, AerogpuFormat } from "../aerogpu/aerogpu_pci.ts";
import {
  AEROGPU_ALLOC_ENTRY_SIZE,
  AEROGPU_ALLOC_FLAG_READONLY,
  AEROGPU_ALLOC_TABLE_HEADER_SIZE,
  AEROGPU_ALLOC_TABLE_MAGIC,
} from "../aerogpu/aerogpu_ring.ts";

import {
  createAerogpuCpuExecutorState,
  decodeAerogpuAllocTable,
  executeAerogpuCmdStream,
} from "../../../web/src/workers/aerogpu-acmd-executor.ts";

function buildAllocTable(
  entries: Array<{ allocId: number; flags: number; gpa: number; sizeBytes: number }>,
  entryStrideBytes: number = AEROGPU_ALLOC_ENTRY_SIZE,
): ArrayBuffer {
  const totalSize = AEROGPU_ALLOC_TABLE_HEADER_SIZE + entries.length * entryStrideBytes;
  const buf = new ArrayBuffer(totalSize);
  const view = new DataView(buf);

  view.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(8, totalSize, true);
  view.setUint32(12, entries.length, true);
  view.setUint32(16, entryStrideBytes, true);

  for (let i = 0; i < entries.length; i += 1) {
    const e = entries[i]!;
    const base = AEROGPU_ALLOC_TABLE_HEADER_SIZE + i * entryStrideBytes;
    view.setUint32(base + 0, e.allocId, true);
    view.setUint32(base + 4, e.flags, true);
    view.setBigUint64(base + 8, BigInt(e.gpa), true);
    view.setBigUint64(base + 16, BigInt(e.sizeBytes), true);
    view.setBigUint64(base + 24, 0n, true);
    if (entryStrideBytes > AEROGPU_ALLOC_ENTRY_SIZE) {
      view.setUint32(base + AEROGPU_ALLOC_ENTRY_SIZE, 0xdeadbeef, true);
    }
  }

  return buf;
}

test("ACMD COPY_BUFFER writeback updates guest memory backing for dst buffer", () => {
  const guest = new Uint8Array(512);

  const allocTableBuf = buildAllocTable([{ allocId: 42, flags: 0, gpa: 100, sizeBytes: 256 }]);
  const allocTable = decodeAerogpuAllocTable(allocTableBuf);

  const w = new AerogpuCmdWriter();
  w.createBuffer(1, 0, 8n, 0, 0); // src
  w.createBuffer(2, 0, 8n, 42, 16); // dst backed
  w.uploadResource(1, 0n, Uint8Array.of(1, 2, 3, 4));
  w.copyBuffer(2, 1, 2n, 0n, 4n, AEROGPU_COPY_FLAG_WRITEBACK_DST);

  const state = createAerogpuCpuExecutorState();
  executeAerogpuCmdStream(state, w.finish().buffer, { allocTable, guestU8: guest });

  assert.deepEqual(Array.from(guest.subarray(118, 122)), [1, 2, 3, 4]);
});

test("ACMD COPY_BUFFER writeback supports guest paddr in high-RAM remap window (>=4GiB)", () => {
  // Mirrors the PC/Q35 E820 layout constants used by the web runtime.
  // Keep in sync with `web/src/runtime/shared_layout.ts`.
  const LOW_RAM_END = 0xb000_0000;
  const HIGH_RAM_START = 0x1_0000_0000;

  class FakeGuestU8 {
    readonly byteLength: number;
    lastSubarray: { start: number; end: number } | null = null;
    lastWrite: { start: number; data: number[] } | null = null;

    constructor(byteLength: number) {
      this.byteLength = byteLength;
    }

    subarray(start: number, end: number): Uint8Array {
      this.lastSubarray = { start, end };
      // Return a minimal view-like object that implements `set()`.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return {
        set: (src: Uint8Array, offset = 0) => {
          this.lastWrite = { start: start + (offset >>> 0), data: Array.from(src) };
        },
      } as any as Uint8Array;
    }
  }

  // Fake a 2.75GiB+256 guest RAM backing store without actually allocating it.
  const guestSize = LOW_RAM_END + 0x100;
  const fakeGuest = new FakeGuestU8(guestSize);
  const guest = fakeGuest as unknown as Uint8Array;

  const allocTableBuf = buildAllocTable([
    // Backing range lives in the remapped high-RAM segment at >=4GiB.
    { allocId: 42, flags: 0, gpa: HIGH_RAM_START, sizeBytes: 0x100 },
  ]);
  const allocTable = decodeAerogpuAllocTable(allocTableBuf);

  const w = new AerogpuCmdWriter();
  w.createBuffer(1, 0, 8n, 0, 0); // src
  w.createBuffer(2, 0, 8n, 42, 0); // dst backed at HIGH_RAM_START
  w.uploadResource(1, 0n, Uint8Array.of(1, 2, 3, 4));
  w.copyBuffer(2, 1, 0n, 0n, 4n, AEROGPU_COPY_FLAG_WRITEBACK_DST);

  const state = createAerogpuCpuExecutorState();
  executeAerogpuCmdStream(state, w.finish().buffer, { allocTable, guestU8: guest });

  // HIGH_RAM_START should translate to backing RAM offset LOW_RAM_END.
  assert.deepEqual(fakeGuest.lastSubarray, { start: LOW_RAM_END, end: LOW_RAM_END + 4 });
  assert.deepEqual(fakeGuest.lastWrite, { start: LOW_RAM_END, data: [1, 2, 3, 4] });
});

test("ACMD COPY_BUFFER writeback rejects READONLY allocs", () => {
  const guest = new Uint8Array(256);
  const allocTableBuf = buildAllocTable([{ allocId: 42, flags: AEROGPU_ALLOC_FLAG_READONLY, gpa: 100, sizeBytes: 128 }]);
  const allocTable = decodeAerogpuAllocTable(allocTableBuf);

  const w = new AerogpuCmdWriter();
  w.createBuffer(1, 0, 8n, 0, 0);
  w.createBuffer(2, 0, 8n, 42, 0);
  w.uploadResource(1, 0n, Uint8Array.of(9, 9, 9, 9));
  w.copyBuffer(2, 1, 0n, 0n, 4n, AEROGPU_COPY_FLAG_WRITEBACK_DST);

  const state = createAerogpuCpuExecutorState();
  assert.throws(
    () => executeAerogpuCmdStream(state, w.finish().buffer, { allocTable, guestU8: guest }),
    /READONLY/,
  );
});

test("decodeAerogpuAllocTable accepts entries with gpa=0", () => {
  const allocTableBuf = buildAllocTable([{ allocId: 1, flags: 0, gpa: 0, sizeBytes: 128 }]);
  const allocTable = decodeAerogpuAllocTable(allocTableBuf);
  assert.deepEqual(allocTable.get(1), { gpa: 0, sizeBytes: 128, flags: 0 });
});

test("decodeAerogpuAllocTable accepts extended entry_stride_bytes", () => {
  const allocTableBuf = buildAllocTable(
    [{ allocId: 1, flags: 0, gpa: 100, sizeBytes: 128 }],
    AEROGPU_ALLOC_ENTRY_SIZE + 16,
  );
  const allocTable = decodeAerogpuAllocTable(allocTableBuf);
  assert.deepEqual(allocTable.get(1), { gpa: 100, sizeBytes: 128, flags: 0 });
});

test("ACMD COPY_TEXTURE2D writeback packs rows using row_pitch_bytes and encodes X8 alpha as 255", () => {
  const guest = new Uint8Array(1024);

  const allocTableBuf = buildAllocTable([{ allocId: 99, flags: 0, gpa: 300, sizeBytes: 256 }]);
  const allocTable = decodeAerogpuAllocTable(allocTableBuf);

  // 2x2 BGRAX texture with padded rows (rowPitch=12, rowBytes=8).
  const rowPitchBytes = 12;
  const upload = new Uint8Array(rowPitchBytes * 2);
  // Row 0 (y=0): pixel(0,0)=[1,2,3,0] pixel(1,0)=[4,5,6,0]
  upload.set([1, 2, 3, 0, 4, 5, 6, 0], 0);
  // Row 1 (y=1): pixel(0,1)=[7,8,9,0] pixel(1,1)=[10,11,12,0]
  upload.set([7, 8, 9, 0, 10, 11, 12, 0], rowPitchBytes);

  const w = new AerogpuCmdWriter();
  w.createTexture2d(3, 0, AerogpuFormat.B8G8R8X8Unorm, 2, 2, 1, 1, rowPitchBytes, 0, 0); // src
  w.createTexture2d(4, 0, AerogpuFormat.B8G8R8X8Unorm, 2, 2, 1, 1, rowPitchBytes, 99, 4); // dst backed
  w.uploadResource(3, 0n, upload);
  // Copy src pixel (0,1) into dst pixel (1,0).
  w.copyTexture2d(4, 3, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, AEROGPU_COPY_FLAG_WRITEBACK_DST);

  const state = createAerogpuCpuExecutorState();
  executeAerogpuCmdStream(state, w.finish().buffer, { allocTable, guestU8: guest });

  // dst backing starts at gpa=300, offset=4. Pixel (1,0) begins at +4 bytes within the row.
  const dstOff = 300 + 4 + 4;
  assert.deepEqual(Array.from(guest.subarray(dstOff, dstOff + 4)), [7, 8, 9, 255]);

  // Other pixels and padding should remain untouched (still zero).
  assert.deepEqual(Array.from(guest.subarray(300 + 4 + 0, 300 + 4 + 4)), [0, 0, 0, 0]);
  assert.deepEqual(Array.from(guest.subarray(300 + 4 + 8, 300 + 4 + 12)), [0, 0, 0, 0]);
});

test("ACMD COPY_TEXTURE2D supports mip+array subresource selectors in the browser executor", () => {
  const guest = new Uint8Array(2048);
  const allocTableBuf = buildAllocTable([{ allocId: 1, flags: 0, gpa: 0, sizeBytes: guest.byteLength }]);
  const allocTable = decodeAerogpuAllocTable(allocTableBuf);

  const width = 2;
  const height = 2;
  const mipLevels = 2;
  const arrayLayers = 2;
  const mip0RowPitchBytes = 12; // padded (rowBytes=8)

  const mip0Size = mip0RowPitchBytes * height; // 24
  const mip1Size = 4; // 1x1 RGBA
  const layerStride = mip0Size + mip1Size; // 28
  const totalBackingSize = layerStride * arrayLayers; // 56

  const writeSolidRgba = (
    base: number,
    w: number,
    h: number,
    rowPitchBytes: number,
    rgba: [number, number, number, number],
  ) => {
    for (let y = 0; y < h; y += 1) {
      const rowBase = base + y * rowPitchBytes;
      for (let x = 0; x < w; x += 1) {
        guest.set(rgba, rowBase + x * 4);
      }
    }
  };

  const srcBackingOffset = 0;
  // layer0 mip0 = red
  writeSolidRgba(srcBackingOffset + 0, 2, 2, mip0RowPitchBytes, [255, 0, 0, 255]);
  // layer0 mip1 = green
  writeSolidRgba(srcBackingOffset + mip0Size, 1, 1, 4, [0, 255, 0, 255]);
  // layer1 mip0 = blue
  writeSolidRgba(srcBackingOffset + layerStride + 0, 2, 2, mip0RowPitchBytes, [0, 0, 255, 255]);
  // layer1 mip1 = yellow (the one we'll copy)
  writeSolidRgba(srcBackingOffset + layerStride + mip0Size, 1, 1, 4, [255, 255, 0, 255]);

  const dstBackingOffset = 512;

  const w = new AerogpuCmdWriter();
  w.createTexture2d(1, 0, AerogpuFormat.R8G8B8A8Unorm, width, height, mipLevels, arrayLayers, mip0RowPitchBytes, 1, srcBackingOffset);
  w.createTexture2d(2, 0, AerogpuFormat.R8G8B8A8Unorm, width, height, mipLevels, arrayLayers, mip0RowPitchBytes, 1, dstBackingOffset);
  // Upload full mip+array chain from guest memory into the executor's internal state.
  w.resourceDirtyRange(1, 0n, BigInt(totalBackingSize));
  // Copy from src (layer=1,mip=1) into dst (layer=0,mip=1) and write back to guest memory.
  w.copyTexture2d(2, 1, 1, 0, 1, 1, 0, 0, 0, 0, 1, 1, AEROGPU_COPY_FLAG_WRITEBACK_DST);

  const state = createAerogpuCpuExecutorState();
  executeAerogpuCmdStream(state, w.finish().buffer, { allocTable, guestU8: guest });

  // Destination mip1/layer0 starts immediately after the padded mip0 rows.
  const dstPixelOff = dstBackingOffset + mip0Size;
  assert.deepEqual(Array.from(guest.subarray(dstPixelOff, dstPixelOff + 4)), [255, 255, 0, 255]);
});

test("ACMD binding table packets are ignored but validated by the browser executor", () => {
  const w = new AerogpuCmdWriter();
  w.createSampler(
    1,
    AerogpuSamplerFilter.Linear,
    AerogpuSamplerAddressMode.Repeat,
    AerogpuSamplerAddressMode.ClampToEdge,
    AerogpuSamplerAddressMode.MirrorRepeat,
  );
  w.setSamplers(AerogpuShaderStage.Pixel, 0, [1]);
  w.setConstantBuffers(AerogpuShaderStage.Vertex, 0, [{ buffer: 1, offsetBytes: 0, sizeBytes: 16 }]);
  w.destroySampler(1);

  const state = createAerogpuCpuExecutorState();
  executeAerogpuCmdStream(state, w.finish().buffer, { allocTable: null, guestU8: null });
});

test("ACMD SET_SAMPLERS rejects truncated handle payloads", () => {
  const w = new AerogpuCmdWriter();
  w.setSamplers(AerogpuShaderStage.Pixel, 0, [1]);
  const bytes = w.finish();
  // Patch sampler_count from 1 -> 2 without extending the packet.
  new DataView(bytes.buffer).setUint32(24 + 16, 2, true);

  const state = createAerogpuCpuExecutorState();
  assert.throws(
    () => executeAerogpuCmdStream(state, bytes.buffer, { allocTable: null, guestU8: null }),
    /SET_SAMPLERS/,
  );
});

test("ACMD SET_CONSTANT_BUFFERS rejects truncated binding payloads", () => {
  const w = new AerogpuCmdWriter();
  w.setConstantBuffers(AerogpuShaderStage.Vertex, 0, [{ buffer: 1, offsetBytes: 0, sizeBytes: 16 }]);
  const bytes = w.finish();
  // Patch buffer_count from 1 -> 2 without extending the packet.
  new DataView(bytes.buffer).setUint32(24 + 16, 2, true);

  const state = createAerogpuCpuExecutorState();
  assert.throws(
    () => executeAerogpuCmdStream(state, bytes.buffer, { allocTable: null, guestU8: null }),
    /SET_CONSTANT_BUFFERS/,
  );
});

test("ACMD FLUSH is accepted by the browser executor", () => {
  const w = new AerogpuCmdWriter();
  w.flush();

  const state = createAerogpuCpuExecutorState();
  executeAerogpuCmdStream(state, w.finish().buffer, { allocTable: null, guestU8: null });

  assert.equal(state.presentCount, 0n);
  assert.equal(state.textures.size, 0);
  assert.equal(state.buffers.size, 0);
});

test("ACMD FLUSH rejects undersized packets", () => {
  const w = new AerogpuCmdWriter();
  w.flush();
  const bytes = w.finish();
  const view = new DataView(bytes.buffer);
  // Truncate the stream to a header-only flush packet by shrinking both:
  // - cmd_stream_header.size_bytes
  // - cmd_hdr.size_bytes
  // This preserves iterator validity while simulating a guest bug.
  view.setUint32(8, 24 + 8, true);
  view.setUint32(24 + 4, 8, true);

  const state = createAerogpuCpuExecutorState();
  assert.throws(() => executeAerogpuCmdStream(state, bytes.buffer, { allocTable: null, guestU8: null }), /FLUSH/);
});

test("ACMD CREATE_TEXTURE2D rejects unsupported formats (e.g. BC) at creation time", () => {
  const w = new AerogpuCmdWriter();
  // Use a format value that is not in the CPU executor's allow-list. This is representative of
  // ABI 1.2+ block-compressed formats, which require the GPU backend.
  w.createTexture2d(1, 0, 0xffff_ffff, 1, 1, 1, 1, 0, 0, 0);

  const state = createAerogpuCpuExecutorState();
  assert.throws(
    () => executeAerogpuCmdStream(state, w.finish().buffer, { allocTable: null, guestU8: null }),
    /aerogpu: CREATE_TEXTURE2D unsupported format .*BC formats require GPU backend/,
  );
});

test(
  "ACMD CREATE_TEXTURE2D accepts *_UNORM_SRGB formats (CPU executor treats them like UNORM)",
  {
    // These enum members are introduced in ABI 1.2+. Skip this test when running against older
    // protocol mirrors.
    skip: typeof (AerogpuFormat as Record<string, unknown>).R8G8B8A8UnormSrgb !== "number",
  },
  () => {
    const fmt = (AerogpuFormat as Record<string, number>).R8G8B8A8UnormSrgb!;
    const w = new AerogpuCmdWriter();
    w.createTexture2d(1, 0, fmt, 1, 1, 1, 1, 0, 0, 0);
    w.uploadResource(1, 0n, Uint8Array.of(1, 2, 3, 4));

    const state = createAerogpuCpuExecutorState();
    executeAerogpuCmdStream(state, w.finish().buffer, { allocTable: null, guestU8: null });

    const tex = state.textures.get(1);
    assert(tex, "texture should exist");
    assert.deepEqual(Array.from(tex.data.subarray(0, 4)), [1, 2, 3, 4]);
  },
);

test(
  "ACMD CREATE_TEXTURE2D supports B8G8R8X8_UNORM_SRGB and preserves opaque alpha semantics",
  {
    // ABI 1.2+ only.
    skip: typeof (AerogpuFormat as Record<string, unknown>).B8G8R8X8UnormSrgb !== "number",
  },
  () => {
    const fmt = (AerogpuFormat as Record<string, number>).B8G8R8X8UnormSrgb!;
    const w = new AerogpuCmdWriter();
    w.createTexture2d(1, 0, fmt, 1, 1, 1, 1, 0, 0, 0);
    // Source bytes are B,G,R,X
    w.uploadResource(1, 0n, Uint8Array.of(1, 2, 3, 4));

    const state = createAerogpuCpuExecutorState();
    executeAerogpuCmdStream(state, w.finish().buffer, { allocTable: null, guestU8: null });

    const tex = state.textures.get(1);
    assert(tex, "texture should exist");
    // Stored bytes are always RGBA8. X8 formats should produce alpha=255.
    assert.deepEqual(Array.from(tex.data.subarray(0, 4)), [3, 2, 1, 255]);
  },
);
