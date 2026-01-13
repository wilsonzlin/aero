import { describe, expect, it } from "vitest";

import type { AsyncSectorDisk } from "./disk";
import { RUNTIME_DISK_MAX_IO_BYTES } from "./runtime_disk_limits";
import type { RuntimeDiskRequestMessage } from "./runtime_disk_protocol";
import { RuntimeDiskWorker, type OpenDiskFn } from "./runtime_disk_worker_impl";

describe("RuntimeDiskWorker (I/O size limits)", () => {
  it("rejects oversize reads before allocating", async () => {
    const posted: any[] = [];
    let readCalls = 0;

    const disk: AsyncSectorDisk = {
      sectorSize: 512,
      capacityBytes: 1024 * 1024,
      async readSectors(_lba, _buffer) {
        readCalls += 1;
      },
      async writeSectors() {},
      async flush() {},
    };

    const openDisk: OpenDiskFn = async () => ({ disk, readOnly: false, backendSnapshot: null });
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta: {} as any } },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);
    const handle = openResp.result.handle as number;

    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "read",
      payload: { handle, lba: 0, byteLength: RUNTIME_DISK_MAX_IO_BYTES + 1 },
    } satisfies RuntimeDiskRequestMessage);

    const readResp = posted.shift();
    expect(readResp.ok).toBe(false);
    expect(String(readResp.error.message)).toMatch(/read too large/i);
    expect(String(readResp.error.message)).toMatch(String(RUNTIME_DISK_MAX_IO_BYTES));
    expect(readCalls).toBe(0);
  });

  it("rejects oversize writes", async () => {
    const posted: any[] = [];
    let writeCalls = 0;

    const disk: AsyncSectorDisk = {
      sectorSize: 512,
      capacityBytes: 1024 * 1024,
      async readSectors() {},
      async writeSectors(_lba, _data) {
        writeCalls += 1;
      },
      async flush() {},
    };

    const openDisk: OpenDiskFn = async () => ({ disk, readOnly: false, backendSnapshot: null });
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta: {} as any } },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);
    const handle = openResp.result.handle as number;

    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "write",
      payload: { handle, lba: 0, data: new Uint8Array(RUNTIME_DISK_MAX_IO_BYTES + 1) },
    } satisfies RuntimeDiskRequestMessage);

    const writeResp = posted.shift();
    expect(writeResp.ok).toBe(false);
    expect(String(writeResp.error.message)).toMatch(/write too large/i);
    expect(String(writeResp.error.message)).toMatch(String(RUNTIME_DISK_MAX_IO_BYTES));
    expect(writeCalls).toBe(0);
  });
});

