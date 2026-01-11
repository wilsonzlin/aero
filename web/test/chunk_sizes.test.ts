import test from "node:test";
import assert from "node:assert/strict";

import { CHUNKED_DISK_CHUNK_SIZE, RANGE_STREAM_CHUNK_SIZE } from "../src/storage/chunk_sizes.ts";
import { EXPORT_CHUNK_SIZE, IDB_CHUNK_SIZE } from "../src/storage/import_export.ts";

test("storage chunk-size defaults are stable and internally consistent", () => {
  assert.equal(RANGE_STREAM_CHUNK_SIZE, 1024 * 1024);
  assert.equal(CHUNKED_DISK_CHUNK_SIZE, 4 * 1024 * 1024);

  // Back-compat exports used by existing code paths.
  assert.equal(EXPORT_CHUNK_SIZE, RANGE_STREAM_CHUNK_SIZE);
  assert.equal(IDB_CHUNK_SIZE, CHUNKED_DISK_CHUNK_SIZE);
});

