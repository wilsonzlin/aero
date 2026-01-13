import { RUNTIME_DISK_MAX_IO_BYTES } from "../storage/runtime_disk_limits";
import type { SharedArrayBufferRange, SharedArrayBufferSlice } from "../storage/runtime_disk_protocol";

export type RuntimeDiskClientLike = {
  read(handle: number, lba: number, byteLength: number): Promise<Uint8Array>;
  readInto?(handle: number, lba: number, byteLength: number, dest: SharedArrayBufferSlice): Promise<void>;
  write(handle: number, lba: number, data: Uint8Array): Promise<void>;
  writeFrom?(handle: number, lba: number, src: SharedArrayBufferRange): Promise<void>;
};

export type AlignedDiskIoRange = {
  lba: number;
  /**
   * Total aligned byte length (multiple of sectorSize).
   *
   * This may be larger than the guest-requested length when diskOffset/len are
   * not sector-aligned.
   */
  byteLength: number;
  /**
   * Offset (in bytes) from the start of the aligned read/write buffer to the
   * guest-requested start byte.
   */
  offset: number;
};

export function computeAlignedDiskIoRange(
  diskOffset: bigint,
  lenU32: number,
  sectorSize: number,
): AlignedDiskIoRange | null {
  if (sectorSize <= 0) return null;
  const sectorSizeBig = BigInt(sectorSize);
  const startLbaBig = diskOffset / sectorSizeBig;
  const offsetBig = diskOffset % sectorSizeBig;
  if (startLbaBig > BigInt(Number.MAX_SAFE_INTEGER)) return null;

  const endByte = diskOffset + BigInt(lenU32);
  const endLbaBig = lenU32 === 0 ? startLbaBig : (endByte + sectorSizeBig - 1n) / sectorSizeBig;
  const sectorsBig = endLbaBig - startLbaBig;
  const byteLengthBig = sectorsBig * sectorSizeBig;
  if (byteLengthBig > BigInt(Number.MAX_SAFE_INTEGER)) return null;

  return { lba: Number(startLbaBig), byteLength: Number(byteLengthBig), offset: Number(offsetBig) };
}

function computeMaxChunkBytes(sectorSize: number, maxIoBytes: number): number {
  if (!Number.isFinite(maxIoBytes) || maxIoBytes <= 0) {
    throw new Error(`invalid maxIoBytes=${String(maxIoBytes)}`);
  }
  if (!Number.isFinite(sectorSize) || sectorSize <= 0) {
    throw new Error(`invalid sectorSize=${String(sectorSize)}`);
  }
  const maxChunkBytes = Math.floor(maxIoBytes / sectorSize) * sectorSize;
  if (maxChunkBytes <= 0) {
    throw new Error(`maxIoBytes ${maxIoBytes} too small for sectorSize ${sectorSize}`);
  }
  return maxChunkBytes;
}

export async function diskReadIntoGuest(opts: {
  client: RuntimeDiskClientLike;
  handle: number;
  range: AlignedDiskIoRange;
  sectorSize: number;
  guestView: Uint8Array;
  maxIoBytes?: number;
}): Promise<{ readBytes: number }> {
  const { client, handle, range, sectorSize, guestView } = opts;
  const maxChunkBytes = computeMaxChunkBytes(sectorSize, opts.maxIoBytes ?? RUNTIME_DISK_MAX_IO_BYTES);

  const requestedLen = guestView.byteLength;
  const requestedStart = range.offset;
  const requestedEnd = requestedStart + requestedLen;

  let readBytes = 0;

  const guestBuf = guestView.buffer;
  if (
    typeof client.readInto === "function" &&
    requestedLen > 0 &&
    range.offset === 0 &&
    requestedLen === range.byteLength &&
    // `SharedArrayBuffer` may not exist in non-COI environments.
    typeof SharedArrayBuffer !== "undefined" &&
    guestBuf instanceof SharedArrayBuffer
  ) {
    let chunkStart = 0;
    while (chunkStart < requestedLen) {
      const remaining = requestedLen - chunkStart;
      const chunkBytes = Math.min(maxChunkBytes, remaining);
      const chunkLba = range.lba + chunkStart / sectorSize;
      await client.readInto!(handle, chunkLba, chunkBytes, {
        sab: guestBuf,
        offsetBytes: guestView.byteOffset + chunkStart,
      });
      readBytes += chunkBytes;
      chunkStart += chunkBytes;
    }
    return { readBytes };
  }

  let chunkStart = 0;
  while (chunkStart < range.byteLength) {
    const remaining = range.byteLength - chunkStart;
    const chunkBytes = Math.min(maxChunkBytes, remaining);
    const chunkLba = range.lba + chunkStart / sectorSize;

    const data = await client.read(handle, chunkLba, chunkBytes);
    if (data.byteLength !== chunkBytes) {
      throw new Error(`runtime disk read returned ${data.byteLength} bytes (expected ${chunkBytes})`);
    }
    readBytes += data.byteLength;

    const chunkEnd = chunkStart + chunkBytes;
    const copyStart = Math.max(chunkStart, requestedStart);
    const copyEnd = Math.min(chunkEnd, requestedEnd);
    if (copyStart < copyEnd) {
      const from = copyStart - chunkStart;
      const to = copyStart - requestedStart;
      guestView.set(data.subarray(from, from + (copyEnd - copyStart)), to);
    }

    chunkStart += chunkBytes;
  }

  return { readBytes };
}

export async function diskWriteFromGuest(opts: {
  client: RuntimeDiskClientLike;
  handle: number;
  range: AlignedDiskIoRange;
  sectorSize: number;
  guestView: Uint8Array;
  maxIoBytes?: number;
}): Promise<{ readBytes: number; writtenBytes: number }> {
  const { client, handle, range, sectorSize, guestView } = opts;
  const maxChunkBytes = computeMaxChunkBytes(sectorSize, opts.maxIoBytes ?? RUNTIME_DISK_MAX_IO_BYTES);

  const requestedLen = guestView.byteLength;
  const aligned = range.offset === 0 && requestedLen % sectorSize === 0;

  let readBytes = 0;
  let writtenBytes = 0;

  if (requestedLen === 0) {
    return { readBytes: 0, writtenBytes: 0 };
  }

  if (aligned) {
    const guestBuf = guestView.buffer;
    if (
      typeof client.writeFrom === "function" &&
      // `SharedArrayBuffer` may not exist in non-COI environments.
      typeof SharedArrayBuffer !== "undefined" &&
      guestBuf instanceof SharedArrayBuffer
    ) {
      let chunkStart = 0;
      while (chunkStart < requestedLen) {
        const remaining = requestedLen - chunkStart;
        const chunkBytes = Math.min(maxChunkBytes, remaining);
        const chunkLba = range.lba + chunkStart / sectorSize;
        await client.writeFrom!(handle, chunkLba, {
          sab: guestBuf,
          offsetBytes: guestView.byteOffset + chunkStart,
          byteLength: chunkBytes,
        });
        writtenBytes += chunkBytes;
        chunkStart += chunkBytes;
      }
      return { readBytes: 0, writtenBytes };
    }

    let chunkStart = 0;
    while (chunkStart < requestedLen) {
      const remaining = requestedLen - chunkStart;
      const chunkBytes = Math.min(maxChunkBytes, remaining);
      const chunkLba = range.lba + chunkStart / sectorSize;
      const chunk = guestView.subarray(chunkStart, chunkStart + chunkBytes);
      const ioBytes = chunk.byteLength;
      await client.write(handle, chunkLba, chunk);
      writtenBytes += ioBytes;
      chunkStart += chunkBytes;
    }
    return { readBytes: 0, writtenBytes };
  }

  const requestedStart = range.offset;
  const requestedEnd = requestedStart + requestedLen;

  let chunkStart = 0;
  while (chunkStart < range.byteLength) {
    const remaining = range.byteLength - chunkStart;
    const chunkBytes = Math.min(maxChunkBytes, remaining);
    const chunkLba = range.lba + chunkStart / sectorSize;

    const buf = await client.read(handle, chunkLba, chunkBytes);
    if (buf.byteLength !== chunkBytes) {
      throw new Error(`runtime disk read returned ${buf.byteLength} bytes (expected ${chunkBytes})`);
    }
    readBytes += buf.byteLength;

    const chunkEnd = chunkStart + chunkBytes;
    const copyStart = Math.max(chunkStart, requestedStart);
    const copyEnd = Math.min(chunkEnd, requestedEnd);
    if (copyStart < copyEnd) {
      const guestOffset = copyStart - requestedStart;
      const bufOffset = copyStart - chunkStart;
      buf.set(guestView.subarray(guestOffset, guestOffset + (copyEnd - copyStart)), bufOffset);
    }

    // `RuntimeDiskClient.write` may transfer/detach `buf` when it is backed by a standalone
    // ArrayBuffer. Capture size before calling write() so callers can keep correct accounting.
    const ioBytes = buf.byteLength;
    await client.write(handle, chunkLba, buf);
    writtenBytes += ioBytes;

    chunkStart += chunkBytes;
  }

  return { readBytes, writtenBytes };
}
