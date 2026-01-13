// Defensive per-request I/O cap for `RuntimeDiskWorker` "read"/"write" operations.
//
// The runtime disk worker allocates a `Uint8Array(byteLength)` for reads, so an
// attacker-controlled `byteLength` can OOM the worker by requesting multi-GB
// reads. Writes have a similar issue when a large ArrayBuffer is transferred
// into the worker.
//
// Guest-visible disk DMA in `web/src/workers/io.worker.ts` is chunked so that
// each runtime-disk request stays within this limit.
export const RUNTIME_DISK_MAX_IO_BYTES = 16 * 1024 * 1024; // 16 MiB

