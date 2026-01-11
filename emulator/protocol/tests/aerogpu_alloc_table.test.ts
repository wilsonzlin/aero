import assert from "node:assert/strict";
import test from "node:test";

import { AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32 } from "../aerogpu/aerogpu_pci.ts";
import {
  AEROGPU_ALLOC_ENTRY_SIZE,
  AEROGPU_ALLOC_FLAG_READONLY,
  AEROGPU_ALLOC_TABLE_HEADER_SIZE,
  AEROGPU_ALLOC_TABLE_MAGIC,
  decodeAllocTable,
} from "../aerogpu/aerogpu_ring.ts";

test("decodeAllocTable decodes a valid allocation table", () => {
  const entryCount = 2;
  const totalSize = AEROGPU_ALLOC_TABLE_HEADER_SIZE + entryCount * AEROGPU_ALLOC_ENTRY_SIZE;
  const buf = new ArrayBuffer(totalSize);
  const view = new DataView(buf);

  // Header.
  view.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(8, totalSize, true);
  view.setUint32(12, entryCount, true);
  view.setUint32(16, AEROGPU_ALLOC_ENTRY_SIZE, true);

  // Entry 0.
  const e0 = AEROGPU_ALLOC_TABLE_HEADER_SIZE;
  view.setUint32(e0 + 0, 10, true);
  view.setUint32(e0 + 4, AEROGPU_ALLOC_FLAG_READONLY, true);
  view.setBigUint64(e0 + 8, 0x1122334455667788n, true);
  view.setBigUint64(e0 + 16, 0x1000n, true);
  view.setBigUint64(e0 + 24, 0n, true);

  // Entry 1.
  const e1 = AEROGPU_ALLOC_TABLE_HEADER_SIZE + AEROGPU_ALLOC_ENTRY_SIZE;
  view.setUint32(e1 + 0, 20, true);
  view.setUint32(e1 + 4, 0, true);
  view.setBigUint64(e1 + 8, 0x8877665544332211n, true);
  view.setBigUint64(e1 + 16, 0x2000n, true);
  view.setBigUint64(e1 + 24, 0n, true);

  const decoded = decodeAllocTable(view);
  assert.equal(decoded.header.abiVersion, AEROGPU_ABI_VERSION_U32);
  assert.equal(decoded.header.sizeBytes, totalSize);
  assert.equal(decoded.header.entryCount, entryCount);
  assert.equal(decoded.header.entryStrideBytes, AEROGPU_ALLOC_ENTRY_SIZE);

  assert.equal(decoded.entries.length, 2);
  assert.equal(decoded.entries[0].allocId, 10);
  assert.equal(decoded.entries[0].flags, AEROGPU_ALLOC_FLAG_READONLY);
  assert.equal(decoded.entries[0].gpa, 0x1122334455667788n);
  assert.equal(decoded.entries[0].sizeBytes, 0x1000n);

  assert.equal(decoded.entries[1].allocId, 20);
  assert.equal(decoded.entries[1].flags, 0);
  assert.equal(decoded.entries[1].gpa, 0x8877665544332211n);
  assert.equal(decoded.entries[1].sizeBytes, 0x2000n);
});

test("decodeAllocTable rejects a too-small buffer", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE - 1);
  const view = new DataView(buf);
  assert.throws(() => decodeAllocTable(view), /Buffer too small/);
});

test("decodeAllocTable rejects bad magic", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  const view = new DataView(buf);
  view.setUint32(0, 0xdeadbeef, true);
  assert.throws(() => decodeAllocTable(view), /Bad alloc table magic/);
});

test("decodeAllocTable rejects unsupported ABI versions", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  const view = new DataView(buf);
  view.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(4, ((AEROGPU_ABI_MAJOR + 1) << 16) | AEROGPU_ABI_MINOR, true);
  view.setUint32(8, AEROGPU_ALLOC_TABLE_HEADER_SIZE, true);
  view.setUint32(12, 0, true);
  view.setUint32(16, AEROGPU_ALLOC_ENTRY_SIZE, true);
  assert.throws(() => decodeAllocTable(view), /Unsupported major/);
});

test("decodeAllocTable rejects invalid size_bytes", () => {
  // size_bytes < header size.
  {
    const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
    const view = new DataView(buf);
    view.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
    view.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
    view.setUint32(8, AEROGPU_ALLOC_TABLE_HEADER_SIZE - 1, true);
    assert.throws(() => decodeAllocTable(view), /size_bytes too small/);
  }

  // size_bytes > buffer length.
  {
    const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
    const view = new DataView(buf);
    view.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
    view.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
    view.setUint32(8, AEROGPU_ALLOC_TABLE_HEADER_SIZE + AEROGPU_ALLOC_ENTRY_SIZE, true);
    view.setUint32(12, 0, true);
    view.setUint32(16, AEROGPU_ALLOC_ENTRY_SIZE, true);
    assert.throws(() => decodeAllocTable(view), /Buffer too small for aerogpu_alloc_table/);
  }
});

test("decodeAllocTable rejects bad entry_stride_bytes", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  const view = new DataView(buf);
  view.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(8, AEROGPU_ALLOC_TABLE_HEADER_SIZE, true);
  view.setUint32(12, 0, true);
  view.setUint32(16, 16, true);
  assert.throws(() => decodeAllocTable(view), /entry_stride_bytes too small/);
});

test("decodeAllocTable rejects out-of-bounds entry_count", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  const view = new DataView(buf);
  view.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(8, AEROGPU_ALLOC_TABLE_HEADER_SIZE, true);
  view.setUint32(12, 1, true);
  view.setUint32(16, AEROGPU_ALLOC_ENTRY_SIZE, true);
  assert.throws(() => decodeAllocTable(view), /size_bytes too small for layout/);
});
