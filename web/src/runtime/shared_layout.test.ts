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
import { computeSharedFramebufferLayout, FramebufferFormat } from "../ipc/shared-layout";
import {
  CURSOR_FORMAT_B8G8R8A8,
  CURSOR_STATE_U32_LEN,
  snapshotCursorState,
} from "../ipc/cursor_state";
import { allocateHarnessSharedMemorySegments } from "./harness_shared_memory";
import { makeNodeWorkerExecArgv } from "../test_utils/worker_threads_exec_argv";
import {
  COMMAND_RING_CAPACITY_BYTES,
  CONTROL_BYTES,
  CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES,
  CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
  CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
  CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
  EVENT_RING_CAPACITY_BYTES,
  HIGH_RAM_START,
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  IO_IPC_HID_IN_QUEUE_KIND,
  IO_IPC_HID_IN_RING_CAPACITY_BYTES,
  IO_IPC_NET_RING_CAPACITY_BYTES,
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  IO_IPC_RING_CAPACITY_BYTES,
  LOW_RAM_END,
  RUNTIME_RESERVED_BYTES,
  STATUS_BYTES,
  STATUS_INTS,
  StatusIndex,
  WORKER_ROLES,
  createIoIpcSab,
  allocateSharedMemorySegments,
  computeGuestRamLayout,
  createSharedMemoryViews,
  guestPaddrToRamOffset,
  guestRangeInBounds,
  guestToLinear,
  readGuestRamLayoutFromStatus,
  ringRegionsForWorker,
  setReadyFlag,
  type GuestRamLayout,
} from "./shared_layout";

describe("runtime/shared_layout", () => {
  // Most unit tests can use a tiny guest RAM size; when the guest memory is too
  // small to embed the demo shared framebuffer, the allocator falls back to a
  // standalone SharedArrayBuffer for the framebuffer.
  const TEST_GUEST_RAM_MIB = 1;
  const TEST_VRAM_MIB = 1;
  // `allocateSharedMemorySegments` always allocates the full wasm32 runtime-reserved region
  // (~128MiB) in addition to guest RAM. Cache the result so we only pay that cost once for
  // this suite (these SharedArrayBuffer/WebAssembly.Memory allocations are not guaranteed to
  // be promptly released by the JS runtime).
  let baseSegments: ReturnType<typeof allocateSharedMemorySegments> | null = null;
  const getBaseSegments = (): ReturnType<typeof allocateSharedMemorySegments> => {
    if (!baseSegments) {
      // Unit tests don't need NET/HID AIPC rings; omit them to keep the cached shared allocations
      // as small as possible.
      baseSegments = allocateSharedMemorySegments({
        guestRamMiB: TEST_GUEST_RAM_MIB,
        vramMiB: TEST_VRAM_MIB,
        ioIpcOptions: { includeNet: false, includeHidIn: false },
        sharedFramebufferLayout: { width: 1, height: 1, tileSize: 0 },
      });
    }
    return baseSegments;
  };

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

    const segments = getBaseSegments();
    for (const role of WORKER_ROLES) {
      const regions = ringRegionsForWorker(role);

      const cmdCtrl = new Int32Array(segments.control, regions.command.byteOffset, ringCtrl.WORDS);
      expect(Array.from(cmdCtrl)).toEqual([0, 0, 0, COMMAND_RING_CAPACITY_BYTES]);

      const evtCtrl = new Int32Array(segments.control, regions.event.byteOffset, ringCtrl.WORDS);
      expect(Array.from(evtCtrl)).toEqual([0, 0, 0, EVENT_RING_CAPACITY_BYTES]);
    }
  });

  it(
    "transfers messages across threads using a shared_layout ring",
    async () => {
      // This test exercises the shared ring buffer logic; it does not require the full runtime
      // allocator (which reserves a large wasm32 runtime region). Use the harness allocator to
      // keep memory usage low.
      const segments = allocateHarnessSharedMemorySegments({
        guestRamBytes: 64 * 1024,
        sharedFramebuffer: new SharedArrayBuffer(8),
        sharedFramebufferOffsetBytes: 0,
        ioIpcBytes: 0,
        vramBytes: 0,
      });
      const regions = ringRegionsForWorker("cpu");
      const ring = new RingBuffer(segments.control, regions.command.byteOffset);

      const count = 100;
      const worker = new Worker(new URL("./shared_layout_ring_consumer_worker.ts", import.meta.url), {
        type: "module",
        workerData: { sab: segments.control, offsetBytes: regions.command.byteOffset, count },
        execArgv: makeNodeWorkerExecArgv(),
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
    },
    // Spawning a worker thread and shuttling 100 messages can take longer under
    // heavy CI load (or when the full suite is running in parallel).
    20_000,
  );

  it("creates shared views for control + guest memory", () => {
    const segments = getBaseSegments();
    const views = createSharedMemoryViews(segments);

    expect(views.status.byteOffset).toBe(0);
    expect(views.status.byteLength).toBe(STATUS_BYTES);
    expect(views.guestLayout.guest_base).toBe(RUNTIME_RESERVED_BYTES);
    expect(views.guestLayout.guest_size).toBe(TEST_GUEST_RAM_MIB * 1024 * 1024);
    expect(views.guestU8.byteOffset).toBe(views.guestLayout.guest_base);
    expect(views.guestU8.byteLength).toBe(views.guestLayout.guest_size);
    expect(views.guestU8.buffer).toBe(segments.guestMemory.buffer);
    expect(segments.vram).toBeInstanceOf(SharedArrayBuffer);
    expect(segments.vram!.byteLength).toBe(TEST_VRAM_MIB * 1024 * 1024);
    expect(views.vramSizeBytes).toBe(TEST_VRAM_MIB * 1024 * 1024);
    expect(views.vramU8.byteLength).toBe(TEST_VRAM_MIB * 1024 * 1024);
    expect(views.vramU8.buffer).toBe(segments.vram);
    expect(views.sharedFramebuffer).toBe(segments.sharedFramebuffer);
    expect(views.sharedFramebufferOffsetBytes).toBe(segments.sharedFramebufferOffsetBytes);
  });

  it("does not allocate a separate vgaFramebuffer segment (legacy scanout uses sharedFramebuffer)", () => {
    const segments = getBaseSegments();
    // Historical field; should be absent so workers can't dead-write into an unused region.
    expect((segments as unknown as { vgaFramebuffer?: unknown }).vgaFramebuffer).toBeUndefined();
  });

  it("allocates and initializes scanoutState", () => {
    const segments = getBaseSegments();
    expect(segments.scanoutState).toBeInstanceOf(SharedArrayBuffer);

    // ScanoutState is embedded inside the shared WebAssembly.Memory so WASM can update it directly.
    // Keep in sync with `web/src/runtime/shared_layout.ts`.
    const expectedOffsetBytes = RUNTIME_RESERVED_BYTES - (64 + SCANOUT_STATE_U32_LEN * 4 + CURSOR_STATE_U32_LEN * 4);
    expect(segments.scanoutState).toBe(segments.guestMemory.buffer);
    expect(segments.scanoutStateOffsetBytes).toBe(expectedOffsetBytes);

    const words = new Int32Array(segments.scanoutState!, expectedOffsetBytes, SCANOUT_STATE_U32_LEN);
    const snap = snapshotScanoutState(words);
    expect(snap.generation).toBe(0);
    expect(snap.source).toBe(SCANOUT_SOURCE_LEGACY_TEXT);
    expect(snap.format).toBe(SCANOUT_FORMAT_B8G8R8X8);
  });

  it("allocates and initializes cursorState", () => {
    const segments = getBaseSegments();
    expect(segments.cursorState).toBeInstanceOf(SharedArrayBuffer);

    // CursorState is embedded in the same wasm linear memory tail region as ScanoutState.
    const expectedScanoutOffsetBytes = RUNTIME_RESERVED_BYTES - (64 + SCANOUT_STATE_U32_LEN * 4 + CURSOR_STATE_U32_LEN * 4);
    const expectedCursorOffsetBytes = expectedScanoutOffsetBytes + SCANOUT_STATE_U32_LEN * 4;
    expect(segments.cursorState).toBe(segments.guestMemory.buffer);
    expect(segments.cursorStateOffsetBytes).toBe(expectedCursorOffsetBytes);

    const words = new Int32Array(segments.cursorState!, expectedCursorOffsetBytes, CURSOR_STATE_U32_LEN);
    const snap = snapshotCursorState(words);
    expect(snap.generation).toBe(0);
    expect(snap.enable).toBe(0);
    expect(snap.x).toBe(0);
    expect(snap.y).toBe(0);
    expect(snap.format).toBe(CURSOR_FORMAT_B8G8R8A8);
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
    const segments = getBaseSegments();
    expect(segments.sharedFramebuffer).not.toBe(segments.guestMemory.buffer);
    expect(segments.sharedFramebufferOffsetBytes).toBe(0);
  });

  it("embeds shared framebuffer in guest memory when there is enough guest RAM", () => {
    const demoLayout = computeSharedFramebufferLayout(
      CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
      CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
      CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH * 4,
      FramebufferFormat.RGBA8,
      CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
    );
    const requiredGuestBytes = CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES + demoLayout.totalBytes;
    const guestRamMiB = Math.ceil(requiredGuestBytes / (1024 * 1024));
    const segments = allocateSharedMemorySegments({
      guestRamMiB,
      vramMiB: 0,
      ioIpcOptions: { includeNet: false, includeHidIn: false },
    });
    expect(segments.sharedFramebuffer).toBe(segments.guestMemory.buffer);
    expect(segments.sharedFramebufferOffsetBytes).toBe(RUNTIME_RESERVED_BYTES + CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES);
  });

  it("can allocate a minimal ioIpc SAB (CMD/EVT only)", () => {
    const segments = getBaseSegments();
    const { queues } = parseIpcBuffer(segments.ioIpc);

    expect(queues.map((q) => q.kind).sort((a, b) => a - b)).toEqual([IO_IPC_CMD_QUEUE_KIND, IO_IPC_EVT_QUEUE_KIND]);

    const caps = new Map(queues.map((q) => [q.kind, q.capacityBytes]));
    expect(caps.get(IO_IPC_CMD_QUEUE_KIND)).toBe(IO_IPC_RING_CAPACITY_BYTES);
    expect(caps.get(IO_IPC_EVT_QUEUE_KIND)).toBe(IO_IPC_RING_CAPACITY_BYTES);
  });

  it("allocates ioIpc AIPC queues for device I/O + raw Ethernet frames (default)", () => {
    const { queues } = parseIpcBuffer(createIoIpcSab());

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

  it("defines unique StatusIndex values (no ABI collisions)", () => {
    const values = Object.values(StatusIndex) as number[];
    expect(values.length).toBeGreaterThan(0);
    expect(new Set(values).size).toBe(values.length);
    for (const value of values) {
      expect(value).toBeGreaterThanOrEqual(0);
      expect(value).toBeLessThan(STATUS_INTS);
    }
  });

  describe("guest physical address translation (PC/Q35 low RAM + hole + high RAM remap)", () => {
    function layoutForTesting(guest_size: number, guest_base = 0): GuestRamLayout {
      return {
        guest_base,
        guest_size,
        runtime_reserved: guest_base,
        wasm_pages: 0,
      };
    }

    it("guestPaddrToRamOffset: identity mapping for small RAM (guest_size <= LOW_RAM_END)", () => {
      const ram = 0x2000;
      const layout = layoutForTesting(ram);

      expect(guestPaddrToRamOffset(layout, 0)).toBe(0);
      expect(guestPaddrToRamOffset(layout, 0x1234)).toBe(0x1234);
      expect(guestPaddrToRamOffset(layout, ram - 1)).toBe(ram - 1);
      // End is out of range for a single address.
      expect(guestPaddrToRamOffset(layout, ram)).toBeNull();
    });

    it("guestRangeInBounds: end boundary + zero-length semantics for small RAM", () => {
      const ram = 0x2000;
      const layout = layoutForTesting(ram);

      expect(guestRangeInBounds(layout, 0, 0)).toBe(true);
      expect(guestRangeInBounds(layout, 0, 1)).toBe(true);
      expect(guestRangeInBounds(layout, 0x1234, 4)).toBe(true);
      expect(guestRangeInBounds(layout, ram - 1, 1)).toBe(true);

      // Out-of-bounds for non-empty ranges.
      expect(guestRangeInBounds(layout, ram, 1)).toBe(false);
      expect(guestRangeInBounds(layout, ram - 1, 2)).toBe(false);

      // Empty slice at end is OK (mirrors slice indexing semantics).
      expect(guestRangeInBounds(layout, ram, 0)).toBe(true);
      expect(guestRangeInBounds(layout, ram + 1, 0)).toBe(false);
    });

    it("guestPaddrToRamOffset: rejects hole and remaps high RAM when guest_size > LOW_RAM_END", () => {
      const ram = LOW_RAM_END + 0x2000;
      const layout = layoutForTesting(ram);

      // Low RAM is identity-mapped.
      expect(guestPaddrToRamOffset(layout, LOW_RAM_END - 4)).toBe(LOW_RAM_END - 4);

      // ECAM/PCI/MMIO hole is not backed by RAM.
      expect(guestPaddrToRamOffset(layout, LOW_RAM_END)).toBeNull();
      expect(guestPaddrToRamOffset(layout, LOW_RAM_END + 0x1000)).toBeNull();
      expect(guestPaddrToRamOffset(layout, HIGH_RAM_START - 4)).toBeNull();

      // High RAM is remapped above 4GiB: physical 4GiB corresponds to RAM offset LOW_RAM_END.
      expect(guestPaddrToRamOffset(layout, HIGH_RAM_START)).toBe(LOW_RAM_END);
      expect(guestPaddrToRamOffset(layout, HIGH_RAM_START + 0x1ffc)).toBe(LOW_RAM_END + 0x1ffc);

      // Past the end of high RAM.
      expect(guestPaddrToRamOffset(layout, HIGH_RAM_START + 0x2000)).toBeNull();
    });

    it("guestRangeInBounds: rejects hole/out-of-range and cross-region ranges when guest_size > LOW_RAM_END", () => {
      const ram = LOW_RAM_END + 0x2000;
      const layout = layoutForTesting(ram);

      // Low RAM: fully in bounds.
      expect(guestRangeInBounds(layout, LOW_RAM_END - 4, 4)).toBe(true);
      // Range ending exactly at the low-RAM boundary is OK.
      expect(guestRangeInBounds(layout, LOW_RAM_END - 2, 2)).toBe(true);

      // Hole rejected.
      expect(guestRangeInBounds(layout, LOW_RAM_END, 4)).toBe(false);
      expect(guestRangeInBounds(layout, HIGH_RAM_START - 4, 4)).toBe(false);

      // Empty slices at boundaries are OK.
      expect(guestRangeInBounds(layout, LOW_RAM_END, 0)).toBe(true);
      expect(guestRangeInBounds(layout, HIGH_RAM_START, 0)).toBe(true);
      // But empty slice inside the hole is still rejected.
      expect(guestRangeInBounds(layout, HIGH_RAM_START - 1, 0)).toBe(false);

      // High RAM: remapped region in bounds.
      expect(guestRangeInBounds(layout, HIGH_RAM_START, 4)).toBe(true);
      expect(guestRangeInBounds(layout, HIGH_RAM_START + 0x1ffc, 4)).toBe(true);

      // Range ending exactly at the end of high RAM is OK.
      expect(guestRangeInBounds(layout, HIGH_RAM_START + 0x1ffe, 2)).toBe(true);
      expect(guestRangeInBounds(layout, HIGH_RAM_START + 0x2000, 0)).toBe(true);

      // Cross-region rejected (low -> hole).
      expect(guestRangeInBounds(layout, LOW_RAM_END - 2, 4)).toBe(false);
      // Hole -> high.
      expect(guestRangeInBounds(layout, HIGH_RAM_START - 2, 4)).toBe(false);

      // Out of range beyond end of high RAM.
      expect(guestRangeInBounds(layout, HIGH_RAM_START + 0x2000, 1)).toBe(false);
    });

    it("guestToLinear throws for hole/out-of-range addresses", () => {
      const base = 0x1000;
      const small = layoutForTesting(0x2000, base);
      expect(guestToLinear(small, 0x1234)).toBe(base + 0x1234);
      expect(() => guestToLinear(small, 0x2000)).toThrow(RangeError);

      const big = layoutForTesting(LOW_RAM_END + 0x2000, base);
      // Valid low + high addresses.
      expect(guestToLinear(big, LOW_RAM_END - 1)).toBe(base + (LOW_RAM_END - 1));
      expect(guestToLinear(big, HIGH_RAM_START)).toBe(base + LOW_RAM_END);

      // Hole + out-of-range throws.
      expect(() => guestToLinear(big, LOW_RAM_END)).toThrow(RangeError);
      expect(() => guestToLinear(big, HIGH_RAM_START - 1)).toThrow(RangeError);
      expect(() => guestToLinear(big, HIGH_RAM_START + 0x2000)).toThrow(RangeError);
    });
  });
});
