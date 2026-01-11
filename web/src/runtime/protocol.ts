import type { AeroConfig } from "../config/aero_config";
import type { PlatformFeatureReport } from "../platform/features";
import type { WasmVariant } from "./wasm_context";
import type { WorkerRole } from "./shared_layout";

/**
 * `postMessage` control messages exchanged between the coordinator and workers.
 *
 * High-frequency traffic uses the AIPC command/event rings (`web/src/ipc/*`).
 */
export enum MessageType {
  READY = 1,
  ERROR = 5,
  WASM_READY = 6,
}

export type ReadyMessage = {
  type: MessageType.READY;
  role: WorkerRole;
};

export type ErrorMessage = {
  type: MessageType.ERROR;
  role: WorkerRole;
  message: string;
};

export type WasmReadyMessage = {
  type: MessageType.WASM_READY;
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
  /** Optional precompiled WASM module (structured-cloneable in modern browsers). */
  wasmModule?: WebAssembly.Module;
  /** Variant corresponding to `wasmModule` (or the preferred variant when no module is sent). */
  wasmVariant?: WasmVariant;
  /**
   * CPU<->I/O AIPC buffer used for high-frequency device operations (disk I/O,
   * etc). Contains at least a command queue and an event queue.
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
   * Layout is defined in `src/shared/frameProtocol.ts`.
   */
  frameStateSab?: SharedArrayBuffer;
  /**
   * Optional perf channel attachment that allows workers to write samples into the
   * shared Perf HUD ring buffers.
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

export type CoordinatorToWorkerPostMessage =
  | WorkerInitMessage
  | ConfigUpdateMessage
  | SetMicrophoneRingBufferMessage
  | SetAudioRingBufferMessage;
export type WorkerToCoordinatorPostMessage = ReadyMessage | ErrorMessage | WasmReadyMessage | ConfigAckMessage;
