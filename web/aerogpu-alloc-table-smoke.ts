import { AerogpuCmdWriter, AEROGPU_RESOURCE_USAGE_RENDER_TARGET } from "../emulator/protocol/aerogpu/aerogpu_cmd";
import { AEROGPU_ABI_VERSION_U32, AerogpuFormat } from "../emulator/protocol/aerogpu/aerogpu_pci";
import { AEROGPU_ALLOC_TABLE_MAGIC } from "../emulator/protocol/aerogpu/aerogpu_ring";
import {
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
} from "./src/ipc/gpu-protocol";
import type { WorkerInitMessage } from "./src/runtime/protocol";
import { createSharedMemoryViews } from "./src/runtime/shared_layout";
import { allocateHarnessSharedMemorySegments } from "./src/runtime/harness_shared_memory";
import { formatOneLineError } from "./src/text";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      error?: string;
      pass?: boolean;
      width?: number;
      height?: number;
      samples?: Record<string, number[]>;
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function renderError(message: string): void {
  const status = $("status");
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message, pass: false };
}

function buildAllocTable(entries: Array<{ allocId: number; gpa: bigint; sizeBytes: bigint }>): ArrayBuffer {
  const headerBytes = 24;
  const entryStrideBytes = 32;
  const sizeBytes = headerBytes + entries.length * entryStrideBytes;
  const buf = new ArrayBuffer(sizeBytes);
  const dv = new DataView(buf);

  dv.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
  dv.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
  dv.setUint32(8, sizeBytes, true);
  dv.setUint32(12, entries.length, true);
  dv.setUint32(16, entryStrideBytes, true);
  dv.setUint32(20, 0, true);

  for (let i = 0; i < entries.length; i += 1) {
    const e = entries[i];
    const base = headerBytes + i * entryStrideBytes;
    dv.setUint32(base + 0, e.allocId >>> 0, true);
    dv.setUint32(base + 4, 0, true);
    dv.setBigUint64(base + 8, e.gpa, true);
    dv.setBigUint64(base + 16, e.sizeBytes, true);
    dv.setBigUint64(base + 24, 0n, true);
  }

  return buf;
}

async function main(): Promise<void> {
  const status = $("status");

  try {
    // Allocate small shared guest RAM + control SABs.
    // This harness does not execute the WASM runtime; avoid the full runtime allocator, which reserves
    // a fixed 128MiB wasm32 runtime region and allocates the large default IO IPC + VRAM buffers.
    const sharedFramebuffer = new SharedArrayBuffer(8);
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const worker = new Worker(new URL("./src/workers/gpu.worker.ts", import.meta.url), { type: "module" });

    const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

    let readyResolve!: () => void;
    let readyReject!: (err: unknown) => void;
    const ready = new Promise<void>((resolve, reject) => {
      readyResolve = resolve;
      readyReject = reject;
    });

    let fatalError: string | null = null;
    let nextRequestId = 1;
    const pendingSubmit = new Map<number, { resolve: () => void; reject: (err: unknown) => void }>();
    const pendingScreenshot = new Map<number, { resolve: (msg: any) => void; reject: (err: unknown) => void }>();

    worker.addEventListener("message", (event) => {
      const msg = event.data as unknown;
      if (!msg || typeof msg !== "object") return;
      const record = msg as Record<string, unknown>;
      const type = record.type;
      if (typeof type !== "string") return;

      switch (type) {
        case "ready":
          readyResolve();
          break;
        case "submit_complete": {
          const requestId = typeof record.requestId === "number" ? record.requestId : Number.NaN;
          const pending = pendingSubmit.get(requestId);
          if (!pending) break;
          pendingSubmit.delete(requestId);
          pending.resolve();
          break;
        }
        case "screenshot": {
          const requestId = typeof record.requestId === "number" ? record.requestId : Number.NaN;
          const pending = pendingScreenshot.get(requestId);
          if (!pending) break;
          pendingScreenshot.delete(requestId);
          pending.resolve(msg);
          break;
        }
        case "error":
          readyResolve();
          fatalError = String(record.message ?? "unknown worker error");
          break;
      }
    });

    worker.addEventListener("error", (event) => {
      readyReject((event as ErrorEvent).error ?? event);
    });

    // Worker-side shared memory init (provides shared guest RAM / GPA space).
    const initMsg: WorkerInitMessage = {
      kind: "init",
      role: "gpu",
      controlSab: segments.control,
      guestMemory: segments.guestMemory,
      vram: segments.vram,
      scanoutState: segments.scanoutState,
      scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      cursorState: segments.cursorState,
      cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      ioIpcSab: segments.ioIpc,
      sharedFramebuffer: segments.sharedFramebuffer,
      sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      vgaFramebuffer: segments.sharedFramebuffer,
    };
    worker.postMessage(initMsg);

    // Runtime init for headless mode (no canvas).
    const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
    const frameState = new Int32Array(sharedFrameState);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
    Atomics.store(frameState, FRAME_SEQ_INDEX, 0);

    // Provide a tiny placeholder framebuffer; screenshot path uses the last-presented AeroGPU texture.
    const dummyFramebuffer = sharedFramebuffer;

    worker.postMessage({
      ...GPU_MESSAGE_BASE,
      type: "init",
      sharedFrameState,
      sharedFramebuffer: dummyFramebuffer,
      sharedFramebufferOffsetBytes: 0,
    });

    await ready;
    if (fatalError) {
      throw new Error(fatalError);
    }

    const texWidth = 3;
    const texHeight = 2;
    const rowBytes = texWidth * 4;
    const rowPitchBytes = 16; // padded (tests row_pitch_bytes handling)
    const backingBytes = rowPitchBytes * texHeight;

    const allocId = 1;
    const gpaBase = 0x1000;
    const allocSizeBytes = 4096;

    if (gpaBase + allocSizeBytes > views.guestLayout.guest_size) {
      throw new Error("guest RAM too small for test allocation");
    }

    // Fill the backing memory in guest RAM.
    const backing = views.guestU8.subarray(gpaBase, gpaBase + backingBytes);
    backing.fill(0);

    // Row 0: RGB pixels + padding (0x09).
    backing.set([255, 0, 0, 255], 0); // red
    backing.set([0, 255, 0, 255], 4); // green
    backing.set([0, 0, 255, 255], 8); // blue
    backing.set([9, 9, 9, 9], rowBytes); // padding bytes that must NOT leak into row 1

    // Row 1 is left at 0; we will not dirty it.

    const allocTable = buildAllocTable([{ allocId, gpa: BigInt(gpaBase), sizeBytes: BigInt(allocSizeBytes) }]);

    // Build an ACMD stream that creates a texture backed by alloc_table + backing_alloc_id,
    // then uploads only the first row via RESOURCE_DIRTY_RANGE, and finally PRESENTs it.
    const writer = new AerogpuCmdWriter();
    // Prefer the ABI 1.2+ sRGB variant when available; the CPU executor treats it the same as
    // UNORM (no colorspace conversion), so this remains a pure row_pitch_bytes/backing test.
    const srgbFormat = (AerogpuFormat as unknown as Record<string, unknown>).R8G8B8A8UnormSrgb;
    const format = typeof srgbFormat === "number" ? srgbFormat : AerogpuFormat.R8G8B8A8Unorm;
    writer.createTexture2d(
      /* textureHandle */ 1,
      /* usageFlags */ AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
      /* format */ format,
      texWidth,
      texHeight,
      /* mipLevels */ 1,
      /* arrayLayers */ 1,
      rowPitchBytes,
      /* backingAllocId */ allocId,
      /* backingOffsetBytes */ 0,
    );
    writer.resourceDirtyRange(/* resourceHandle */ 1, /* offsetBytes */ 0n, /* sizeBytes */ BigInt(rowPitchBytes));
    writer.setRenderTargets([1], 0);
    writer.present(/* scanoutId */ 0, /* flags */ 0);
    const cmdBytes = writer.finish();

    const submitReqId = nextRequestId++;
    const submit = new Promise<void>((resolve, reject) => {
      pendingSubmit.set(submitReqId, { resolve, reject });
    });
    worker.postMessage(
      {
        ...GPU_MESSAGE_BASE,
        type: "submit_aerogpu",
        requestId: submitReqId,
        contextId: 0,
        signalFence: 1n,
        cmdStream: cmdBytes.buffer,
        allocTable,
      },
      [cmdBytes.buffer, allocTable],
    );
    await submit;

    const screenshotReqId = nextRequestId++;
    const screenshot = new Promise<any>((resolve, reject) => {
      pendingScreenshot.set(screenshotReqId, { resolve, reject });
    });
    worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId: screenshotReqId });
    const shot = await screenshot;

    const w = Number(shot.width) | 0;
    const h = Number(shot.height) | 0;
    const rgba8 = new Uint8Array(shot.rgba8);
    const sample = (x: number, y: number): number[] => {
      const i = (y * w + x) * 4;
      return [rgba8[i + 0], rgba8[i + 1], rgba8[i + 2], rgba8[i + 3]];
    };

    const samples: Record<string, number[]> = {
      p00: sample(0, 0),
      p10: sample(1, 0),
      p20: sample(2, 0),
      p01: sample(0, 1),
    };

    const expected: Record<string, number[]> = {
      p00: [255, 0, 0, 255],
      p10: [0, 255, 0, 255],
      p20: [0, 0, 255, 255],
      p01: [0, 0, 0, 0],
    };
    const pass =
      w === texWidth &&
      h === texHeight &&
      Object.entries(expected).every(([key, value]) => JSON.stringify(samples[key]) === JSON.stringify(value));

    if (status) {
      status.textContent =
        `backend=headless\n` +
        `size=${w}x${h} rowPitch=${rowPitchBytes}\n` +
        `samples=${JSON.stringify(samples)}\n` +
        `expected=${JSON.stringify(expected)}\n` +
        `pass=${pass}\n`;
    }

    window.__aeroTest = { ready: true, pass, width: w, height: h, samples };
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

void main();
