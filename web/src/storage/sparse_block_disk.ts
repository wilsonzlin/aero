import type { AsyncSectorDisk } from "./disk";

/**
 * A sparse disk that exposes fixed-size blocks.
 *
 * This is primarily used for copy-on-write overlays and remote download caches.
 *
 * `OpfsAeroSparseDisk` is the main production implementation, but tests can
 * provide an in-memory variant.
 *
 * Canonical trait note:
 * `AsyncSectorDisk` is the canonical TS disk interface in this repo. This interface is a
 * specialized extension for sparse/overlay use cases; prefer taking `AsyncSectorDisk` in APIs
 * unless you specifically need block allocation semantics.
 *
 * See `docs/20-storage-trait-consolidation.md`.
 */
export interface SparseBlockDisk extends AsyncSectorDisk {
  readonly blockSizeBytes: number;

  isBlockAllocated(blockIndex: number): boolean;
  readBlock(blockIndex: number, dst: Uint8Array): Promise<void>;
  writeBlock(blockIndex: number, data: Uint8Array): Promise<void>;
}
