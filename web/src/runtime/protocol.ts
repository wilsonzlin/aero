import type { AeroConfig } from "../config/aero_config";
import type { PlatformFeatureReport } from "../platform/features";
import type { WasmVariant } from "./wasm_context";
import type { WorkerRole } from "./shared_layout";

/**
 * `postMessage` control messages exchanged between the coordinator and workers.
 *
 * High-frequency traffic uses the AIPC command/event rings (`web/src/ipc/*`).
 */
export const MessageType = {
  READY: 1,
  ERROR: 5,
  WASM_READY: 6,
} as const;

export type MessageType = (typeof MessageType)[keyof typeof MessageType];

export type ReadyMessage = {
  type: typeof MessageType.READY;
  role: WorkerRole;
};

export type ErrorMessage = {
  type: typeof MessageType.ERROR;
  role: WorkerRole;
  message: string;
};

export type WasmReadyMessage = {
  type: typeof MessageType.WASM_READY;
  role: WorkerRole;
  variant: WasmVariant;
  value: number;
};

export type ProtocolMessage =
  | ReadyMessage
  | ErrorMessage
  | WasmReadyMessage;

/**
 * `postMessage`-only init message used to hand SharedArrayBuffers to workers.
 *
 * Ring buffers are used after init for ongoing IPC.
 */
export type WorkerInitMessage = {
  kind: "init";
  role: WorkerRole;
  controlSab: SharedArrayBuffer;
  guestMemory: WebAssembly.Memory;
  vgaFramebuffer: SharedArrayBuffer;
  /**
   * Shared scanout descriptor used to select which framebuffer is currently presented.
   *
   * Layout/protocol: `web/src/ipc/scanout_state.ts` / `crates/aero-shared/src/scanout_state.rs`.
   */
  scanoutState?: SharedArrayBuffer;
  /**
   * Byte offset within `scanoutState` where the scanout header begins (typically 0).
   */
  scanoutStateOffsetBytes?: number;
  /** Optional precompiled WASM module (structured-cloneable in modern browsers). */
  wasmModule?: WebAssembly.Module;
  /** Variant corresponding to `wasmModule` (or the preferred variant when no module is sent). */
  wasmVariant?: WasmVariant;
  /**
   * CPU<->I/O AIPC buffer used for high-frequency operations (disk I/O, port I/O,
   * MMIO, etc). Contains at least a command queue and an event queue.
   */
  ioIpcSab: SharedArrayBuffer;
  /**
   * Shared CPU→GPU framebuffer region backing the runtime demo.
   *
   * Layout is defined in `src/ipc/shared-layout.ts`.
   */
  sharedFramebuffer: SharedArrayBuffer;
  /**
   * Byte offset within `sharedFramebuffer` where the framebuffer header begins.
   */
  sharedFramebufferOffsetBytes: number;
  /**
   * Optional platform feature report captured by the main thread.
   *
   * Workers may use this to avoid redundant capability probing and to gate
   * features that are sensitive to CSP (e.g. dynamic WASM compilation).
   */
  platformFeatures?: PlatformFeatureReport;
  /**
   * Optional SharedArrayBuffer used for main-thread ↔ GPU-worker frame pacing state.
   *
   * Layout is defined in `src/ipc/gpu-protocol.ts` (FRAME_* constants).
   */
  frameStateSab?: SharedArrayBuffer;
  /**
   * Optional perf channel attachment that allows workers to write samples into the
   * shared Perf HUD ring buffers.
   *
   * `frameHeader` layout is defined in `src/perf/shared.js` (FRAME_ID, T_US, ENABLED).
   */
  perfChannel?: {
    runStartEpochMs: number;
    frameHeader: SharedArrayBuffer;
    buffer: SharedArrayBuffer;
    workerKind: number;
  };
};

export type SetMicrophoneRingBufferMessage = {
  type: "setMicrophoneRingBuffer";
  ringBuffer: SharedArrayBuffer | null;
  sampleRate: number;
};

export type SetAudioRingBufferMessage = {
  type: "setAudioRingBuffer";
  ringBuffer: SharedArrayBuffer | null;
  capacityFrames: number;
  channelCount: number;
  dstSampleRate: number;
};

/**
 * Structured config update pushed from the coordinator to workers.
 *
 * This is intentionally a `postMessage` payload (not ring-buffer encoded) since
 * it is low-frequency / human-facing configuration.
 */
export type ConfigUpdateMessage = {
  kind: "config.update";
  version: number;
  config: AeroConfig;
  /**
   * Optional platform feature report captured by the main thread.
   *
   * This is included so workers can react to feature changes without needing
   * to be restarted (rare, but useful for dev/testing overrides).
   */
  platformFeatures?: PlatformFeatureReport;
};

export type ConfigAckMessage = {
  kind: "config.ack";
  version: number;
};

export type SerialOutputMessage = {
  kind: "serial.output";
  port: number;
  data: Uint8Array;
};

export type ResetRequestMessage = {
  kind: "reset.request";
  reason?: string;
};

export type NetTraceEnableMessage = {
  kind: "net.trace.enable";
};

export type NetTraceDisableMessage = {
  kind: "net.trace.disable";
};

export type NetTraceClearMessage = {
  kind: "net.trace.clear";
};

export type NetTraceTakePcapngMessage = {
  kind: "net.trace.take_pcapng";
  requestId: number;
};

export type NetTraceExportPcapngMessage = {
  // Non-draining snapshot export.
  kind: "net.trace.export_pcapng";
  requestId: number;
};

export type NetTraceStatusMessage = {
  kind: "net.trace.status";
  requestId: number;
};

export type NetTracePcapngMessage = {
  kind: "net.trace.pcapng";
  requestId: number;
  bytes: ArrayBuffer;
};

export type NetTraceStatusResponseMessage = {
  kind: "net.trace.status";
  requestId: number;
  enabled: boolean;
  records: number;
  bytes: number;
  droppedRecords?: number;
  droppedBytes?: number;
};

export type CoordinatorToWorkerPostMessage =
  | WorkerInitMessage
  | ConfigUpdateMessage
  | SetMicrophoneRingBufferMessage
  | SetAudioRingBufferMessage
  | NetTraceEnableMessage
  | NetTraceDisableMessage
  | NetTraceClearMessage
  | NetTraceTakePcapngMessage
  | NetTraceExportPcapngMessage
  | NetTraceStatusMessage;

/**
 * Cursor image update forwarded from an emulation worker (typically CPU/WASM) to the coordinator.
 *
 * The coordinator is responsible for forwarding this to the GPU presenter worker (if present).
 *
 * NOTE: This intentionally uses `postMessage` (not the ring-buffer IPC) because cursor images are
 * relatively large payloads and can exceed the fixed control-ring capacity.
 */
export type CursorSetImageMessage = {
  kind: "cursor.set_image";
  width: number;
  height: number;
  rgba8: ArrayBuffer;
};

/**
 * Cursor state update forwarded from an emulation worker (typically CPU/WASM) to the coordinator.
 *
 * Coordinates use a top-left origin in the source framebuffer coordinate space.
 */
export type CursorSetStateMessage = {
  kind: "cursor.set_state";
  enabled: boolean;
  x: number;
  y: number;
  hotX: number;
  hotY: number;
};

export type WorkerToCoordinatorPostMessage =
  | ReadyMessage
  | ErrorMessage
  | WasmReadyMessage
  | ConfigAckMessage
  | SerialOutputMessage
  | ResetRequestMessage
  | CursorSetImageMessage
  | CursorSetStateMessage
  | NetTracePcapngMessage
  | NetTraceStatusResponseMessage;
