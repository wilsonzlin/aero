import { describe, expect, it } from "vitest";

import { parseIpcBuffer } from "../ipc/ipc";
import { RingBuffer } from "../ipc/ring_buffer";
import { RECORD_ALIGN, ringCtrl } from "../ipc/layout";
import { Worker, type WorkerOptions } from "node:worker_threads";
import { PCI_MMIO_BASE } from "../arch/guest_phys.ts";
import {
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_LEGACY_TEXT,
  SCANOUT_STATE_U32_LEN,
  snapshotScanoutState,
} from "../ipc/scanout_state";
import {
  COMMAND_RING_CAPACITY_BYTES,
  CONTROL_BYTES,
  CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES,
  EVENT_RING_CAPACITY_BYTES,
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  IO_IPC_HID_IN_QUEUE_KIND,
  IO_IPC_HID_IN_RING_CAPACITY_BYTES,
  IO_IPC_NET_RING_CAPACITY_BYTES,
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  IO_IPC_RING_CAPACITY_BYTES,
  RUNTIME_RESERVED_BYTES,
  STATUS_BYTES,
  STATUS_INTS,
  StatusIndex,
  WORKER_ROLES,
  allocateSharedMemorySegments,
  computeGuestRamLayout,
  createSharedMemoryViews,
  readGuestRamLayoutFromStatus,
  ringRegionsForWorker,
  setReadyFlag,
} from "./shared_layout";

describe("runtime/shared_layout", () => {
  // Most unit tests can use a tiny guest RAM size; when the guest memory is too
  // small to embed the demo shared framebuffer, the allocator falls back to a
  // standalone SharedArrayBuffer for the framebuffer.
  const TEST_GUEST_RAM_MIB = 1;

  it("places status + rings without overlap", () => {
    const regions: Array<{ name: string; start: number; end: number }> = [
      { name: "status", start: 0, end: STATUS_BYTES },
    ];

    const expectedCommandBytes = ringCtrl.BYTES + COMMAND_RING_CAPACITY_BYTES;
    const expectedEventBytes = ringCtrl.BYTES + EVENT_RING_CAPACITY_BYTES;

    for (const role of WORKER_ROLES) {
      const r = ringRegionsForWorker(role);
      expect(r.command.byteLength).toBe(expectedCommandBytes);
      expect(r.event.byteLength).toBe(expectedEventBytes);

      regions.push({
        name: `${role}.command`,
        start: r.command.byteOffset,
        end: r.command.byteOffset + r.command.byteLength,
      });
      regions.push({
        name: `${role}.event`,
        start: r.event.byteOffset,
        end: r.event.byteOffset + r.event.byteLength,
      });
    }

    for (const region of regions) {
      expect(region.start).toBeGreaterThanOrEqual(0);
      expect(region.end).toBeGreaterThan(region.start);
      expect(region.end).toBeLessThanOrEqual(CONTROL_BYTES);
      expect(region.start % 4).toBe(0);
    }

    const sorted = regions.slice().sort((a, b) => a.start - b.start);
    for (let i = 1; i < sorted.length; i++) {
      const prev = sorted[i - 1];
      const cur = sorted[i];
      expect(cur.start).toBeGreaterThanOrEqual(prev.end);
    }
  });

  it("initializes ring headers using the AIPC layout", () => {
    expect(COMMAND_RING_CAPACITY_BYTES % RECORD_ALIGN).toBe(0);
    expect(EVENT_RING_CAPACITY_BYTES % RECORD_ALIGN).toBe(0);

    const segments = allocateSharedMemorySegments({ guestRamMiB: TEST_GUEST_RAM_MIB });
    for (const role of WORKER_ROLES) {
      const regions = ringRegionsForWorker(role);

      const cmdCtrl = new Int32Array(segments.control, regions.command.byteOffset, ringCtrl.WORDS);
      expect(Array.from(cmdCtrl)).toEqual([0, 0, 0, COMMAND_RING_CAPACITY_BYTES]);

      const evtCtrl = new Int32Array(segments.control, regions.event.byteOffset, ringCtrl.WORDS);
      expect(Array.from(evtCtrl)).toEqual([0, 0, 0, EVENT_RING_CAPACITY_BYTES]);
    }
  });

  it("transfers messages across threads using a shared_layout ring", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: TEST_GUEST_RAM_MIB });
    const regions = ringRegionsForWorker("cpu");
    const ring = new RingBuffer(segments.control, regions.command.byteOffset);

    const count = 100;
    const worker = new Worker(new URL("./shared_layout_ring_consumer_worker.ts", import.meta.url), {
      type: "module",
      workerData: { sab: segments.control, offsetBytes: regions.command.byteOffset, count },
      execArgv: ["--experimental-strip-types"],
    } as unknown as WorkerOptions);

    try {
      for (let i = 0; i < count; i++) {
        const payload = new Uint8Array(4);
        new DataView(payload.buffer).setUint32(0, i, true);
        while (!ring.tryPush(payload)) {
          await new Promise<void>((resolve) => setTimeout(resolve, 0));
        }
      }

      const received = await new Promise<number[]>((resolve, reject) => {
        worker.once("message", (msg) => resolve(msg as number[]));
        worker.once("error", reject);
        worker.once("exit", (code) => {
          if (code !== 0) reject(new Error(`ring buffer worker exited with code ${code}`));
        });
      });

      expect(received).toEqual(Array.from({ length: count }, (_, i) => i));
    } finally {
      await worker.terminate();
    }
  });

  it("creates shared views for control + guest memory", () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: TEST_GUEST_RAM_MIB });
    const views = createSharedMemoryViews(segments);

    expect(views.status.byteOffset).toBe(0);
    expect(views.status.byteLength).toBe(STATUS_BYTES);
    expect(views.guestLayout.guest_base).toBe(RUNTIME_RESERVED_BYTES);
    expect(views.guestLayout.guest_size).toBe(TEST_GUEST_RAM_MIB * 1024 * 1024);
    expect(views.guestU8.byteOffset).toBe(views.guestLayout.guest_base);
    expect(views.guestU8.byteLength).toBe(views.guestLayout.guest_size);
    expect(views.guestU8.buffer).toBe(segments.guestMemory.buffer);
    expect(views.sharedFramebuffer).toBe(segments.sharedFramebuffer);
    expect(views.sharedFramebufferOffsetBytes).toBe(segments.sharedFramebufferOffsetBytes);
  });

  it("allocates and initializes scanoutState", () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: TEST_GUEST_RAM_MIB });
    expect(segments.scanoutState).toBeInstanceOf(SharedArrayBuffer);
    expect(segments.scanoutStateOffsetBytes).toBe(0);

    const words = new Int32Array(segments.scanoutState!, 0, SCANOUT_STATE_U32_LEN);
    const snap = snapshotScanoutState(words);
    expect(snap.generation).toBe(0);
    expect(snap.source).toBe(SCANOUT_SOURCE_LEGACY_TEXT);
    expect(snap.format).toBe(SCANOUT_FORMAT_B8G8R8X8);
  });

  it("clamps maximum guest RAM size below the PCI MMIO BAR window", () => {
    // Requesting "as much as possible" (u32 max) should clamp to the start of the fixed
    // PCI MMIO BAR window so PCI BARs can never overlap guest RAM.
    const layout = computeGuestRamLayout(0xffff_ffff);
    expect(layout.guest_size).toBeLessThanOrEqual(PCI_MMIO_BASE);
    expect(layout.guest_size).toBe(PCI_MMIO_BASE);
  });

  it("rejects status guest RAM layouts that overlap the PCI MMIO BAR window", () => {
    const status = new Int32Array(new SharedArrayBuffer(STATUS_BYTES));
    Atomics.store(status, StatusIndex.GuestBase, RUNTIME_RESERVED_BYTES | 0);
    // Intentionally write a value larger than PCI_MMIO_BASE to validate the guard.
    Atomics.store(status, StatusIndex.GuestSize, 0xf000_0000 | 0);
    expect(() => readGuestRamLayoutFromStatus(status)).toThrow(/PCI MMIO/i);
  });

  it("falls back to standalone shared framebuffer when guest RAM is too small to embed it", () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });
    expect(segments.sharedFramebuffer).not.toBe(segments.guestMemory.buffer);
    expect(segments.sharedFramebufferOffsetBytes).toBe(0);
  });

  it("embeds shared framebuffer in guest memory when there is enough guest RAM", () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 16 });
    expect(segments.sharedFramebuffer).toBe(segments.guestMemory.buffer);
    expect(segments.sharedFramebufferOffsetBytes).toBe(RUNTIME_RESERVED_BYTES + CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES);
  });

  it("allocates ioIpc AIPC queues for device I/O + raw Ethernet frames", () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: TEST_GUEST_RAM_MIB });
    const { queues } = parseIpcBuffer(segments.ioIpc);

    expect(queues.map((q) => q.kind).sort((a, b) => a - b)).toEqual([
      IO_IPC_CMD_QUEUE_KIND,
      IO_IPC_EVT_QUEUE_KIND,
      IO_IPC_NET_TX_QUEUE_KIND,
      IO_IPC_NET_RX_QUEUE_KIND,
      IO_IPC_HID_IN_QUEUE_KIND,
    ]);

    const caps = new Map(queues.map((q) => [q.kind, q.capacityBytes]));
    expect(caps.get(IO_IPC_CMD_QUEUE_KIND)).toBe(IO_IPC_RING_CAPACITY_BYTES);
    expect(caps.get(IO_IPC_EVT_QUEUE_KIND)).toBe(IO_IPC_RING_CAPACITY_BYTES);
    expect(caps.get(IO_IPC_NET_TX_QUEUE_KIND)).toBe(IO_IPC_NET_RING_CAPACITY_BYTES);
    expect(caps.get(IO_IPC_NET_RX_QUEUE_KIND)).toBe(IO_IPC_NET_RING_CAPACITY_BYTES);
    expect(caps.get(IO_IPC_HID_IN_QUEUE_KIND)).toBe(IO_IPC_HID_IN_RING_CAPACITY_BYTES);
  });

  it("sets worker ready flags without overlapping status indices", () => {
    // Keep existing indices stable: changing these breaks the runtime ABI and
    // can corrupt shared status reads across workers.
    expect(StatusIndex.CpuReady).toBe(8);
    expect(StatusIndex.GpuReady).toBe(9);
    expect(StatusIndex.IoReady).toBe(10);
    expect(StatusIndex.JitReady).toBe(11);
    expect(StatusIndex.NetReady).toBe(15);
    expect(StatusIndex.IoHidInputReportDropCounter).not.toBe(StatusIndex.NetReady);

    const status = new Int32Array(new SharedArrayBuffer(STATUS_BYTES));
    for (const role of WORKER_ROLES) {
      setReadyFlag(status, role, true);
    }

    expect(Atomics.load(status, StatusIndex.CpuReady)).toBe(1);
    expect(Atomics.load(status, StatusIndex.GpuReady)).toBe(1);
    expect(Atomics.load(status, StatusIndex.IoReady)).toBe(1);
    expect(Atomics.load(status, StatusIndex.JitReady)).toBe(1);
    expect(Atomics.load(status, StatusIndex.NetReady)).toBe(1);

    for (const role of WORKER_ROLES) {
      setReadyFlag(status, role, false);
    }
    expect(Atomics.load(status, StatusIndex.CpuReady)).toBe(0);
    expect(Atomics.load(status, StatusIndex.GpuReady)).toBe(0);
    expect(Atomics.load(status, StatusIndex.IoReady)).toBe(0);
    expect(Atomics.load(status, StatusIndex.JitReady)).toBe(0);
    expect(Atomics.load(status, StatusIndex.NetReady)).toBe(0);
  });

  it("defines unique status indices within bounds", () => {
    const values = Object.values(StatusIndex);
    const unique = new Set(values);
    expect(unique.size).toBe(values.length);
    for (const idx of unique) {
      expect(idx).toBeGreaterThanOrEqual(0);
      expect(idx).toBeLessThan(STATUS_INTS);
    }
  });
});
