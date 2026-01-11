/**
 * Authoritative chunk size defaults used by the storage subsystem.
 *
 * We intentionally use different defaults per delivery mode:
 * - HTTP Range streaming: smaller fetch unit to limit over-fetch on random I/O.
 * - No-Range chunked delivery: larger objects to reduce request/object count.
 *
 * Keep these in sync with:
 * - docs/05-storage-subsystem.md
 * - docs/16-disk-image-streaming-auth.md
 * - docs/18-chunked-disk-image-format.md
 * - tools/image-chunker (CLI default)
 */

/** Default fetch unit for HTTP `Range`-based remote disk streaming (1 MiB). */
export const RANGE_STREAM_CHUNK_SIZE = 1024 * 1024;

/** Default object size for the no-Range chunked disk image format (4 MiB). */
export const CHUNKED_DISK_CHUNK_SIZE = 4 * 1024 * 1024;

