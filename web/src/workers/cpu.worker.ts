/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { RingBuffer } from "../ipc/ring_buffer";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { perf } from "../perf/perf";
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { FRAME_DIRTY, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX } from "../ipc/gpu-protocol";
import {
  layoutFromHeader,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
  type SharedFramebufferLayout,
} from "../ipc/shared-layout";
import {
  FRAMEBUFFER_FORMAT_RGBA8888,
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  addHeaderI32,
  initFramebufferHeader,
  storeHeaderI32,
  wrapSharedFramebuffer,
} from "../display/framebuffer_protocol";
import {
  CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
  CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
  CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
  CPU_WORKER_DEMO_GUEST_COUNTER_INDEX,
  CPU_WORKER_DEMO_GUEST_COUNTER_OFFSET_BYTES,
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  StatusIndex,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
  type WorkerRole,
} from "../runtime/shared_layout";
import {
  CAPACITY_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
  micRingBufferReadInto,
} from "../audio/mic_ring.js";
import type { MicRingBuffer } from "../audio/mic_ring.js";
import { AudioFrameClock, performanceNowNs } from "../audio/audio_frame_clock";
import {
  HEADER_U32_LEN as AUDIO_HEADER_U32_LEN,
  framesAvailableClamped as audioFramesAvailableClamped,
  framesFree as audioFramesFree,
  getRingBufferLevelFrames as getAudioRingBufferLevelFrames,
  wrapRingBuffer as wrapAudioRingBuffer,
} from "../audio/audio_worklet_ring";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type SetMicrophoneRingBufferMessage,
  type SetAudioRingBufferMessage,
  type CursorSetImageMessage,
  type CursorSetStateMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import {
  serializeVmSnapshotError,
  type CoordinatorToWorkerSnapshotMessage,
  type VmSnapshotCpuStateMessage,
  type VmSnapshotCpuStateSetMessage,
  type VmSnapshotPausedMessage,
  type VmSnapshotResumedMessage,
} from "../runtime/snapshot_protocol";
import { initWasmForContext, type WasmApi } from "../runtime/wasm_context";
import { assertWasmMemoryWiring } from "../runtime/wasm_memory_probe";
import { AeroIpcIoClient } from "../io/ipc/aero_ipc_io";
import {
  IRQ_REFCOUNT_SATURATED,
  IRQ_REFCOUNT_UNDERFLOW,
  applyIrqRefCountChange,
} from "../io/irq_refcount";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

type AudioOutputHdaDemoStartMessage = {
  type: "audioOutputHdaDemo.start";
  ringBuffer: SharedArrayBuffer;
  capacityFrames: number;
  channelCount: number;
  sampleRate: number;
  freqHz?: number;
  gain?: number;
};

type AudioOutputHdaDemoReadyMessage = {
  type: "audioOutputHdaDemo.ready";
};

type AudioOutputHdaDemoErrorMessage = {
  type: "audioOutputHdaDemo.error";
  message: string;
};

type AudioOutputHdaDemoStopMessage = {
  type: "audioOutputHdaDemo.stop";
};

type AudioHdaCaptureSyntheticStartMessage = {
  type: "audioHdaCaptureSynthetic.start";
  requestId: number;
};

type AudioHdaCaptureSyntheticReadyMessage = {
  type: "audioHdaCaptureSynthetic.ready";
  requestId: number;
  pciDevice: number;
  bar0: number;
  mmioBaseLo: number;
  corbBase: number;
  rirbBase: number;
  bdlBase: number;
  pcmBase: number;
  pcmBytes: number;
};

type AudioHdaCaptureSyntheticErrorMessage = {
  type: "audioHdaCaptureSynthetic.error";
  requestId: number;
  message: string;
};

type AudioOutputHdaDemoStatsMessage = {
  type: "audioOutputHdaDemo.stats";
  bufferLevelFrames: number;
  targetFrames: number;
  totalFramesProduced?: number;
  totalFramesWritten?: number;
  totalFramesDropped?: number;
  lastTickRequestedFrames?: number;
  lastTickProducedFrames?: number;
  lastTickWrittenFrames?: number;
  lastTickDroppedFrames?: number;
};

type CursorDemoStartMessage = {
  type: "cursorDemo.start";
};

type CursorDemoStopMessage = {
  type: "cursorDemo.stop";
};

type AudioOutputHdaPciDeviceStartMessage = {
  type: "audioOutputHdaPciDevice.start";
  /**
   * Optional test-tone frequency used to populate the guest DMA buffer.
   * Defaults to 440Hz.
   */
  freqHz?: number;
  /**
   * Optional sine gain used to populate the guest DMA buffer.
   * Defaults to 0.1.
   */
  gain?: number;
};

type AudioOutputHdaPciDeviceStopMessage = {
  type: "audioOutputHdaPciDevice.stop";
};

type AudioOutputHdaPciDeviceReadyMessage = {
  type: "audioOutputHdaPciDevice.ready";
  pci: { bus: number; device: number; function: number };
  bar0: number;
};

type AudioOutputHdaPciDeviceErrorMessage = {
  type: "audioOutputHdaPciDevice.error";
  message: string;
};

let role: WorkerRole = "cpu";
let status!: Int32Array;
let commandRing!: RingBuffer;
let eventRing!: RingBuffer;
let guestI32!: Int32Array;
let guestU8!: Uint8Array;
let vgaFramebuffer: ReturnType<typeof wrapSharedFramebuffer> | null = null;
let frameState: Int32Array | null = null;
let io: AeroIpcIoClient | null = null;
let ioNetTxRing: RingBuffer | null = null;
let ioNetRxRing: RingBuffer | null = null;
let didIoDemo = false;

let irqBitmapLo = 0;
let irqBitmapHi = 0;
// Per-IRQ reference counts so multiple devices can share an interrupt input line
// (wire-OR semantics).
//
// The I/O worker transports IRQ activity as discrete `irqRaise`/`irqLower` events.
// In the canonical browser runtime path (`web/src/workers/io.worker.ts`), those
// events correspond to *aggregated line level transitions* after wire-OR.
//
// We still keep a refcount here as a robustness guard so alternate I/O paths
// (tests, demos) can safely emit per-device assert/deassert events while still
// producing a correct wire-OR bitmap.
//
// Note: A level bitmap alone cannot represent edge-triggered interrupts.
// Edge-triggered devices (e.g. i8042) are represented as explicit pulses
// (0→1→0 transitions); the eventual PIC/APIC model should latch rising edges so
// they are not missed even if the line is lowered quickly.
const irqRefCounts = new Uint16Array(256);
const irqWarnedUnderflow = new Uint8Array(256);
const irqWarnedSaturated = new Uint8Array(256);

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfLastFrameId = 0;
let perfCpuMs = 0;
let perfInstructions = 0n;
let sharedHeader: Int32Array | null = null;
let sharedLayout: SharedFramebufferLayout | null = null;
let sharedSlot0: Uint32Array | null = null;
let sharedSlot1: Uint32Array | null = null;
let sharedDirty0: Uint32Array | null = null;
let sharedDirty1: Uint32Array | null = null;
let sharedTileToggle = false;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

type MicRingBufferView = MicRingBuffer & { sampleRate: number };
let hdaDemoTimer: number | null = null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let hdaDemoInstance: any | null = null;
let hdaDemoHeader: Uint32Array | null = null;
let hdaDemoCapacityFrames = 0;
let hdaDemoSampleRate = 0;
let hdaDemoNextStatsMs = 0;
let hdaDemoWasmMemory: WebAssembly.Memory | null = null;
let hdaDemoClock: AudioFrameClock | null = null;
let hdaDemoClockStarted = false;

let pendingHdaPciDeviceStart: AudioOutputHdaPciDeviceStartMessage | null = null;
let hdaPciDeviceBar0Base: bigint | null = null;

function hdaDemoTargetFrames(capacityFrames: number, sampleRate: number): number {
  // Default to ~200ms buffered, but scale up for larger ring buffers so the demo has
  // more slack when the CPU worker is temporarily stalled (e.g. during GC) or when
  // WASM startup runs long in CI/headless environments.
  return Math.min(capacityFrames, Math.max(Math.floor(sampleRate / 5), Math.floor(capacityFrames / 4)));
}

function readDemoNumber(demo: unknown, key: string): number | undefined {
  if (!demo || typeof demo !== "object") return undefined;
  const record = demo as Record<string, unknown>;
  const value = record[key];
  if (typeof value === "number") return value;
  if (typeof value === "function") {
    try {
      // wasm-bindgen getters may appear as methods in some builds.
      const out = (value as (...args: unknown[]) => unknown).call(demo);
      return typeof out === "number" ? out : undefined;
    } catch {
      return undefined;
    }
  }
  return undefined;
}

function maybePostHdaDemoStats(): void {
  if (!hdaDemoInstance || !hdaDemoHeader) return;
  const now = typeof performance?.now === "function" ? performance.now() : Date.now();
  if (now < hdaDemoNextStatsMs) return;
  hdaDemoNextStatsMs = now + 250;

  const capacity = hdaDemoCapacityFrames;
  const sampleRate = hdaDemoSampleRate;
  const targetFrames = hdaDemoTargetFrames(capacity, sampleRate);
  const msg: AudioOutputHdaDemoStatsMessage = {
    type: "audioOutputHdaDemo.stats",
    bufferLevelFrames: getAudioRingBufferLevelFrames(hdaDemoHeader, capacity),
    targetFrames,
  };

  const totalFramesProduced = readDemoNumber(hdaDemoInstance, "total_frames_produced");
  const totalFramesWritten = readDemoNumber(hdaDemoInstance, "total_frames_written");
  const totalFramesDropped = readDemoNumber(hdaDemoInstance, "total_frames_dropped");
  const lastTickRequestedFrames = readDemoNumber(hdaDemoInstance, "last_tick_requested_frames");
  const lastTickProducedFrames = readDemoNumber(hdaDemoInstance, "last_tick_produced_frames");
  const lastTickWrittenFrames = readDemoNumber(hdaDemoInstance, "last_tick_written_frames");
  const lastTickDroppedFrames = readDemoNumber(hdaDemoInstance, "last_tick_dropped_frames");

  if (typeof totalFramesProduced === "number") msg.totalFramesProduced = totalFramesProduced;
  if (typeof totalFramesWritten === "number") msg.totalFramesWritten = totalFramesWritten;
  if (typeof totalFramesDropped === "number") msg.totalFramesDropped = totalFramesDropped;
  if (typeof lastTickRequestedFrames === "number") msg.lastTickRequestedFrames = lastTickRequestedFrames;
  if (typeof lastTickProducedFrames === "number") msg.lastTickProducedFrames = lastTickProducedFrames;
  if (typeof lastTickWrittenFrames === "number") msg.lastTickWrittenFrames = lastTickWrittenFrames;
  if (typeof lastTickDroppedFrames === "number") msg.lastTickDroppedFrames = lastTickDroppedFrames;

  ctx.postMessage(msg);
}

function stopHdaDemo(): void {
  if (hdaDemoTimer !== null) {
    ctx.clearInterval(hdaDemoTimer);
    hdaDemoTimer = null;
  }
  if (hdaDemoInstance && typeof hdaDemoInstance.free === "function") {
    hdaDemoInstance.free();
  }
  hdaDemoInstance = null;
  hdaDemoHeader = null;
  hdaDemoCapacityFrames = 0;
  hdaDemoSampleRate = 0;
  hdaDemoNextStatsMs = 0;
  hdaDemoClock = null;
  hdaDemoClockStarted = false;
}

async function startHdaDemo(msg: AudioOutputHdaDemoStartMessage): Promise<void> {
  stopHdaDemo();

  const Sab = globalThis.SharedArrayBuffer;
  if (typeof Sab === "undefined" || !(msg.ringBuffer instanceof Sab)) {
    throw new Error("audioOutputHdaDemo.start requires a SharedArrayBuffer ring buffer.");
  }

  const capacityFrames = msg.capacityFrames | 0;
  const channelCount = msg.channelCount | 0;
  const sampleRate = msg.sampleRate | 0;
  if (capacityFrames <= 0) throw new Error("capacityFrames must be > 0");
  if (channelCount !== 2) throw new Error("channelCount must be 2 for HDA demo output");
  if (sampleRate <= 0) throw new Error("sampleRate must be > 0");

  // Prefer the single-threaded WASM build for this standalone demo mode.
  // Playwright CI prebuilds `pkg-single` but not always `pkg-threaded`; forcing
  // "single" avoids an extra failed fetch/compile attempt before falling back.
  //
  // If the single build is unavailable, fall back to the default auto selection.
  let api: WasmApi;
  try {
    // The WASM module uses a custom allocator that reserves a fixed low-address
    // region of linear memory for the runtime (currently 128MiB) so guest RAM can
    // live above it. When we
    // instantiate the module without a coordinator-provided `WebAssembly.Memory`,
    // the wasm-bindgen glue defaults to a ~1MiB memory, leaving essentially no
    // heap and causing `HdaPlaybackDemo::new()` to abort on allocation.
    //
    // Allocate a minimal non-shared memory (currently 128MiB) so the demo can allocate its
    // guest backing store and stream audio without requiring the full VM worker
    // harness.
    if (!hdaDemoWasmMemory) {
      const pages = 128 * 1024 * 1024 / (64 * 1024); // 128MiB / 64KiB
      hdaDemoWasmMemory = new WebAssembly.Memory({ initial: pages, maximum: pages });
    }
    ({ api } = await initWasmForContext({ variant: "single", memory: hdaDemoWasmMemory }));
    // Sanity-check that the memory we allocated is actually wired up as the module's linear memory.
    // (Older/out-of-date wasm-pack outputs can ignore imported memory, which would make the demo's
    // heap sizing assumptions incorrect.)
    assertWasmMemoryWiring({ api, memory: hdaDemoWasmMemory, context: "cpu.worker:hdaDemo" });
  } catch (err) {
    console.warn("Failed to init single-threaded WASM for HDA demo; falling back to auto:", err);
    ({ api } = await initWasmForContext());
  }
  if (typeof api.HdaPlaybackDemo !== "function") {
    // Graceful degrade: nothing to do if the WASM build doesn't include the demo wrapper.
    const message = "HdaPlaybackDemo wasm export is unavailable; skipping HDA audio demo.";
    console.warn(message);
    ctx.postMessage({ type: "audioOutputHdaDemo.error", message } satisfies AudioOutputHdaDemoErrorMessage);
    return;
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const DemoCtor = api.HdaPlaybackDemo as any;
  // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
  const demo = new DemoCtor(msg.ringBuffer, capacityFrames, channelCount, sampleRate);

  const freqHz = typeof msg.freqHz === "number" ? msg.freqHz : 440;
  const gain = typeof msg.gain === "number" ? msg.gain : 0.1;
  if (typeof demo.init_sine_dma === "function") {
    // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
    demo.init_sine_dma(freqHz, gain);
  }

  hdaDemoInstance = demo;
  const header = new Uint32Array(msg.ringBuffer, 0, AUDIO_HEADER_U32_LEN);
  hdaDemoHeader = header;
  hdaDemoCapacityFrames = capacityFrames;
  hdaDemoSampleRate = sampleRate;
  hdaDemoClock = new AudioFrameClock(sampleRate, performanceNowNs());
  hdaDemoClockStarted = false;

  const targetFrames = hdaDemoTargetFrames(capacityFrames, sampleRate);
  // Prime up to the target fill level (without overrunning if the buffer is already full).
  const level = getAudioRingBufferLevelFrames(header, capacityFrames);
  const prime = Math.max(0, targetFrames - level);
  if (prime > 0 && typeof demo.tick === "function") {
    // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
    demo.tick(prime);
  }
  maybePostHdaDemoStats();

  const timer = ctx.setInterval(() => {
    if (!hdaDemoInstance || !hdaDemoHeader) return;
    const clock = hdaDemoClock;
    const header = hdaDemoHeader;
    const capacity = hdaDemoCapacityFrames;
    const sampleRateHz = hdaDemoSampleRate;
    if (!clock || !header || capacity <= 0 || sampleRateHz <= 0) return;

    const nowNs = performanceNowNs();
    const level = getAudioRingBufferLevelFrames(header, capacity);
    const target = hdaDemoTargetFrames(capacity, sampleRateHz);

    // The main thread pre-fills the ring buffer with silence up to capacity so the
    // AudioWorklet doesn't count startup underruns while the worker and WASM spin
    // up. Avoid producing audio frames until enough silence has drained so we
    // won't overflow the buffer (keeping overrunCount at 0 for CI smoke tests).
    if (!hdaDemoClockStarted) {
      // Keep the clock aligned to real time even while we're holding the device
      // "paused" behind the silence prefill.
      hdaDemoClock = new AudioFrameClock(sampleRateHz, nowNs);

      if (level > target) {
        maybePostHdaDemoStats();
        return;
      }

      const prime = Math.max(0, target - level);
      if (prime > 0) {
        // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
        hdaDemoInstance.tick(prime);
      }

      hdaDemoClockStarted = true;
      hdaDemoClock = new AudioFrameClock(sampleRateHz, nowNs);
      maybePostHdaDemoStats();
      return;
    }

    const elapsedFrames = clock.advanceTo(nowNs);
    if (elapsedFrames > 0) {
      const free = Math.max(0, capacity - level);
      const frames = Math.min(elapsedFrames, free);
      if (frames > 0) {
        // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
        hdaDemoInstance.tick(frames);
      }
    }
    maybePostHdaDemoStats();
  }, 20);
  (timer as unknown as { unref?: () => void }).unref?.();
  hdaDemoTimer = timer as unknown as number;

  ctx.postMessage({ type: "audioOutputHdaDemo.ready" } satisfies AudioOutputHdaDemoReadyMessage);
}

function guestBoundsCheck(offset: number, len: number): void {
  const mem = guestU8 as unknown as Uint8Array | undefined;
  if (!mem) throw new Error("guest memory is not initialized yet");
  if (offset < 0 || offset + len > mem.byteLength) {
    throw new Error(`guest memory out of bounds: need [0x${offset.toString(16)}, +${len}]`);
  }
}

// MMIO register offsets used by the HDA PCI playback harness.
const HDA_REG_INTCTL = 0x20n;
const HDA_REG_CORBCTL = 0x4cn;
const HDA_REG_RIRBCTL = 0x5cn;
const HDA_REG_SD0_CTL = 0x80n;

function stopHdaPciDevice(): void {
  // Best-effort: stop any in-flight start request first.
  pendingHdaPciDeviceStart = null;

  const client = io;
  const bar0Base = hdaPciDeviceBar0Base;
  hdaPciDeviceBar0Base = null;

  if (!client || bar0Base === null) return;

  try {
    // Stop the stream DMA engine.
    client.mmioWrite(bar0Base + HDA_REG_SD0_CTL, 4, 0);
  } catch {
    // ignore
  }
  try {
    // Stop CORB/RIRB DMA engines.
    client.mmioWrite(bar0Base + HDA_REG_CORBCTL, 1, 0);
    client.mmioWrite(bar0Base + HDA_REG_RIRBCTL, 1, 0);
  } catch {
    // ignore
  }
  try {
    // Disable interrupts (best-effort).
    client.mmioWrite(bar0Base + HDA_REG_INTCTL, 4, 0);
  } catch {
    // ignore
  }
}

async function startHdaPciDevice(msg: AudioOutputHdaPciDeviceStartMessage): Promise<void> {
  const client = io;
  if (!client) {
    throw new Error("I/O client is not initialized yet");
  }

  // Ensure any previous stream started by this harness is stopped before reprogramming.
  stopHdaPciDevice();

  // Stream descriptor control bits (subset).
  const SD_CTL_SRST = 1 << 0;
  const SD_CTL_RUN = 1 << 1;
  const SD_CTL_IOCE = 1 << 2;
  const SD_CTL_STRM_SHIFT = 20;

  // Wait for the IO worker to report ready (PCI config + MMIO routes depend on it).
  const ioReadyIndex = StatusIndex.IoReady;
  const ioReadyDeadline = (typeof performance?.now === "function" ? performance.now() : Date.now()) + 30_000;
  while (Atomics.load(status, ioReadyIndex) !== 1) {
    const now = typeof performance?.now === "function" ? performance.now() : Date.now();
    if (now >= ioReadyDeadline) {
      throw new Error("Timed out waiting for IO worker ready while starting HDA PCI device.");
    }
    await sleepMs(50);
  }

  const pciEnable = 0x8000_0000;
  const cfgAddr = (bus: number, dev: number, fn: number, reg: number) =>
    (pciEnable | ((bus & 0xff) << 16) | ((dev & 0x1f) << 11) | ((fn & 0x7) << 8) | (reg & 0xfc)) >>> 0;
  const readDword = (bus: number, dev: number, fn: number, reg: number) => {
    client.portWrite(0x0cf8, 4, cfgAddr(bus, dev, fn, reg));
    return client.portRead(0x0cfc, 4) >>> 0;
  };
  const writeDword = (bus: number, dev: number, fn: number, reg: number, value: number) => {
    client.portWrite(0x0cf8, 4, cfgAddr(bus, dev, fn, reg));
    client.portWrite(0x0cfc, 4, value >>> 0);
  };

  // Scan bus0 for Intel ICH6 HD Audio (8086:2668).
  //
  // This device is registered by the IO worker after WASM init completes; when Chromium
  // doesn't have a cached compilation artifact yet (common in CI), we may need to retry.
  let found: { bus: number; device: number; function: number } | null = null;
  const scanDeadlineMs = (typeof performance?.now === "function" ? performance.now() : Date.now()) + 45_000;
  while ((typeof performance?.now === "function" ? performance.now() : Date.now()) < scanDeadlineMs) {
    for (let dev = 0; dev < 32; dev++) {
      const id = readDword(0, dev, 0, 0x00);
      const vendorId = id & 0xffff;
      const deviceId = (id >>> 16) & 0xffff;
      if (vendorId === 0xffff) continue;
      if (vendorId === 0x8086 && deviceId === 0x2668) {
        found = { bus: 0, device: dev, function: 0 };
        break;
      }
    }
    if (found) break;
    await sleepMs(50);
  }
  if (!found) {
    throw new Error("Timed out waiting for Intel HDA PCI function (8086:2668) to appear on bus0.");
  }

  const { bus, device, function: fn } = found;

  // Enable memory-space decoding + bus mastering in PCI command register.
  const cmdStatus = readDword(bus, device, fn, 0x04);
  const command = cmdStatus & 0xffff;
  const newCommand = (command | 0x2 | 0x4) & 0xffff;
  writeDword(bus, device, fn, 0x04, (cmdStatus & 0xffff_0000) | newCommand);

  const bar0 = readDword(bus, device, fn, 0x10) >>> 0;
  // Avoid JS bitwise ops here: BAR bases commonly live above 2^31 (e.g. 0xE000_0000),
  // and `bar0 & 0xffff_fff0` would sign-extend to a negative number before converting to BigInt.
  const bar0Base = BigInt(bar0) & 0xffff_fff0n;
  if (bar0Base === 0n) {
    throw new Error("HDA BAR0 is zero after enabling MEM decoding.");
  }
  hdaPciDeviceBar0Base = bar0Base;

  // MMIO register offsets (subset).
  const REG_GCTL = 0x08n;
  const REG_STATESTS = 0x0en;
  const REG_INTCTL = 0x20n;

  const REG_CORBLBASE = 0x40n;
  const REG_CORBUBASE = 0x44n;
  const REG_CORBWP = 0x48n;
  const REG_CORBRP = 0x4an;
  const REG_CORBCTL = 0x4cn;
  const REG_CORBSIZE = 0x4en;

  const REG_RIRBLBASE = 0x50n;
  const REG_RIRBUBASE = 0x54n;
  const REG_RIRBWP = 0x58n;
  const REG_RIRBCTL = 0x5cn;
  const REG_RIRBSIZE = 0x5en;

  const REG_SD0_CTL = 0x80n;
  const REG_SD0_CBL = 0x88n;
  const REG_SD0_LVI = 0x8cn;
  const REG_SD0_FMT = 0x92n;
  const REG_SD0_BDPL = 0x98n;
  const REG_SD0_BDPU = 0x9cn;

  // Bring controller out of reset.
  client.mmioWrite(bar0Base + REG_GCTL, 4, 0x1);
  const gctlDeadline = (typeof performance?.now === "function" ? performance.now() : Date.now()) + 1_000;
  while ((client.mmioRead(bar0Base + REG_GCTL, 4) & 0x1) === 0) {
    const now = typeof performance?.now === "function" ? performance.now() : Date.now();
    if (now >= gctlDeadline) {
      throw new Error("Timed out waiting for HDA GCTL.CRST to become 1.");
    }
    await sleepMs(1);
  }

  // STATESTS bit0 should indicate codec0 present once out of reset.
  const statests = client.mmioRead(bar0Base + REG_STATESTS, 2) & 0xffff;
  if ((statests & 0x1) === 0) {
    // Not fatal in the harness, but helps debug if the model is miswired.
    console.warn(`[cpu] HDA STATESTS missing codec0 presence bit: 0x${statests.toString(16)}`);
  }

  // Guest memory layout for CORB/RIRB + BDL + PCM.
  //
  // Avoid low-memory offsets that are used by other CPU-worker demos (e.g. the
  // diskRead demo uses 0x1000 as a scratch buffer) and keep this region distinct
  // from the synthetic HDA capture harness (which also reserves a chunk of guest
  // RAM for its own CORB/RIRB/BDL/PCM buffers).
  const corbBase = 0x0030_0000;
  const rirbBase = 0x0030_1000;
  const bdlBase = 0x0040_0000;
  const pcmBase = 0x0041_0000;

  const CORB_ENTRIES = 256;
  const RIRB_ENTRIES = 256;
  const CORB_BYTES = CORB_ENTRIES * 4;
  const RIRB_BYTES = RIRB_ENTRIES * 8;

  guestBoundsCheck(corbBase, CORB_BYTES);
  guestBoundsCheck(rirbBase, RIRB_BYTES);
  guestBoundsCheck(bdlBase, 16);

  // Configure CORB/RIRB to exercise the command/response path.
  client.mmioWrite(bar0Base + REG_CORBLBASE, 4, corbBase);
  client.mmioWrite(bar0Base + REG_CORBUBASE, 4, 0);
  client.mmioWrite(bar0Base + REG_CORBSIZE, 1, 0x2); // 256 entries
  client.mmioWrite(bar0Base + REG_CORBRP, 2, 0x00ff); // first command lands at entry 0

  client.mmioWrite(bar0Base + REG_RIRBLBASE, 4, rirbBase);
  client.mmioWrite(bar0Base + REG_RIRBUBASE, 4, 0);
  client.mmioWrite(bar0Base + REG_RIRBSIZE, 1, 0x2); // 256 entries
  client.mmioWrite(bar0Base + REG_RIRBWP, 2, 0x00ff); // first response lands at entry 0

  client.mmioWrite(bar0Base + REG_RIRBCTL, 1, 0x02); // RUN
  client.mmioWrite(bar0Base + REG_CORBCTL, 1, 0x02); // RUN

  const guestDv = new DataView(guestU8.buffer, guestU8.byteOffset, guestU8.byteLength);

  // Configure codec output converter (NID 2) to listen on stream 1, channel 0.
  // HDA CORB command format: CAD[31:28] | NID[27:20] | VERB[19:0].
  const mkCorbCmd = (cad: number, nid: number, verb20: number) =>
    (((cad & 0xf) << 28) | ((nid & 0x7f) << 20) | (verb20 & 0x000f_ffff)) >>> 0;

  const setStreamChVerb20 = ((0x706 << 8) | 0x10) >>> 0; // stream=1, channel=0
  const fmtRaw = 0x0011; // 48kHz base, 16-bit, stereo
  const setFmtVerb20 = ((0x200 << 8) | (fmtRaw & 0xffff)) >>> 0;

  guestDv.setUint32(corbBase + 0, mkCorbCmd(0, 2, setStreamChVerb20), true);
  guestDv.setUint32(corbBase + 4, mkCorbCmd(0, 2, setFmtVerb20), true);
  client.mmioWrite(bar0Base + REG_CORBWP, 2, 0x0001);

  // Wait for both verbs to be processed (RIRBWP should advance to 1).
  const rirbDeadline = (typeof performance?.now === "function" ? performance.now() : Date.now()) + 1_000;
  while ((client.mmioRead(bar0Base + REG_RIRBWP, 2) & 0xffff) !== 0x0001) {
    const now = typeof performance?.now === "function" ? performance.now() : Date.now();
    if (now >= rirbDeadline) {
      throw new Error("Timed out waiting for HDA CORB/RIRB verb processing.");
    }
    await sleepMs(1);
  }

  // Populate a looping PCM buffer (sine wave) and a single-entry BDL pointing at it.
  const freqHz = typeof msg.freqHz === "number" ? msg.freqHz : 440;
  const gain = typeof msg.gain === "number" ? msg.gain : 0.1;
  const sampleRate = 48_000;
  const frames = Math.floor(sampleRate / 5); // ~200ms
  const bytesPerFrame = 4; // 16-bit stereo
  const pcmLenBytes = frames * bytesPerFrame;

  guestBoundsCheck(pcmBase, pcmLenBytes);

  for (let n = 0; n < frames; n++) {
    const t = n / sampleRate;
    const s = Math.sin(2 * Math.PI * freqHz * t) * gain;
    let v16 = Math.round(s * 0x7fff);
    if (v16 > 0x7fff) v16 = 0x7fff;
    if (v16 < -0x8000) v16 = -0x8000;

    const off = pcmBase + n * bytesPerFrame;
    guestDv.setInt16(off, v16, true);
    guestDv.setInt16(off + 2, v16, true);
  }

  // BDL entry: [addr:u64, len:u32, flags:u32]. IOC=1 so real implementations can raise BCIS.
  guestDv.setUint32(bdlBase + 0, pcmBase >>> 0, true);
  guestDv.setUint32(bdlBase + 4, 0, true);
  guestDv.setUint32(bdlBase + 8, pcmLenBytes >>> 0, true);
  guestDv.setUint32(bdlBase + 12, 1, true);

  // Program stream descriptor 0.
  client.mmioWrite(bar0Base + REG_SD0_BDPL, 4, bdlBase);
  client.mmioWrite(bar0Base + REG_SD0_BDPU, 4, 0);
  client.mmioWrite(bar0Base + REG_SD0_CBL, 4, pcmLenBytes >>> 0);
  client.mmioWrite(bar0Base + REG_SD0_LVI, 2, 0);
  client.mmioWrite(bar0Base + REG_SD0_FMT, 2, fmtRaw);

  // SRST | RUN | IOCE | stream number 1.
  const sdCtl = (SD_CTL_SRST | SD_CTL_RUN | SD_CTL_IOCE | (1 << SD_CTL_STRM_SHIFT)) >>> 0;
  client.mmioWrite(bar0Base + REG_SD0_CTL, 4, sdCtl);

  // Enable global interrupt + stream0 enable (best-effort).
  client.mmioWrite(bar0Base + REG_INTCTL, 4, 0x8000_0000 | 0x1);

  ctx.postMessage({ type: "audioOutputHdaPciDevice.ready", pci: found, bar0 } satisfies AudioOutputHdaPciDeviceReadyMessage);
}

type WasmMicBridgeHandle = {
  read_f32_into(out: Float32Array): number;
  free?: () => void;
};

let micRingBuffer: MicRingBufferView | null = null;
let micScratch = new Float32Array();
let loopbackScratch = new Float32Array();
let micResampleScratch = new Float32Array();
let micResampler: JsStreamingLinearResamplerMono | null = null;
let wasmMicBridge: WasmMicBridgeHandle | null = null;

let wasmApi: WasmApi | null = null;
type CpuWorkerDemoCtor = NonNullable<WasmApi["CpuWorkerDemo"]>;
type CpuWorkerDemoInstance = InstanceType<CpuWorkerDemoCtor>;
let cpuDemo: CpuWorkerDemoInstance | null = null;

type WasmVmCtor = NonNullable<WasmApi["WasmVm"]>;
type WasmVmInstance = InstanceType<WasmVmCtor>;
let wasmVm: WasmVmInstance | null = null;
let vmBooted = false;
let vmBootSectorLoaded = false;
let vmLastVgaTextBytes: Uint8Array | null = null;
let vmNextBootSectorLoadAttemptMs = 0;

let perfIoWaitMs = 0;
let perfDeviceExits = 0;
let perfDeviceIoReadBytes = 0;
let perfDeviceIoWriteBytes = 0;

// Demo framebuffer region inside guest RAM. The worker drives a tiny JS→WASM→SAB
// render path by asking WASM to fill pixels here and then bulk-copying them into the VGA SAB.
// NOTE: Keep this disjoint from the shared framebuffer demo region starting at
// `CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES`.
const DEMO_FB_OFFSET = 0x500000;
const DEMO_FB_MAX_BYTES = 1024 * 768 * 4;

let audioRingBuffer: SharedArrayBuffer | null = null;
let audioDstSampleRate = 0;
let audioChannelCount = 0;
let audioCapacityFrames = 0;

let workletBridge: unknown | null = null;
type SineToneHandle = {
  write: (bridge: unknown, frames: number, freqHz: number, sampleRate: number, gain: number) => number;
  free?: () => void;
};
let sineTone: SineToneHandle | null = null;

let nextAudioFillDeadlineMs = 0;
// Tracks whether this CPU worker currently "owns" the AudioWorklet output ring
// buffer producer side. The output ring is single-producer/single-consumer; if a
// real VM is running, the I/O worker's guest audio device will become the sole
// producer and the CPU worker must not write fallback samples.
let cpuIsAudioRingProducer = false;

function detachMicBridge(): void {
  if (wasmMicBridge && typeof wasmMicBridge.free === "function") {
    wasmMicBridge.free();
  }
  wasmMicBridge = null;
}

function maybeInitMicBridge(): void {
  if (wasmMicBridge) return;
  const apiAny = wasmApi as unknown as Record<string, unknown> | null;
  const mic = micRingBuffer;
  if (!apiAny || !mic) return;

  try {
    if (typeof apiAny.attach_mic_bridge === "function") {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      wasmMicBridge = (apiAny.attach_mic_bridge as any)(mic.sab) as WasmMicBridgeHandle;
      return;
    }

    const MicBridge = apiAny.MicBridge as { fromSharedBuffer?: unknown } | undefined;
    if (MicBridge && typeof MicBridge.fromSharedBuffer === "function") {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      wasmMicBridge = (MicBridge.fromSharedBuffer as any)(mic.sab) as WasmMicBridgeHandle;
    }
  } catch (err) {
    console.warn("Failed to attach WASM mic bridge:", err);
    detachMicBridge();
  }
}

function attachMicrophoneRingBuffer(msg: SetMicrophoneRingBufferMessage): void {
  const ringBuffer = msg.ringBuffer;
  if (ringBuffer !== null) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") {
      throw new Error("SharedArrayBuffer is unavailable; microphone capture requires crossOriginIsolated.");
    }
    if (!(ringBuffer instanceof Sab)) {
      throw new Error("setMicrophoneRingBuffer expects a SharedArrayBuffer or null.");
    }
  }

  if ((micRingBuffer?.sab ?? null) !== ringBuffer) {
    detachMicBridge();
    micResampler = null;
  }

  micRingBuffer = null;
  if (!ringBuffer) return;

  const header = new Uint32Array(ringBuffer, 0, MIC_HEADER_U32_LEN);
  const capacity = Atomics.load(header, CAPACITY_SAMPLES_INDEX) >>> 0;
  if (capacity === 0) {
    throw new Error("mic ring buffer capacity must be non-zero");
  }

  const requiredBytes = MIC_HEADER_BYTES + capacity * Float32Array.BYTES_PER_ELEMENT;
  if (ringBuffer.byteLength < requiredBytes) {
    throw new Error(`mic ring buffer is too small: need ${requiredBytes} bytes, got ${ringBuffer.byteLength} bytes`);
  }

  const data = new Float32Array(ringBuffer, MIC_HEADER_BYTES, capacity);
  micRingBuffer = { sab: ringBuffer, header, data, capacity, sampleRate: (msg.sampleRate ?? 0) | 0 };

  maybeInitMicBridge();
}

class JsWorkletBridge {
  readonly capacity_frames: number;
  readonly channel_count: number;
  private readonly readIndex: Uint32Array;
  private readonly writeIndex: Uint32Array;
  private readonly underrunCount: Uint32Array;
  private readonly overrunCount: Uint32Array;
  private readonly samples: Float32Array;

  constructor(sab: SharedArrayBuffer, capacityFrames: number, channelCount: number) {
    this.capacity_frames = capacityFrames;
    this.channel_count = channelCount;

    const views = wrapAudioRingBuffer(sab, capacityFrames, channelCount);
    this.readIndex = views.readIndex;
    this.writeIndex = views.writeIndex;
    this.underrunCount = views.underrunCount;
    this.overrunCount = views.overrunCount;
    this.samples = views.samples;
  }

  buffer_level_frames(): number {
    const read = Atomics.load(this.readIndex, 0) >>> 0;
    const write = Atomics.load(this.writeIndex, 0) >>> 0;
    return audioFramesAvailableClamped(read, write, this.capacity_frames);
  }

  underrun_count(): number {
    return Atomics.load(this.underrunCount, 0) >>> 0;
  }

  overrun_count(): number {
    return Atomics.load(this.overrunCount, 0) >>> 0;
  }

  write_f32_interleaved(input: Float32Array): number {
    const requestedFrames = Math.floor(input.length / this.channel_count);
    if (requestedFrames === 0) return 0;

    const read = Atomics.load(this.readIndex, 0) >>> 0;
    const write = Atomics.load(this.writeIndex, 0) >>> 0;

    const free = audioFramesFree(read, write, this.capacity_frames);
    const framesToWrite = Math.min(requestedFrames, free);
    const droppedFrames = requestedFrames - framesToWrite;
    if (droppedFrames > 0) {
      Atomics.add(this.overrunCount, 0, droppedFrames);
    }
    if (framesToWrite === 0) return 0;

    const writePos = write % this.capacity_frames;
    const firstFrames = Math.min(framesToWrite, this.capacity_frames - writePos);
    const secondFrames = framesToWrite - firstFrames;

    const cc = this.channel_count;
    const firstSamples = firstFrames * cc;
    const secondSamples = secondFrames * cc;

    this.samples.set(input.subarray(0, firstSamples), writePos * cc);
    if (secondFrames > 0) {
      this.samples.set(input.subarray(firstSamples, firstSamples + secondSamples), 0);
    }

    Atomics.store(this.writeIndex, 0, write + framesToWrite);
    return framesToWrite;
  }

  free(): void {
    // No-op; included for parity with wasm-bindgen objects.
  }
}

class JsSineTone {
  private phase = 0;
  private scratch = new Float32Array();
  private readonly channelCount: number;

  constructor(channelCount: number) {
    this.channelCount = channelCount;
  }

  write(bridge: unknown, frames: number, freqHz: number, sampleRate: number, gain: number): number {
    if (frames <= 0 || sampleRate <= 0) return 0;

    const cc = this.channelCount;
    const totalSamples = frames * cc;
    if (this.scratch.length < totalSamples) {
      this.scratch = new Float32Array(totalSamples);
    }
    const out = this.scratch.subarray(0, totalSamples);

    for (let i = 0; i < frames; i++) {
      const s = Math.sin(this.phase * 2 * Math.PI) * gain;
      for (let c = 0; c < cc; c++) out[i * cc + c] = s;
      this.phase += freqHz / sampleRate;
      if (this.phase >= 1) this.phase -= 1;
    }

    return (bridge as JsWorkletBridge).write_f32_interleaved(out);
  }

  free(): void {
    // No-op; included for parity with wasm-bindgen objects.
  }
}

class JsStreamingLinearResamplerMono {
  private srcRate = 0;
  private dstRate = 0;
  private stepSrcPerDst = 1;
  private srcPos = 0;

  private buf = new Float32Array(0);
  private start = 0;
  private end = 0;

  configure(srcRate: number, dstRate: number): void {
    const s = Number.isFinite(srcRate) ? Math.floor(srcRate) : 0;
    const d = Number.isFinite(dstRate) ? Math.floor(dstRate) : 0;
    if (s <= 0 || d <= 0) {
      this.reset();
      this.srcRate = 0;
      this.dstRate = 0;
      this.stepSrcPerDst = 1;
      return;
    }
    if (this.srcRate === s && this.dstRate === d) return;
    this.srcRate = s;
    this.dstRate = d;
    this.stepSrcPerDst = s / d;
    this.reset();
  }

  reset(): void {
    this.srcPos = 0;
    this.start = 0;
    this.end = 0;
  }

  queuedSourceFrames(): number {
    return Math.max(0, this.end - this.start);
  }

  requiredSourceFrames(dstFrames: number): number {
    const frames = Math.max(0, dstFrames | 0);
    if (frames === 0) return 0;

    // Need idx and idx+1 for the final output frame.
    const lastPos = this.srcPos + (frames - 1) * this.stepSrcPerDst;
    const idx = Math.floor(lastPos);
    const frac = lastPos - idx;
    if (Math.abs(frac) <= 1e-12) return idx + 1;
    return idx + 2;
  }

  pushSource(samples: Float32Array, count?: number): void {
    const len = Math.max(0, Math.min(samples.length, count ?? samples.length) | 0);
    if (len === 0) return;

    this.ensureCapacity(len);
    this.buf.set(samples.subarray(0, len), this.end);
    this.end += len;
  }

  produceInto(dstFrames: number, out: Float32Array): number {
    const frames = Math.max(0, dstFrames | 0);
    if (frames === 0) return 0;
    if (out.length < frames) return 0;

    let produced = 0;
    for (; produced < frames; produced++) {
      const idx = Math.floor(this.srcPos);
      const frac = this.srcPos - idx;
      const base = this.start + idx;
      if (base >= this.end) break;

      const a = this.buf[base];
      let sample = a;
      if (Math.abs(frac) > 1e-12) {
        if (base + 1 >= this.end) break;
        const b = this.buf[base + 1];
        sample = a + (b - a) * frac;
      }

      out[produced] = sample;

      this.srcPos += this.stepSrcPerDst;
      const drop = Math.floor(this.srcPos);
      if (drop > 0) {
        this.start += drop;
        this.srcPos -= drop;

        // Compact the queue once it grows a bit to avoid unbounded growth.
        if (this.start > 4096 && this.start > (this.buf.length >> 1)) {
          const remaining = this.end - this.start;
          this.buf.copyWithin(0, this.start, this.end);
          this.start = 0;
          this.end = remaining;
        }
      }
    }

    return produced;
  }

  private ensureCapacity(extra: number): void {
    const queued = this.end - this.start;
    if (queued < 0) {
      this.reset();
      return;
    }

    // First attempt to compact in-place if we have headroom at the front.
    if (this.start > 0 && this.buf.length - queued >= extra) {
      this.buf.copyWithin(0, this.start, this.end);
      this.start = 0;
      this.end = queued;
      return;
    }

    const needed = queued + extra;
    if (this.buf.length >= needed && this.start === 0) {
      return;
    }

    const nextCap = Math.max(needed, this.buf.length > 0 ? this.buf.length * 2 : 1024);
    const next = new Float32Array(nextCap);
    if (queued > 0) {
      next.set(this.buf.subarray(this.start, this.end), 0);
    }
    this.buf = next;
    this.start = 0;
    this.end = queued;
  }
}

function detachAudioOutput(): void {
  const wasProducer = cpuIsAudioRingProducer;
  cpuIsAudioRingProducer = false;
  if (sineTone?.free) {
    sineTone.free();
  }
  sineTone = null;

  if (workletBridge && typeof (workletBridge as { free?: unknown }).free === "function") {
    (workletBridge as { free(): void }).free();
  }
  workletBridge = null;
  nextAudioFillDeadlineMs = 0;

  // Only the current producer should touch the shared telemetry slots. Clearing
  // these unconditionally would stomp I/O-worker telemetry once the VM wires up
  // a real guest audio device.
  if (typeof status !== "undefined" && wasProducer) {
    Atomics.store(status, StatusIndex.AudioBufferLevelFrames, 0);
    Atomics.store(status, StatusIndex.AudioUnderrunCount, 0);
    Atomics.store(status, StatusIndex.AudioOverrunCount, 0);
  }
}

function maybeInitAudioOutput(): void {
  detachAudioOutput();

  if (!audioRingBuffer) return;
  if (audioCapacityFrames <= 0 || audioChannelCount <= 0) return;

  // Prefer the WASM-side bridge + sine generator if available; otherwise fall back
  // to a tiny JS implementation so worker-driven audio works even when the WASM
  // packages are absent (e.g. in CI or fresh checkouts).
  if (wasmApi?.attach_worklet_bridge && wasmApi?.SineTone) {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      workletBridge = (wasmApi.attach_worklet_bridge as any)(audioRingBuffer, audioCapacityFrames, audioChannelCount);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      sineTone = new (wasmApi.SineTone as any)() as SineToneHandle;
      nextAudioFillDeadlineMs = performance.now();
      return;
    } catch (err) {
      console.error("Failed to init WASM audio output bridge:", err);
      detachAudioOutput();
    }
  }

  try {
    workletBridge = new JsWorkletBridge(audioRingBuffer, audioCapacityFrames, audioChannelCount);
    sineTone = new JsSineTone(audioChannelCount);
    nextAudioFillDeadlineMs = performance.now();
  } catch (err) {
    console.error("Failed to init JS audio output bridge:", err);
    detachAudioOutput();
  }
}

function attachAudioRingBuffer(msg: SetAudioRingBufferMessage): void {
  const ringBuffer = msg.ringBuffer;
  if (ringBuffer !== null) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") {
      throw new Error("SharedArrayBuffer is unavailable; audio output requires crossOriginIsolated.");
    }
    if (!(ringBuffer instanceof Sab)) {
      throw new Error("setAudioRingBuffer expects a SharedArrayBuffer or null.");
    }
  }

  audioRingBuffer = ringBuffer;
  audioDstSampleRate = msg.dstSampleRate >>> 0;
  audioChannelCount = msg.channelCount >>> 0;
  audioCapacityFrames = msg.capacityFrames >>> 0;

  maybeInitAudioOutput();
}

function pumpMicLoopback(maxWriteFrames: number): number {
  const mic = micRingBuffer;
  const bridge = workletBridge as { write_f32_interleaved?: (samples: Float32Array) => number } | null;
  if (!mic || !bridge || typeof bridge.write_f32_interleaved !== "function") return 0;

  const cc = audioChannelCount;
  if (cc <= 0) return 0;

  const gain = 1.0;
  const maxChunkFrames = 256;
  const srcRate = mic.sampleRate;
  const dstRate = audioDstSampleRate;

  let remaining = Math.max(0, maxWriteFrames | 0);
  let totalWritten = 0;

  if (srcRate > 0 && dstRate > 0 && srcRate !== dstRate) {
    const resampler = micResampler ?? (micResampler = new JsStreamingLinearResamplerMono());
    resampler.configure(srcRate, dstRate);

    while (remaining > 0) {
      const frames = Math.min(remaining, maxChunkFrames);
      if (frames <= 0) break;

      const requiredSrc = resampler.requiredSourceFrames(frames);
      const queuedSrc = resampler.queuedSourceFrames();
      const needSrc = Math.max(0, requiredSrc - queuedSrc);

      if (needSrc > 0) {
        if (micScratch.length < needSrc) micScratch = new Float32Array(needSrc);
        const micSlice = micScratch.subarray(0, needSrc);
        let read = 0;
        if (wasmMicBridge) {
          try {
            read = wasmMicBridge.read_f32_into(micSlice) | 0;
          } catch (err) {
            console.warn("WASM mic bridge read failed; falling back to JS ring reader:", err);
            detachMicBridge();
          }
        }
        if (!wasmMicBridge) {
          read = micRingBufferReadInto(mic, micSlice);
        }

        if (read > 0) {
          resampler.pushSource(micSlice, read);
        } else if (queuedSrc === 0) {
          break;
        }
      }

      if (micResampleScratch.length < frames) micResampleScratch = new Float32Array(frames);
      const monoOut = micResampleScratch.subarray(0, frames);
      const produced = resampler.produceInto(frames, monoOut);
      if (produced === 0) break;

      const outSamples = produced * cc;
      if (loopbackScratch.length < outSamples) loopbackScratch = new Float32Array(outSamples);

      if (cc === 1) {
        for (let i = 0; i < produced; i++) loopbackScratch[i] = monoOut[i] * gain;
      } else {
        for (let i = 0; i < produced; i++) {
          const s = monoOut[i] * gain;
          const base = i * cc;
          for (let c = 0; c < cc; c++) loopbackScratch[base + c] = s;
        }
      }

      const written = bridge.write_f32_interleaved(loopbackScratch.subarray(0, outSamples)) | 0;
      if (written === 0) break;
      totalWritten += written;
      remaining -= written;
    }

    return totalWritten;
  }

  // We aren't resampling this call; reset any prior resampler state so we don't
  // replay stale queued samples if we later re-enter the resampling path with
  // the same rate pair.
  micResampler?.reset();

  while (remaining > 0) {
    const frames = Math.min(remaining, maxChunkFrames);
    if (micScratch.length < frames) micScratch = new Float32Array(frames);

    const micSlice = micScratch.subarray(0, frames);
    let read = 0;
    if (wasmMicBridge) {
      try {
        read = wasmMicBridge.read_f32_into(micSlice) | 0;
      } catch (err) {
        console.warn("WASM mic bridge read failed; falling back to JS ring reader:", err);
        detachMicBridge();
      }
    }
    if (!wasmMicBridge) {
      read = micRingBufferReadInto(mic, micSlice);
    }
    if (read === 0) break;

    const outSamples = read * cc;
    if (loopbackScratch.length < outSamples) loopbackScratch = new Float32Array(outSamples);

    if (cc === 1) {
      for (let i = 0; i < read; i++) loopbackScratch[i] = micScratch[i] * gain;
    } else {
      for (let i = 0; i < read; i++) {
        const s = micScratch[i] * gain;
        const base = i * cc;
        for (let c = 0; c < cc; c++) loopbackScratch[base + c] = s;
      }
    }

    const written = bridge.write_f32_interleaved(loopbackScratch.subarray(0, outSamples)) | 0;
    if (written === 0) break;
    totalWritten += written;
    remaining -= written;
  }

  return totalWritten;
}

let diskDemoStarted = false;
let diskDemoResponses = 0;

let cursorDemoEnabled = false;
let snapshotPaused = false;

function sleepMs(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    (timer as unknown as { unref?: () => void }).unref?.();
  });
}

function guestWriteU32(addr: number, value: number): void {
  const view = new DataView(guestU8.buffer);
  view.setUint32(guestU8.byteOffset + (addr >>> 0), value >>> 0, true);
}

function guestWriteU64(addr: number, value: bigint): void {
  const view = new DataView(guestU8.buffer);
  const off = guestU8.byteOffset + (addr >>> 0);
  view.setUint32(off, Number(value & 0xffff_ffffn) >>> 0, true);
  view.setUint32(off + 4, Number((value >> 32n) & 0xffff_ffffn) >>> 0, true);
}

async function startHdaCaptureSynthetic(msg: AudioHdaCaptureSyntheticStartMessage): Promise<void> {
  const requestId = msg.requestId >>> 0;
  try {
    const ioClient = io;
    if (!ioClient) throw new Error("I/O client is not initialized (CPU worker not ready).");
    if (typeof guestU8 === "undefined") throw new Error("guest memory is not initialized.");

    // PCI config scan for Intel ICH6 HDA (8086:2668).
    const PCI_ENABLE = 0x8000_0000;
    const pciCfgAddr = (device: number, reg: number): number => (PCI_ENABLE | ((device & 0x1f) << 11) | (reg & 0xfc)) >>> 0;
    const pciReadDword = (device: number, reg: number): number => {
      ioClient.portWrite(0x0cf8, 4, pciCfgAddr(device, reg));
      return ioClient.portRead(0x0cfc, 4) >>> 0;
    };
    const pciWriteDword = (device: number, reg: number, value: number): void => {
      ioClient.portWrite(0x0cf8, 4, pciCfgAddr(device, reg));
      ioClient.portWrite(0x0cfc, 4, value >>> 0);
    };

    let pciDevice = -1;
    let bar0 = 0;
    // WASM + device bring-up can be slow in CI/headless environments (especially if the shared
    // guest-memory build has to compile on-demand). Be generous here so we don't fail the
    // synthetic HDA capture harness due to transient startup latency.
    const deadlineMs = performance.now() + 30_000;
    while (performance.now() < deadlineMs) {
      for (let dev = 0; dev < 32; dev++) {
        const id = pciReadDword(dev, 0x00);
        const vendorId = id & 0xffff;
        const deviceId = (id >>> 16) & 0xffff;
        if (vendorId === 0xffff) continue;
        if (vendorId === 0x8086 && deviceId === 0x2668) {
          pciDevice = dev;
          bar0 = pciReadDword(dev, 0x10) >>> 0;
          break;
        }
      }
      if (pciDevice >= 0 && bar0 !== 0) break;
      await sleepMs(50);
    }
    if (pciDevice < 0) {
      throw new Error("HDA PCI function (8086:2668) not found on bus 0.");
    }
    if (bar0 === 0) {
      throw new Error(`HDA PCI BAR0 is not programmed (dev=${pciDevice}).`);
    }

    // Enable memory-space decoding (and bus mastering for realism).
    const cmd = pciReadDword(pciDevice, 0x04);
    pciWriteDword(pciDevice, 0x04, (cmd | 0x0000_0006) >>> 0);

    const mmioBase = BigInt(bar0 >>> 0) & 0xffff_fff0n;

    const hdaWrite = (offset: number, size: number, value: number): void => {
      ioClient.mmioWrite(mmioBase + BigInt(offset >>> 0), size, value >>> 0);
    };
    const hdaRead = (offset: number, size: number): number => {
      return ioClient.mmioRead(mmioBase + BigInt(offset >>> 0), size) >>> 0;
    };

    // Bring controller out of reset (GCTL.CRST).
    hdaWrite(0x08, 4, 0x1);

    // Guest memory layout for this debug harness.
    const corbBase = 0x0010_0000;
    const rirbBase = 0x0010_1000;
    const bdlBase = 0x0020_0000; // 128-byte aligned
    const pcmBase = 0x0021_0000;
    const pcmBytes = 0x4000;

    // Clear the target PCM buffer so the main thread can detect progress reliably.
    guestU8.fill(0, pcmBase, pcmBase + pcmBytes);

    // Setup CORB/RIRB in guest memory (2 entries each).
    hdaWrite(0x4e, 1, 0); // CORBSIZE: 2 entries
    hdaWrite(0x5e, 1, 0); // RIRBSIZE: 2 entries
    hdaWrite(0x40, 4, corbBase);
    hdaWrite(0x44, 4, 0);
    hdaWrite(0x50, 4, rirbBase);
    hdaWrite(0x54, 4, 0);

    // Set pointers so first command/response lands at entry 0.
    hdaWrite(0x4a, 2, 0x00ff); // CORBRP
    hdaWrite(0x58, 2, 0x00ff); // RIRBWP

    // Start rings.
    hdaWrite(0x5c, 1, 0x02); // RIRBCTL.RUN
    hdaWrite(0x4c, 1, 0x02); // CORBCTL.RUN

    // Queue one verb: SET_STREAM_CHANNEL on input converter (NID 4) to stream 2, channel 0.
    // cmd = (cad<<28) | (nid<<20) | verb_20
    const verb20 = ((0x706 << 8) | 0x20) >>> 0;
    const cmdWord = ((4 << 20) | verb20) >>> 0;
    guestWriteU32(corbBase, cmdWord);
    hdaWrite(0x48, 2, 0x0000); // CORBWP = 0

    // Wait (briefly) for the verb to be processed so the capture stream is accepted.
    const rirbDeadline = performance.now() + 1000;
    while (performance.now() < rirbDeadline) {
      const rirbSts = hdaRead(0x5d, 1) & 0xff;
      if ((rirbSts & 0x1) !== 0) break;
      await sleepMs(10);
    }

    // Program capture stream 1 BDL entry.
    guestWriteU64(bdlBase + 0x00, BigInt(pcmBase));
    guestWriteU32(bdlBase + 0x08, pcmBytes);
    guestWriteU32(bdlBase + 0x0c, 0);

    const SD_BASE = 0x80;
    const SD_STRIDE = 0x20;
    const sd1 = SD_BASE + SD_STRIDE * 1;
    const SD_CTL = sd1 + 0x00;
    const SD_CBL = sd1 + 0x08;
    const SD_LVI = sd1 + 0x0c;
    const SD_FMT = sd1 + 0x12;
    const SD_BDPL = sd1 + 0x18;
    const SD_BDPU = sd1 + 0x1c;

    // 48kHz, 16-bit, mono (matches `aero_audio` tests).
    const fmtRaw = 1 << 4;
    hdaWrite(SD_BDPL, 4, bdlBase);
    hdaWrite(SD_BDPU, 4, 0);
    hdaWrite(SD_CBL, 4, pcmBytes);
    hdaWrite(SD_LVI, 2, 0);
    hdaWrite(SD_FMT, 2, fmtRaw);
    // SRST | RUN | stream number 2.
    const SD_CTL_SRST = 1 << 0;
    const SD_CTL_RUN = 1 << 1;
    const ctl = (SD_CTL_SRST | SD_CTL_RUN | (2 << 20)) >>> 0;
    hdaWrite(SD_CTL, 4, ctl);

    ctx.postMessage({
      type: "audioHdaCaptureSynthetic.ready",
      requestId,
      pciDevice,
      bar0,
      mmioBaseLo: Number(mmioBase & 0xffff_ffffn) >>> 0,
      corbBase,
      rirbBase,
      bdlBase,
      pcmBase,
      pcmBytes,
    } satisfies AudioHdaCaptureSyntheticReadyMessage);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    ctx.postMessage({ type: "audioHdaCaptureSynthetic.error", requestId, message } satisfies AudioHdaCaptureSyntheticErrorMessage);
  }
}

function startCursorDemo(): void {
  cursorDemoEnabled = true;

  // 1x1 opaque blue pixel at (0,0). Keeping this tiny and fully opaque makes it
  // suitable for deterministic screenshot assertions in Playwright, and avoids
  // colliding with the shared-framebuffer demo's red/green tiles.
  const cursorBytes = new Uint8Array([0, 0, 255, 255]);
  ctx.postMessage(
    { kind: "cursor.set_image", width: 1, height: 1, rgba8: cursorBytes.buffer } satisfies CursorSetImageMessage,
    [cursorBytes.buffer],
  );
  ctx.postMessage(
    { kind: "cursor.set_state", enabled: true, x: 0, y: 0, hotX: 0, hotY: 0 } satisfies CursorSetStateMessage,
  );
}

function stopCursorDemo(): void {
  cursorDemoEnabled = false;
  ctx.postMessage(
    { kind: "cursor.set_state", enabled: false, x: 0, y: 0, hotX: 0, hotY: 0 } satisfies CursorSetStateMessage,
  );
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const msg = ev.data as
    | Partial<WorkerInitMessage>
    | Partial<ConfigUpdateMessage>
    | Partial<SetAudioRingBufferMessage>
    | Partial<SetMicrophoneRingBufferMessage>
    | Partial<CoordinatorToWorkerSnapshotMessage>
    | Partial<AudioOutputHdaDemoStartMessage>
    | Partial<AudioOutputHdaDemoStopMessage>
    | Partial<AudioHdaCaptureSyntheticStartMessage>
    | Partial<CursorDemoStartMessage>
    | Partial<CursorDemoStopMessage>
    | undefined;
  if (!msg) return;

  const snapshotMsg = msg as Partial<CoordinatorToWorkerSnapshotMessage>;
  if (typeof snapshotMsg.kind === "string" && snapshotMsg.kind.startsWith("vm.snapshot.")) {
    const requestId = snapshotMsg.requestId;
    if (typeof requestId !== "number") return;

    switch (snapshotMsg.kind) {
      case "vm.snapshot.pause": {
        snapshotPaused = true;
        ctx.postMessage({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
        return;
      }
      case "vm.snapshot.resume": {
        snapshotPaused = false;
        // Snapshot pause/resume can introduce large wall-clock gaps (e.g. OPFS snapshot
        // streaming or slow restore). Any time-based audio producer loops must not
        // interpret that gap as "audio time elapsed" or they may burst-generate a
        // large number of frames on the next tick.
        //
        // Reset the producer deadline so the next audio tick observes ~0 elapsed time.
        const now = typeof performance?.now === "function" ? performance.now() : Date.now();
        nextAudioFillDeadlineMs = now;
        ctx.postMessage({ kind: "vm.snapshot.resumed", requestId, ok: true } satisfies VmSnapshotResumedMessage);
        return;
      }
      case "vm.snapshot.getCpuState": {
        try {
          const vm = wasmVm;
          if (!vm) {
            throw new Error("WasmVm is not initialized; cannot snapshot CPU state.");
          }
          const save = vm.save_state_v2;
          if (typeof save !== "function") {
            throw new Error("WasmVm.save_state_v2 is unavailable in this WASM build.");
          }
          const saved = save.call(vm);
          const cpu = saved?.cpu;
          const mmu = saved?.mmu;
          if (!(cpu instanceof Uint8Array) || !(mmu instanceof Uint8Array)) {
            throw new Error("WasmVm.save_state_v2 returned an unexpected result shape.");
          }

          // Always copy into a standalone ArrayBuffer so it can be transferred safely
          // (WASM memory / SharedArrayBuffer-backed views are not transferable).
          const cpuBuf = new Uint8Array(cpu.byteLength);
          cpuBuf.set(cpu);
          const mmuBuf = new Uint8Array(mmu.byteLength);
          mmuBuf.set(mmu);

          ctx.postMessage(
            { kind: "vm.snapshot.cpuState", requestId, ok: true, cpu: cpuBuf.buffer, mmu: mmuBuf.buffer } satisfies VmSnapshotCpuStateMessage,
            [cpuBuf.buffer, mmuBuf.buffer],
          );
        } catch (err) {
          ctx.postMessage({
            kind: "vm.snapshot.cpuState",
            requestId,
            ok: false,
            error: serializeVmSnapshotError(err),
          } satisfies VmSnapshotCpuStateMessage);
        }
        return;
      }
      case "vm.snapshot.setCpuState": {
        try {
          const vm = wasmVm;
          if (!vm) {
            throw new Error("WasmVm is not initialized; cannot restore CPU state.");
          }
          const load = vm.load_state_v2;
          if (typeof load !== "function") {
            throw new Error("WasmVm.load_state_v2 is unavailable in this WASM build.");
          }
          if (!(snapshotMsg.cpu instanceof ArrayBuffer) || !(snapshotMsg.mmu instanceof ArrayBuffer)) {
            throw new Error("vm.snapshot.setCpuState expected ArrayBuffer payloads.");
          }
          load.call(vm, new Uint8Array(snapshotMsg.cpu), new Uint8Array(snapshotMsg.mmu));
          ctx.postMessage({ kind: "vm.snapshot.cpuStateSet", requestId, ok: true } satisfies VmSnapshotCpuStateSetMessage);
        } catch (err) {
          ctx.postMessage({
            kind: "vm.snapshot.cpuStateSet",
            requestId,
            ok: false,
            error: serializeVmSnapshotError(err),
          } satisfies VmSnapshotCpuStateSetMessage);
        }
        return;
      }
      default:
        return;
    }
  }

  if ((msg as Partial<AudioOutputHdaDemoStopMessage>).type === "audioOutputHdaDemo.stop") {
    stopHdaDemo();
    return;
  }

  if ((msg as Partial<AudioOutputHdaDemoStartMessage>).type === "audioOutputHdaDemo.start") {
    void startHdaDemo(msg as AudioOutputHdaDemoStartMessage).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      console.error(err);
      ctx.postMessage({ type: "audioOutputHdaDemo.error", message } satisfies AudioOutputHdaDemoErrorMessage);
      stopHdaDemo();
    });
    return;
  }

  if ((msg as Partial<AudioHdaCaptureSyntheticStartMessage>).type === "audioHdaCaptureSynthetic.start") {
    void startHdaCaptureSynthetic(msg as AudioHdaCaptureSyntheticStartMessage);
    return;
  }

  if ((msg as Partial<AudioOutputHdaPciDeviceStopMessage>).type === "audioOutputHdaPciDevice.stop") {
    stopHdaPciDevice();
    return;
  }

  if ((msg as Partial<AudioOutputHdaPciDeviceStartMessage>).type === "audioOutputHdaPciDevice.start") {
    const startMsg = msg as AudioOutputHdaPciDeviceStartMessage;
    if (!io) {
      pendingHdaPciDeviceStart = startMsg;
      return;
    }
    pendingHdaPciDeviceStart = null;
    void startHdaPciDevice(startMsg).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      console.error(err);
      // If the device was partially programmed before failing, ensure we don't leave
      // a running stream/CORB/RIRB behind in the long-lived worker runtime.
      stopHdaPciDevice();
      ctx.postMessage({ type: "audioOutputHdaPciDevice.error", message } satisfies AudioOutputHdaPciDeviceErrorMessage);
    });
    return;
  }

  if ((msg as Partial<CursorDemoStopMessage>).type === "cursorDemo.stop") {
    stopCursorDemo();
    return;
  }

  if ((msg as Partial<CursorDemoStartMessage>).type === "cursorDemo.start") {
    startCursorDemo();
    return;
  }

  if ((msg as { kind?: unknown }).kind === "config.update") {
    currentConfig = (msg as ConfigUpdateMessage).config;
    currentConfigVersion = (msg as ConfigUpdateMessage).version;
    ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  if ((msg as Partial<SetMicrophoneRingBufferMessage>)?.type === "setMicrophoneRingBuffer") {
    attachMicrophoneRingBuffer(msg as SetMicrophoneRingBufferMessage);
    return;
  }

  if ((msg as Partial<SetAudioRingBufferMessage>)?.type === "setAudioRingBuffer") {
    attachAudioRingBuffer(msg as SetAudioRingBufferMessage);
    return;
  }

  if ((msg as { type?: unknown }).type === "setAudioOutputRingBuffer") {
    const legacy = msg as Partial<{
      ringBuffer: SharedArrayBuffer | null;
      sampleRate: number;
      channelCount: number;
      capacityFrames: number;
    }>;
    attachAudioRingBuffer({
      type: "setAudioRingBuffer",
      ringBuffer: (legacy.ringBuffer as SharedArrayBuffer | null) ?? null,
      capacityFrames: legacy.capacityFrames ?? 0,
      channelCount: legacy.channelCount ?? 0,
      dstSampleRate: legacy.sampleRate ?? 0,
    });
    return;
  }

  const init = msg as Partial<WorkerInitMessage>;
  if (init?.kind !== "init") return;
  void initAndRun(init as WorkerInitMessage);
};

async function initAndRun(init: WorkerInitMessage): Promise<void> {
  perf.spanBegin("worker:boot");
  try {
    perf.spanBegin("worker:init");
    try {
      role = init.role ?? "cpu";
      const segments = {
        control: init.controlSab!,
        guestMemory: init.guestMemory!,
        vgaFramebuffer: init.vgaFramebuffer!,
        scanoutState: init.scanoutState,
        scanoutStateOffsetBytes: init.scanoutStateOffsetBytes ?? 0,
        ioIpc: init.ioIpcSab!,
        sharedFramebuffer: init.sharedFramebuffer!,
        sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
      };
      const views = createSharedMemoryViews(segments);
      status = views.status;
      guestI32 = views.guestI32;
      guestU8 = views.guestU8;
      vgaFramebuffer = wrapSharedFramebuffer(segments.vgaFramebuffer, 0);
      frameState = init.frameStateSab ? new Int32Array(init.frameStateSab) : null;
      (globalThis as unknown as { __aeroScanoutState?: Int32Array }).__aeroScanoutState = views.scanoutStateI32;

      const demoFbEnd = DEMO_FB_OFFSET + DEMO_FB_MAX_BYTES;
      if (demoFbEnd > guestU8.byteLength) {
        const guestBytes = guestU8.byteLength;
        const message = `guestMemory too small for demo framebuffer: need >= ${demoFbEnd} bytes, got ${guestBytes} bytes.`;
        setReadyFlag(status, role, false);
        ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
        ctx.close();
        return;
      }

      if (init.perfChannel) {
        perfWriter = new PerfWriter(init.perfChannel.buffer, {
          workerKind: init.perfChannel.workerKind,
          runStartEpochMs: init.perfChannel.runStartEpochMs,
        });
        perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
      }

      initFramebufferHeader(vgaFramebuffer.header, {
        width: 320,
        height: 200,
        strideBytes: 320 * 4,
        format: FRAMEBUFFER_FORMAT_RGBA8888,
      });

      initSharedFramebufferViews(segments.sharedFramebuffer, segments.sharedFramebufferOffsetBytes);

      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset);
      const ioCmd = openRingByKind(segments.ioIpc, IO_IPC_CMD_QUEUE_KIND);
      const ioEvt = openRingByKind(segments.ioIpc, IO_IPC_EVT_QUEUE_KIND);
      ioNetTxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
      ioNetRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);
      irqBitmapLo = 0;
      irqBitmapHi = 0;
      irqRefCounts.fill(0);
      irqWarnedUnderflow.fill(0);
      irqWarnedSaturated.fill(0);
      Atomics.store(status, StatusIndex.CpuIrqBitmapLo, 0);
      Atomics.store(status, StatusIndex.CpuIrqBitmapHi, 0);
      Atomics.store(status, StatusIndex.CpuA20Enabled, 0);
      io = new AeroIpcIoClient(ioCmd, ioEvt, {
        onIrq: (irq, level) => {
          perf.instant("cpu:io:irq", "t", { irq, level });
          const idx = irq & 0xff;
          const flags = applyIrqRefCountChange(irqRefCounts, idx, level);
          if (import.meta.env.DEV && (flags & IRQ_REFCOUNT_UNDERFLOW) && irqWarnedUnderflow[idx] === 0) {
            irqWarnedUnderflow[idx] = 1;
            console.warn(`[cpu.worker] IRQ${idx} refcount underflow (irqLower without matching irqRaise?)`);
          }
          if (import.meta.env.DEV && (flags & IRQ_REFCOUNT_SATURATED) && irqWarnedSaturated[idx] === 0) {
            irqWarnedSaturated[idx] = 1;
            console.warn(`[cpu.worker] IRQ${idx} refcount saturated at 0xffff (irqRaise without matching irqLower?)`);
          }
          const asserted = irqRefCounts[idx] > 0;
          if (idx < 32) {
            const bit = 1 << idx;
            irqBitmapLo = asserted ? (irqBitmapLo | bit) >>> 0 : (irqBitmapLo & ~bit) >>> 0;
            Atomics.store(status, StatusIndex.CpuIrqBitmapLo, irqBitmapLo | 0);
          } else if (idx < 64) {
            const bit = 1 << (idx - 32);
            irqBitmapHi = asserted ? (irqBitmapHi | bit) >>> 0 : (irqBitmapHi & ~bit) >>> 0;
            Atomics.store(status, StatusIndex.CpuIrqBitmapHi, irqBitmapHi | 0);
          }
        },
        onA20: (enabled) => {
          perf.counter("cpu:io:a20Enabled", enabled ? 1 : 0);
          Atomics.store(status, StatusIndex.CpuA20Enabled, enabled ? 1 : 0);
        },
        onSerialOutput: (port, data) => {
          // Forward serial output to the coordinator via the runtime event ring.
          // Best-effort: don't block the CPU on log traffic.
          pushEvent({ kind: "serialOutput", port: port & 0xffff, data });
        },
        onReset: () => {
          // Reset requests are rare but important; use a blocking push so the
          // coordinator reliably observes the event and can restart the VM.
          pushEventBlocking({ kind: "resetRequest" }, 250);
        },
      });

      // WASM-side port I/O glue: the `crates/aero-wasm` Tier-0 VM imports these
      // globals and uses them for `IN`/`OUT` assists.
      //
      // The CPU worker tracks some basic perf counters here so we can attribute
      // time spent stalled on the IO worker separately from time spent executing
      // guest instructions.
      (globalThis as any).__aero_io_port_read = (port: number, size: number) => {
        const client = io;
        if (!client) return 0;
        const t0 = performance.now();
        try {
          perfDeviceExits += 1;
          perfDeviceIoReadBytes += size >>> 0;
          return client.portRead(port >>> 0, size >>> 0) >>> 0;
        } finally {
          perfIoWaitMs += performance.now() - t0;
        }
      };
      (globalThis as any).__aero_io_port_write = (port: number, size: number, value: number) => {
        const client = io;
        if (!client) return;
        const t0 = performance.now();
        try {
          perfDeviceExits += 1;
          perfDeviceIoWriteBytes += size >>> 0;
          client.portWrite(port >>> 0, size >>> 0, value >>> 0);
        } finally {
          perfIoWaitMs += performance.now() - t0;
        }
      };

      // WASM-side MMIO glue: the `crates/aero-wasm` Tier-0 VM calls these shims
      // when a guest memory access falls outside the configured guest RAM region.
      (globalThis as any).__aero_mmio_read = (addr: bigint, size: number) => {
        const client = io;
        if (!client) return 0;
        const t0 = performance.now();
        try {
          perfDeviceExits += 1;
          perfDeviceIoReadBytes += size >>> 0;
          return client.mmioRead(addr, size >>> 0) >>> 0;
        } finally {
          perfIoWaitMs += performance.now() - t0;
        }
      };
      (globalThis as any).__aero_mmio_write = (addr: bigint, size: number, value: number) => {
        const client = io;
        if (!client) return;
        const t0 = performance.now();
        try {
          perfDeviceExits += 1;
          perfDeviceIoWriteBytes += size >>> 0;
          client.mmioWrite(addr, size >>> 0, value >>> 0);
        } finally {
          perfIoWaitMs += performance.now() - t0;
        }
      };

      // Tier-1 JIT execution hook used by `WasmTieredVm`.
      //
      // The tiered VM calls out to JS so the CPU worker can execute JIT blocks that were
      // compiled/instantiated out-of-band. Until the worker installs a real dispatch table, keep a
      // safe default that forces an interpreter fallback.
      (globalThis as any).__aero_jit_call = (_tableIndex: number, cpuPtr: number, _jitCtxPtr: number) => {
        // Ensure the tiered runtime treats this as a non-committed execution (the stub did not run
        // any guest instructions).
        //
        // This mirrors `crates/aero-wasm/src/tiered_vm.rs` where the wasm backend expects the JS
        // host to clear a commit flag slot when it rolls back (or otherwise does not commit) a JIT
        // block.
        const commitFlagOffset = (() => {
          try {
            const api = wasmApi;
            if (!api) return undefined;

            // Newer WASM builds expose the Tier-1 JIT layout (including commit flag offset) via
            // `jit_abi_constants()`. Prefer that to avoid JS-side drift.
            const jitAbiFn = api.jit_abi_constants;
            if (typeof jitAbiFn === "function") {
              const jitAbi = jitAbiFn();
              const commitFlagOffset = readDemoNumber(jitAbi, "commit_flag_offset");
              if (typeof commitFlagOffset === "number" && Number.isFinite(commitFlagOffset) && commitFlagOffset >= 0) {
                return commitFlagOffset;
              }
            }

            // Fallback for older builds: use the dedicated `tiered_vm_jit_abi_layout()` helper.
            const layoutFn = api.tiered_vm_jit_abi_layout;
            if (typeof layoutFn !== "function") return undefined;
            const layout = layoutFn();
            const fallbackCommitFlagOffset = readDemoNumber(layout, "commit_flag_offset");
            if (
              typeof fallbackCommitFlagOffset === "number" &&
              Number.isFinite(fallbackCommitFlagOffset) &&
              fallbackCommitFlagOffset >= 0
            ) {
              return fallbackCommitFlagOffset;
            }
            return undefined;
          } catch {
            return undefined;
          }
        })();
        try {
          if (typeof commitFlagOffset === "number" && Number.isFinite(commitFlagOffset) && commitFlagOffset >= 0) {
            const commitFlagAddr = (cpuPtr + commitFlagOffset) >>> 0;
            new DataView(segments.guestMemory.buffer).setUint32(commitFlagAddr, 0, true);
          }
        } catch {
          // ignore
        }
        try {
          // Backwards-compatible signal for older/alternate JIT hosts that report commit status via a global flag.
          (globalThis as any).__aero_jit_last_committed = false;
        } catch {
          // ignore
        }
        return -1n;
      };
      setReadyFlag(status, role, true);
      ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
      if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role });

      const pending = pendingHdaPciDeviceStart;
      if (pending) {
        pendingHdaPciDeviceStart = null;
        void startHdaPciDevice(pending).catch((err) => {
          const message = err instanceof Error ? err.message : String(err);
          console.error(err);
          ctx.postMessage({ type: "audioOutputHdaPciDevice.error", message } satisfies AudioOutputHdaPciDeviceErrorMessage);
        });
      }

      // WASM is optional in this repo (CI runs with `--skip-wasm`), but worker init
      // should be fast enough to start pumping AudioWorklet ring buffers.
      //
      // Kick off WASM init in the background so the worker can enter its main loop
      // immediately (JS fallbacks will be used until WASM is ready).
      void initWasmInBackground(init, segments.guestMemory, segments.sharedFramebufferOffsetBytes);
    } finally {
      perf.spanEnd("worker:init");
    }
  } finally {
    perf.spanEnd("worker:boot");
  }

  void runLoop();
}

async function initWasmInBackground(
  init: WorkerInitMessage,
  guestMemory: WebAssembly.Memory,
  sharedFramebufferOffsetBytes: number,
): Promise<void> {
  try {
    const { api, variant } = await perf.spanAsync("wasm:init", () =>
      initWasmForContext({
        variant: init.wasmVariant,
        memory: guestMemory,
        module: init.wasmModule,
      }),
    );

    // Sanity-check that the provided `guestMemory` is actually wired up as
    // the WASM module's linear memory (imported+exported memory build).
    //
    // This enables shared-memory integration where JS + WASM + other workers
    // all observe the same guest RAM.
    //
    // We probe within the runtime-reserved region (not guest RAM) to keep the probe
    // side-effect-free from the guest's perspective.
    //
    // The probe helper hashes `context` to pick a stable per-context offset within the
    // reserved scratch window, so multiple workers can initialize concurrently without
    // racing on the same 32-bit word.
    assertWasmMemoryWiring({ api, memory: guestMemory, context: "cpu.worker" });

    wasmApi = api;
    cpuDemo = null;
    wasmVm = null;
    vmBooted = false;
    vmBootSectorLoaded = false;
    vmLastVgaTextBytes = null;
    vmNextBootSectorLoadAttemptMs = 0;
    const CpuWorkerDemo = api.CpuWorkerDemo;
    if (CpuWorkerDemo) {
      try {
        const ramSizeBytes = guestMemory.buffer.byteLength >>> 0;
        const framebufferLinearOffset = (sharedFramebufferOffsetBytes ?? 0) >>> 0;
        if (framebufferLinearOffset === 0) {
          throw new Error("shared framebuffer is not embedded in guest memory; CpuWorkerDemo requires an in-wasm framebuffer.");
        }
        const guestCounterLinearOffset = (guestU8.byteOffset + CPU_WORKER_DEMO_GUEST_COUNTER_OFFSET_BYTES) >>> 0;
        cpuDemo = new CpuWorkerDemo(
          ramSizeBytes,
          framebufferLinearOffset,
          CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
          CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
          CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
          guestCounterLinearOffset,
        );
      } catch (err) {
        console.warn("Failed to init CpuWorkerDemo wasm export:", err);
        cpuDemo = null;
      }
    }

    const WasmVm = api.WasmVm;
    if (WasmVm) {
      try {
        wasmVm = new WasmVm(guestU8.byteOffset >>> 0, guestU8.byteLength >>> 0);
      } catch (err) {
        console.warn("Failed to init WasmVm wasm export:", err);
        wasmVm = null;
      }
    }

    maybeInitAudioOutput();
    maybeInitMicBridge();

    if (Atomics.load(status, StatusIndex.StopRequested) !== 1) {
      const value = api.add(20, 22);
      ctx.postMessage({ type: MessageType.WASM_READY, role, variant, value } satisfies ProtocolMessage);
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    // WASM init is best-effort: keep the CPU worker alive so non-WASM demos
    // (including AudioWorklet ring-buffer smoke tests) can run in environments
    // where the generated wasm-pack output is absent.
    console.error("WASM init failed in CPU worker:", err);
    pushEvent({ kind: "log", level: "error", message: `WASM init failed: ${message}` });
    wasmApi = null;
    cpuDemo = null;
    maybeInitAudioOutput();
  }
}

async function runLoop(): Promise<void> {
  try {
    await runLoopInner();
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    pushEventBlocking({ kind: "panic", message });
    setReadyFlag(status, role, false);
    ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  }
}

async function runLoopInner(): Promise<void> {
  let running = false;
  const heartbeatIntervalMs = 250;
  const frameIntervalMs = 1000 / 60;
  const modeSwitchIntervalMs = 2500;
  const audioFillIntervalMs = 20;

  let nextHeartbeatMs = performance.now();
  let nextFrameMs = performance.now();
  let nextModeSwitchMs = performance.now() + modeSwitchIntervalMs;

  const modes = [
    { width: 320, height: 200 },
    { width: 640, height: 480 },
    { width: 1024, height: 768 },
  ] as const;
  let modeIndex = 0;
  let mode: (typeof modes)[number] = modes[0];
  let demoFbView = guestU8.subarray(DEMO_FB_OFFSET, DEMO_FB_OFFSET + mode.width * mode.height * 4);
  const demoFbLinearOffset = guestU8.byteOffset + DEMO_FB_OFFSET;
  const instructionsPerSharedFrame = BigInt(CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH * CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT);

  const maybeEmitPerfSample = () => {
    if (!perfWriter || !perfFrameHeader) return;
    const enabled = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
    const frameId = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
    if (!enabled) {
      perfLastFrameId = frameId;
      perfCpuMs = 0;
      perfInstructions = 0n;
      perfIoWaitMs = 0;
      perfDeviceExits = 0;
      perfDeviceIoReadBytes = 0;
      perfDeviceIoWriteBytes = 0;
      return;
    }
    if (frameId === 0) {
      // Perf is enabled, but the main thread hasn't published a frame ID yet.
      // Keep accumulating so the first non-zero frame can include this interval.
      perfLastFrameId = 0;
      return;
    }
    if (perfLastFrameId === 0) {
      // First observed frame ID after enabling perf. Only emit if we have some
      // accumulated work; otherwise establish a baseline and wait for the next
      // frame boundary.
      if (perfCpuMs <= 0 && perfInstructions === 0n) {
        perfLastFrameId = frameId;
        return;
      }
    }
    if (frameId === perfLastFrameId) return;
    perfLastFrameId = frameId;

    perfWriter.frameSample(frameId, {
      durations: { cpu_ms: perfCpuMs, io_ms: perfIoWaitMs },
      counters: {
        instructions: perfInstructions,
        // Reuse draw_calls as a generic "device exits" counter for now; the CPU
        // worker does not emit graphics samples yet.
        draw_calls: perfDeviceExits,
        io_read_bytes: perfDeviceIoReadBytes,
        io_write_bytes: perfDeviceIoWriteBytes,
      },
    });

    perfCpuMs = 0;
    perfInstructions = 0n;
    perfIoWaitMs = 0;
    perfDeviceExits = 0;
    perfDeviceIoReadBytes = 0;
    perfDeviceIoWriteBytes = 0;
  };

  while (true) {
    // Drain commands.
    while (true) {
      const bytes = commandRing.tryPop();
      if (!bytes) break;
      let cmd: Command;
      try {
        cmd = decodeCommand(bytes);
      } catch {
        continue;
      }

      if (cmd.kind === "nop") {
        running = true;
        perfCpuMs = 0;
        perfInstructions = 0n;
        perfIoWaitMs = 0;
        perfDeviceExits = 0;
        perfDeviceIoReadBytes = 0;
        perfDeviceIoWriteBytes = 0;
        perfLastFrameId = 0;
        nextHeartbeatMs = performance.now();
        nextFrameMs = performance.now();
        nextModeSwitchMs = performance.now() + modeSwitchIntervalMs;
        // Keep the legacy disk demo behind the "no active disk image" path so it
        // doesn't interfere with real VM boot fixtures.
        if (!diskDemoStarted && !currentConfig?.activeDiskImage) {
          diskDemoStarted = true;
          void runDiskReadDemo();
        }
        // Ack acts as a cheap "ring is alive" signal for the coordinator.
        pushEvent({ kind: "ack", seq: cmd.seq });
      } else if (cmd.kind === "shutdown") {
        Atomics.store(status, StatusIndex.StopRequested, 1);
      }
    }

    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

    if (snapshotPaused) {
      // VM execution is paused for snapshotting. Keep draining ring-buffer commands
      // (above), but avoid touching guest state until we receive a resume request.
      await commandRing.waitForDataAsync(100);
      continue;
    }

    if (running) {
      const now = performance.now();
      // Drain asynchronous device events (IRQs, A20, etc.) even when the CPU is
      // not actively waiting on an I/O roundtrip.
      io?.poll(64);

      const demoMode = !currentConfig?.activeDiskImage;
      // The AudioWorklet output ring buffer is treated as SPSC (single-producer,
      // single-consumer). In demo mode we let the CPU worker own the producer side
      // to generate a fallback sine tone or mic loopback for smoke tests.
      //
      // Once a real VM is requested (`activeDiskImage` is set), the I/O worker's
      // guest audio device must become the sole producer. If the CPU worker kept
      // writing its demo audio, we'd have multiple producers racing and corrupting
      // the ring buffer.
      if (workletBridge && audioDstSampleRate > 0 && audioCapacityFrames > 0) {
        if (nextAudioFillDeadlineMs === 0) nextAudioFillDeadlineMs = now;
        if (now >= nextAudioFillDeadlineMs) {
          if (demoMode) {
            let level = 0;
            let underruns = 0;
            let overruns = 0;
            const bridge = workletBridge as {
              buffer_level_frames?: () => number;
              underrun_count?: () => number;
              overrun_count?: () => number;
            };
            if (typeof bridge.buffer_level_frames === "function") level = bridge.buffer_level_frames() | 0;
            if (typeof bridge.underrun_count === "function") underruns = bridge.underrun_count() | 0;
            if (typeof bridge.overrun_count === "function") overruns = bridge.overrun_count() | 0;

            const targetFrames = Math.min(audioCapacityFrames, Math.floor(audioDstSampleRate / 5)); // ~200ms
            const need = Math.max(0, targetFrames - level);
            if (need > 0) {
              const maxWriteFrames = Math.min(need, Math.min(targetFrames, Math.floor(audioDstSampleRate / 10))); // cap to ~100ms
              if (maxWriteFrames > 0) {
                if (micRingBuffer) {
                  pumpMicLoopback(maxWriteFrames);
                } else {
                  sineTone?.write(workletBridge, maxWriteFrames, 440, audioDstSampleRate, 0.1);
                }
              }
            }

            // Export a tiny amount of producer-side telemetry for the UI.
            //
            // NOTE: These StatusIndex.Audio* counters are owned by the active audio
            // producer. The CPU worker publishes them for demo tone/loopback mode;
            // during real VM runs (guest HDA in the IO worker) the IO worker should
            // publish these instead.
            if (typeof bridge.buffer_level_frames === "function") level = bridge.buffer_level_frames() | 0;
            if (typeof bridge.underrun_count === "function") underruns = bridge.underrun_count() | 0;
            if (typeof bridge.overrun_count === "function") overruns = bridge.overrun_count() | 0;
            Atomics.store(status, StatusIndex.AudioBufferLevelFrames, level);
            Atomics.store(status, StatusIndex.AudioUnderrunCount, underruns);
            Atomics.store(status, StatusIndex.AudioOverrunCount, overruns);
            cpuIsAudioRingProducer = true;
          } else if (cpuIsAudioRingProducer) {
            // Transition out of demo mode: clear the CPU worker's telemetry once so
            // stale demo values don't persist, but avoid stomping telemetry that
            // may be written by the real I/O-worker audio producer.
            cpuIsAudioRingProducer = false;
            Atomics.store(status, StatusIndex.AudioBufferLevelFrames, 0);
            Atomics.store(status, StatusIndex.AudioUnderrunCount, 0);
            Atomics.store(status, StatusIndex.AudioOverrunCount, 0);
          }

          nextAudioFillDeadlineMs = now + audioFillIntervalMs;
        }
      } else {
        nextAudioFillDeadlineMs = 0;
        if (demoMode || cpuIsAudioRingProducer) {
          // Demo mode expects the CPU worker to own audio telemetry; keep it at 0
          // when no audio output ring is attached. In VM mode, only clear once if
          // we previously acted as the producer (so we don't stomp I/O-worker data).
          cpuIsAudioRingProducer = false;
          Atomics.store(status, StatusIndex.AudioBufferLevelFrames, 0);
          Atomics.store(status, StatusIndex.AudioUnderrunCount, 0);
          Atomics.store(status, StatusIndex.AudioOverrunCount, 0);
        }
      }

      if (!didIoDemo && io && Atomics.load(status, StatusIndex.IoReady) === 1 && !currentConfig?.activeDiskImage) {
        didIoDemo = true;
        try {
          perf.spanBegin("cpu:io:demo");
          // Read i8042 status (0x64) and command byte (via 0x20 -> 0x60).
          const status64 = perf.span("cpu:io:portRead 0x64", () => io!.portRead(0x64, 1));
          perf.counter("cpu:io:i8042:status", status64);
          const cmdByte = perf.span("cpu:io:i8042:readCommandByte", () => {
            io!.portWrite(0x64, 1, 0x20);
            return io!.portRead(0x60, 1);
          });
          perf.counter("cpu:io:i8042:commandByte", cmdByte);

          // PCI config + BAR MMIO demo (PciTestDevice on bus0/dev0/fn0).
          const pci = perf.span("cpu:io:pci:probe", () => {
            const pciEnable = 0x8000_0000;
            const cfgAddr = (reg: number) => (pciEnable | (reg & 0xfc)) >>> 0;
            const readDword = (reg: number) => {
              io!.portWrite(0x0cf8, 4, cfgAddr(reg));
              return io!.portRead(0x0cfc, 4) >>> 0;
            };
            const writeDword = (reg: number, value: number) => {
              io!.portWrite(0x0cf8, 4, cfgAddr(reg));
              io!.portWrite(0x0cfc, 4, value >>> 0);
            };

            const id = readDword(0x00);
            const vendorId = id & 0xffff;
            const deviceId = (id >>> 16) & 0xffff;
            const subsys = readDword(0x2c);
            const subsystemVendorId = subsys & 0xffff;
            const subsystemId = (subsys >>> 16) & 0xffff;
            const intx = readDword(0x3c);
            const irqLine = intx & 0xff;
            const irqPin = (intx >>> 8) & 0xff;
            const bar0 = readDword(0x10);

            // Enable memory-space decoding.
            writeDword(0x04, 0x0000_0002);

            const bar0Base = BigInt(bar0 >>> 0) & 0xffff_fff0n;
            io!.mmioWrite(bar0Base, 4, 0xcafe_babe);
            const mmio0 = io!.mmioRead(bar0Base, 4) >>> 0;

            return { vendorId, deviceId, subsystemVendorId, subsystemId, irqLine, irqPin, bar0, mmio0 };
          });
          perf.counter("cpu:io:pci:vendorId", pci.vendorId);
          perf.counter("cpu:io:pci:deviceId", pci.deviceId);
          perf.counter("cpu:io:pci:ssVendorId", pci.subsystemVendorId);
          perf.counter("cpu:io:pci:ssId", pci.subsystemId);
          perf.counter("cpu:io:pci:irqLine", pci.irqLine);
          perf.counter("cpu:io:pci:irqPin", pci.irqPin);
          perf.counter("cpu:io:pci:mmio0", pci.mmio0);

          // Emit a couple bytes on COM1; the I/O worker should mirror them back
          // as `serialOutput` events, which we forward to the coordinator/UI.
          perf.span("cpu:io:uart16550:write", () => {
            io!.portWrite(0x3f8, 1, "H".charCodeAt(0));
            io!.portWrite(0x3f8, 1, "i".charCodeAt(0));
            io!.portWrite(0x3f8, 1, "\r".charCodeAt(0));
            io!.portWrite(0x3f8, 1, "\n".charCodeAt(0));
          });

          // eslint-disable-next-line no-console
          console.log(
            `[cpu] io demo: i8042 status=0x${status64.toString(16)} cmdByte=0x${cmdByte.toString(
              16,
            )} pci=${pci.vendorId.toString(16)}:${pci.deviceId.toString(16)} ss=${pci.subsystemVendorId.toString(
              16,
            )}:${pci.subsystemId.toString(16)} intx=${pci.irqLine}/${pci.irqPin} bar0=0x${pci.bar0.toString(16)} mmio0=0x${pci.mmio0.toString(16)}`,
          );
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("[cpu] io demo failed:", err);
        } finally {
          perf.spanEnd("cpu:io:demo");
        }
      }

      if (now >= nextHeartbeatMs) {
        const counter = Atomics.add(status, StatusIndex.HeartbeatCounter, 1) + 1;
        if (cpuDemo) {
          cpuDemo.tick(now);
        } else {
          Atomics.add(guestI32, CPU_WORKER_DEMO_GUEST_COUNTER_INDEX, 1);
        }
        perf.counter("heartbeatCounter", counter);
        // Best-effort: heartbeat events are allowed to drop if the ring is full.
        pushEvent({ kind: "ack", seq: counter });
        nextHeartbeatMs = now + heartbeatIntervalMs;
      }

      if (now >= nextFrameMs) {
        const header = perfFrameHeader;
        const perfActive =
          !!perfWriter && !!header && Atomics.load(header, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
        const t0 = perfActive ? performance.now() : 0;
        const ioWaitBefore = perfIoWaitMs;

        const vmRequested = !!currentConfig?.activeDiskImage;
        const vmReady = vmRequested && !!wasmVm;

        if (vmReady && wasmVm && io && Atomics.load(status, StatusIndex.IoReady) === 1) {
          // Bootstrap: load LBA0 into guest RAM at 0x7C00 and jump into it.
          if (!vmBootSectorLoaded && now >= vmNextBootSectorLoadAttemptMs) {
            vmNextBootSectorLoadAttemptMs = now + 50;
            try {
              const diskT0 = performance.now();
              const evt = io.diskRead(0n, 512, 0x7c00n, 2000);
              perfIoWaitMs += performance.now() - diskT0;
              perfDeviceExits += 1;
              perfDeviceIoReadBytes += evt.bytes >>> 0;
              if (evt.ok) {
                vmBootSectorLoaded = true;
              }
            } catch (err) {
              // Best-effort: keep retrying until the harness opens a disk.
              if (import.meta.env.DEV) {
                console.warn("[cpu] vm boot: diskRead failed:", err);
              }
            }
          }

          if (vmBootSectorLoaded && !vmBooted) {
            try {
              wasmVm.reset_real_mode(0x7c00);
              vmBooted = true;
              vmLastVgaTextBytes = null;
            } catch (err) {
              console.error("[cpu] vm boot: reset_real_mode failed:", err);
            }
          }

          if (vmBooted) {
            try {
              const exit = wasmVm.run_slice(10_000);
              try {
                const executed = readDemoNumber(exit, "executed") ?? 0;
                if (perfActive) perfInstructions += BigInt(executed >>> 0);
              } finally {
                try {
                  (exit as { free?: () => void }).free?.();
                } catch {
                  // ignore
                }
              }

              // Translate VGA text memory writes into a tiny shared-framebuffer
              // signature so GPU worker paths can present something deterministic.
              const vga = guestU8.subarray(0xb8000, 0xb8000 + 10);
              let changed = vmLastVgaTextBytes === null;
              if (!changed && vmLastVgaTextBytes) {
                for (let i = 0; i < vga.length; i++) {
                  if (vga[i] !== vmLastVgaTextBytes[i]) {
                    changed = true;
                    break;
                  }
                }
              }
              if (changed) {
                publishSharedFramebufferVgaText(vga);
                vmLastVgaTextBytes = new Uint8Array(vga);
              }
            } catch (err) {
              console.error("[cpu] vm run_slice failed:", err);
            }
          } else {
            // VM requested but not ready yet: keep the demo render loop alive so
            // the UI stays responsive while we wait for a disk to be opened.
            if (vgaFramebuffer) {
              if (now >= nextModeSwitchMs) {
                modeIndex = (modeIndex + 1) % modes.length;
                mode = modes[modeIndex];

                const strideBytes = mode.width * 4;
                storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_WIDTH, mode.width);
                storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_HEIGHT, mode.height);
                storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_STRIDE_BYTES, strideBytes);
                addHeaderI32(vgaFramebuffer.header, HEADER_INDEX_CONFIG_COUNTER, 1);

                demoFbView = guestU8.subarray(DEMO_FB_OFFSET, DEMO_FB_OFFSET + strideBytes * mode.height);
                nextModeSwitchMs = now + modeSwitchIntervalMs;
              }

              const strideBytes = mode.width * 4;
              const wasmRender = wasmApi?.demo_render_rgba8888;
              if (typeof wasmRender === "function") {
                const instructions = wasmRender(demoFbLinearOffset, mode.width, mode.height, strideBytes, now) >>> 0;
                vgaFramebuffer.pixelsU8Clamped.set(demoFbView);
                if (perfActive) perfInstructions += BigInt(instructions);
              } else {
                // Fallback for dev builds where the wasm package hasn't been rebuilt yet.
                renderTestPattern(vgaFramebuffer, mode.width, mode.height, now);
                if (perfActive) perfInstructions += BigInt(mode.width * mode.height);
              }

              addHeaderI32(vgaFramebuffer.header, HEADER_INDEX_FRAME_COUNTER, 1);
            }

            if (cpuDemo) {
              const seq = cpuDemo.render_frame(0, now);
              if (perfActive) perfInstructions += instructionsPerSharedFrame;
              if (frameState) {
                Atomics.store(frameState, FRAME_SEQ_INDEX, seq);
                Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
              }
            } else {
              publishSharedFramebufferFrame();
            }
          }
        } else {
          // Legacy demo loop: render a moving gradient into the VGA scratch buffer
          // and publish a shared-framebuffer tile toggle for GPU smoke tests.
          if (vgaFramebuffer) {
            if (now >= nextModeSwitchMs) {
              modeIndex = (modeIndex + 1) % modes.length;
              mode = modes[modeIndex];

              const strideBytes = mode.width * 4;
              storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_WIDTH, mode.width);
              storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_HEIGHT, mode.height);
              storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_STRIDE_BYTES, strideBytes);
              addHeaderI32(vgaFramebuffer.header, HEADER_INDEX_CONFIG_COUNTER, 1);

              demoFbView = guestU8.subarray(DEMO_FB_OFFSET, DEMO_FB_OFFSET + strideBytes * mode.height);
              nextModeSwitchMs = now + modeSwitchIntervalMs;
            }

            const strideBytes = mode.width * 4;
            const wasmRender = wasmApi?.demo_render_rgba8888;
            if (typeof wasmRender === "function") {
              const instructions = wasmRender(demoFbLinearOffset, mode.width, mode.height, strideBytes, now) >>> 0;
              vgaFramebuffer.pixelsU8Clamped.set(demoFbView);
              if (perfActive) perfInstructions += BigInt(instructions);
            } else {
              // Fallback for dev builds where the wasm package hasn't been rebuilt yet.
              renderTestPattern(vgaFramebuffer, mode.width, mode.height, now);
              if (perfActive) perfInstructions += BigInt(mode.width * mode.height);
            }

            addHeaderI32(vgaFramebuffer.header, HEADER_INDEX_FRAME_COUNTER, 1);
          }

          if (cpuDemo) {
            const seq = cpuDemo.render_frame(0, now);
            if (perfActive) perfInstructions += instructionsPerSharedFrame;
            if (frameState) {
              Atomics.store(frameState, FRAME_SEQ_INDEX, seq);
              Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
            }
          } else {
            publishSharedFramebufferFrame();
          }
        }

        if (perfActive) {
          const elapsed = performance.now() - t0;
          const ioWaitDelta = perfIoWaitMs - ioWaitBefore;
          perfCpuMs += Math.max(0, elapsed - ioWaitDelta);
        }
        nextFrameMs = now + frameIntervalMs;
      }

      maybeEmitPerfSample();
    }

    // Sleep until either new commands arrive or the next heartbeat tick.
    if (!running) {
      // IMPORTANT: Use the async wait path so the worker stays responsive to
      // structured `postMessage` attachments (e.g. audio ring + mic ring buffer)
      // while idle. A blocking Atomics.wait loop would starve the message queue.
      await commandRing.waitForDataAsync(1000);
      continue;
    }

    const now = performance.now();
    const nextAudioMs = workletBridge ? nextAudioFillDeadlineMs : Number.POSITIVE_INFINITY;
    const until = Math.min(nextHeartbeatMs, nextFrameMs, nextModeSwitchMs, nextAudioMs) - now;
    await commandRing.waitForDataAsync(Math.max(0, Math.min(heartbeatIntervalMs, until)));
  }

  setReadyFlag(status, role, false);
  detachMicBridge();
  detachAudioOutput();
  if (cpuDemo) {
    cpuDemo.free();
    cpuDemo = null;
  }
  if (wasmVm) {
    try {
      wasmVm.free();
    } catch {
      // ignore
    }
    wasmVm = null;
  }
  ctx.close();
}

async function runDiskReadDemo(): Promise<void> {
  const client = io;
  if (!client) return;

  // Wait until the I/O worker reports ready.
  while (Atomics.load(status, StatusIndex.IoReady) !== 1) {
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }

  // Read the first sector into guest RAM at an arbitrary scratch offset.
  const guestOffset = 0x1000n;
  const len = 512;
  try {
    const evt = client.diskRead(0n, len, guestOffset, 2000);
    diskDemoResponses += 1;
    perf.counter("diskReadDemoResponses", diskDemoResponses);
    if (perf.traceEnabled) perf.instant("diskReadDemoResp", "t", evt as unknown as Record<string, unknown>);

    if (evt.ok && evt.bytes >= 4) {
      const firstDword = new DataView(guestU8.buffer, guestU8.byteOffset + Number(guestOffset), 4).getUint32(0, true);
      perf.counter("diskReadDemoFirstDword", firstDword);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[cpu] disk read demo failed:", err);
    if (perf.traceEnabled) {
      perf.instant("diskReadDemoError", "t", { message: err instanceof Error ? err.message : String(err) });
    }
  }
}

function pushEvent(evt: Event): void {
  try {
    eventRing.tryPush(encodeEvent(evt));
  } catch {
    // Ignore malformed events.
  }
}

function pushEventBlocking(evt: Event, timeoutMs = 1000): void {
  const payload = encodeEvent(evt);
  if (eventRing.tryPush(payload)) return;
  try {
    eventRing.pushBlocking(payload, timeoutMs);
  } catch {
    // Ignore if the ring is wedged; postMessage ERROR remains a backup.
  }
}

function initSharedFramebufferViews(shared: SharedArrayBuffer, offsetBytes: number): void {
  const header = new Int32Array(shared, offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
  // Stored in an Int32Array header (Atomics requires a signed typed array), so
  // compare against the signed i32 constants from `ipc/shared-layout.ts`.
  const magic = Atomics.load(header, SharedFramebufferHeaderIndex.MAGIC) | 0;
  const version = Atomics.load(header, SharedFramebufferHeaderIndex.VERSION) | 0;
  if (magic !== SHARED_FRAMEBUFFER_MAGIC || version !== SHARED_FRAMEBUFFER_VERSION) {
    throw new Error(
      `shared framebuffer header mismatch: magic=0x${(magic >>> 0).toString(16)} version=${version} expected magic=0x${(SHARED_FRAMEBUFFER_MAGIC >>> 0).toString(16)} version=${SHARED_FRAMEBUFFER_VERSION}`,
    );
  }

  const layout = layoutFromHeader(header);
  const stridePixels = layout.strideBytes / 4;
  const pixelWords = stridePixels * layout.height;

  sharedHeader = header;
  sharedLayout = layout;
  sharedSlot0 = new Uint32Array(shared, offsetBytes + layout.framebufferOffsets[0], pixelWords);
  sharedSlot1 = new Uint32Array(shared, offsetBytes + layout.framebufferOffsets[1], pixelWords);
  sharedDirty0 =
    layout.dirtyWordsPerBuffer === 0 ? null : new Uint32Array(shared, offsetBytes + layout.dirtyOffsets[0], layout.dirtyWordsPerBuffer);
  sharedDirty1 =
    layout.dirtyWordsPerBuffer === 0 ? null : new Uint32Array(shared, offsetBytes + layout.dirtyOffsets[1], layout.dirtyWordsPerBuffer);
}

function publishSharedFramebufferFrame(): void {
  if (!sharedHeader || !sharedLayout || !sharedSlot0 || !sharedSlot1) return;

  const active = Atomics.load(sharedHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
  const back = active ^ 1;

  const backPixels = back === 0 ? sharedSlot0 : sharedSlot1;
  const backDirty = back === 0 ? sharedDirty0 : sharedDirty1;

  const base = 0xff00ff00; // RGBA green in little-endian u32
  const tileColor = sharedTileToggle ? 0xff0000ff /* RGBA red */ : base;
  // Advance the toggle once per full double-buffer cycle. If we flip the color every
  // frame while also flipping which buffer is active, each slot ends up with a
  // stable color (slot0 always red, slot1 always green). That makes smoke tests
  // flaky if the presenter consistently drops one parity of frames.
  if (back === 0) sharedTileToggle = !sharedTileToggle;

  const backSlotSeq = Atomics.load(
    sharedHeader,
    back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
  );
  const backSlotInitialized = backSlotSeq !== 0;

  // Full frame is constant green; only the top-left tile toggles, allowing dirty-rect
  // uploads (when supported) to preserve the rest of the texture.
  if (!backSlotInitialized) {
    backPixels.fill(base);
  }

  const tileSize = sharedLayout.tileSize || sharedLayout.width;
  const tileW = Math.min(tileSize, sharedLayout.width);
  const tileH = Math.min(tileSize, sharedLayout.height);
  const stridePixels = sharedLayout.strideBytes / 4;
  for (let y = 0; y < tileH; y += 1) {
    const row = y * stridePixels;
    backPixels.fill(tileColor, row, row + tileW);
  }

  const prevSeq = Atomics.load(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ);
  const newSeq = (prevSeq + 1) | 0;

  if (backDirty) {
    if (!backSlotInitialized) {
      // Initialize the presenter texture for each slot with a full upload the first
      // time we write it (double buffering means slot 1 is uninitialized on frame 1).
      backDirty.fill(0xffffffff);
    } else {
      backDirty.fill(0);
      backDirty[0] = 1; // mark tile 0 (top-left)
    }
  }

  Atomics.store(
    sharedHeader,
    back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
    newSeq,
  );
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
  Atomics.notify(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);

  if (frameState) {
    Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
    Atomics.notify(frameState, FRAME_STATUS_INDEX);
  }
}

function publishSharedFramebufferVgaText(vgaTextBytes: Uint8Array): void {
  if (!sharedHeader || !sharedLayout || !sharedSlot0 || !sharedSlot1) return;

  const active = Atomics.load(sharedHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
  const back = active ^ 1;

  const backPixels = back === 0 ? sharedSlot0 : sharedSlot1;
  const backDirty = back === 0 ? sharedDirty0 : sharedDirty1;

  const base = 0xff00ff00; // RGBA green

  const backSlotSeq = Atomics.load(
    sharedHeader,
    back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
  );
  const backSlotInitialized = backSlotSeq !== 0;

  if (!backSlotInitialized) {
    backPixels.fill(base);
  }

  // Encode the first 5 VGA text cells (10 bytes) into the first row of pixels:
  // pixel[i] = RGBA(char, attr, 0, 255).
  const cells = Math.min(5, Math.floor(vgaTextBytes.length / 2));
  for (let i = 0; i < cells; i++) {
    const ch = vgaTextBytes[i * 2] ?? 0;
    const attr = vgaTextBytes[i * 2 + 1] ?? 0;
    backPixels[i] = ((0xff << 24) | (attr << 8) | ch) >>> 0;
  }

  const prevSeq = Atomics.load(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ);
  const newSeq = (prevSeq + 1) | 0;

  if (backDirty) {
    if (!backSlotInitialized) {
      backDirty.fill(0xffffffff);
    } else {
      backDirty.fill(0);
      backDirty[0] = 1;
    }
  }

  Atomics.store(
    sharedHeader,
    back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
    newSeq,
  );
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
  Atomics.notify(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);

  if (frameState) {
    Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
    Atomics.notify(frameState, FRAME_STATUS_INDEX);
  }
}

function renderTestPattern(
  fb: ReturnType<typeof wrapSharedFramebuffer>,
  width: number,
  height: number,
  nowMs: number,
): void {
  const pixels = fb.pixelsU8Clamped;
  const strideBytes = width * 4;
  const t = nowMs * 0.001;

  for (let y = 0; y < height; y++) {
    const base = y * strideBytes;
    for (let x = 0; x < width; x++) {
      const i = base + x * 4;
      pixels[i + 0] = (x + t * 60) & 255;
      pixels[i + 1] = (y + t * 35) & 255;
      pixels[i + 2] = ((x ^ y) + t * 20) & 255;
      pixels[i + 3] = 255;
    }
  }
}

// Keep config in scope for devtools inspection.
void currentConfig;
