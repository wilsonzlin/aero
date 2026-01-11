import type { AeroConfig } from "../config/aero_config";
import type { PlatformFeatureReport } from "../platform/features";
import type { WasmVariant } from "./wasm_context";
import type { WorkerRole } from "./shared_layout";

/**
 * Stable numeric message identifiers used for compact ring-buffer encoding.
 * Keep IDs stable over time (don't reorder), even if names change.
 */
export enum MessageType {
  READY = 1,
  START = 2,
  STOP = 3,
  HEARTBEAT = 4,
  ERROR = 5,
  WASM_READY = 6,
}

export const WORKER_ROLE_IDS: Record<WorkerRole, number> = {
  cpu: 1,
  gpu: 2,
  io: 3,
  jit: 4,
};

export function workerRoleToId(role: WorkerRole): number {
  return WORKER_ROLE_IDS[role];
}

export function idToWorkerRole(id: number): WorkerRole | null {
  for (const [role, roleId] of Object.entries(WORKER_ROLE_IDS) as Array<[WorkerRole, number]>) {
    if (roleId === id) return role;
  }
  return null;
}

const WASM_VARIANT_IDS: Record<WasmVariant, number> = {
  single: 1,
  threaded: 2,
};

function wasmVariantToId(variant: WasmVariant): number {
  return WASM_VARIANT_IDS[variant];
}

function idToWasmVariant(id: number): WasmVariant | null {
  for (const [variant, variantId] of Object.entries(WASM_VARIANT_IDS) as Array<[WasmVariant, number]>) {
    if (variantId === id) return variant;
  }
  return null;
}

export type ReadyMessage = {
  type: MessageType.READY;
  role: WorkerRole;
};

export type StartMessage = {
  type: MessageType.START;
};

export type StopMessage = {
  type: MessageType.STOP;
};

export type HeartbeatMessage = {
  type: MessageType.HEARTBEAT;
  role: WorkerRole;
  counter: number;
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
  | StartMessage
  | StopMessage
  | HeartbeatMessage
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
   * CPU<->I/O AIPC buffer used for high-frequency device operations (disk I/O,
   * etc). Contains at least a command queue and an event queue.
   */
  ioIpcSab: SharedArrayBuffer;
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

export type CoordinatorToWorkerPostMessage = WorkerInitMessage | ConfigUpdateMessage;
export type WorkerToCoordinatorPostMessage = ReadyMessage | ErrorMessage | WasmReadyMessage | ConfigAckMessage;

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

export function encodeProtocolMessage(msg: ProtocolMessage): Uint8Array {
  switch (msg.type) {
    case MessageType.START:
    case MessageType.STOP: {
      return Uint8Array.of(msg.type);
    }
    case MessageType.READY: {
      return Uint8Array.of(msg.type, workerRoleToId(msg.role));
    }
    case MessageType.HEARTBEAT: {
      const buf = new Uint8Array(1 + 1 + 4);
      buf[0] = msg.type;
      buf[1] = workerRoleToId(msg.role);
      new DataView(buf.buffer).setUint32(2, msg.counter >>> 0, true);
      return buf;
    }
    case MessageType.ERROR: {
      const encoded = textEncoder.encode(msg.message);
      if (encoded.byteLength > 0xffff) {
        throw new Error("ERROR message too large to encode");
      }
      const buf = new Uint8Array(1 + 1 + 2 + encoded.byteLength);
      buf[0] = msg.type;
      buf[1] = workerRoleToId(msg.role);
      const view = new DataView(buf.buffer);
      view.setUint16(2, encoded.byteLength, true);
      buf.set(encoded, 4);
      return buf;
    }
    case MessageType.WASM_READY: {
      const buf = new Uint8Array(1 + 1 + 1 + 4);
      buf[0] = msg.type;
      buf[1] = workerRoleToId(msg.role);
      buf[2] = wasmVariantToId(msg.variant);
      const view = new DataView(buf.buffer);
      view.setInt32(3, msg.value | 0, true);
      return buf;
    }
    default: {
      // Ensure exhaustive checking if MessageType changes.
      const neverType: never = msg;
      throw new Error(`Unsupported message type: ${String(neverType)}`);
    }
  }
}

export function decodeProtocolMessage(bytes: Uint8Array): ProtocolMessage | null {
  if (bytes.byteLength === 0) return null;
  const type = bytes[0] as MessageType;

  switch (type) {
    case MessageType.START:
      return { type: MessageType.START };
    case MessageType.STOP:
      return { type: MessageType.STOP };
    case MessageType.READY: {
      if (bytes.byteLength !== 2) return null;
      const role = idToWorkerRole(bytes[1]);
      if (!role) return null;
      return { type: MessageType.READY, role };
    }
    case MessageType.HEARTBEAT: {
      if (bytes.byteLength !== 6) return null;
      const role = idToWorkerRole(bytes[1]);
      if (!role) return null;
      const counter = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(2, true);
      return { type: MessageType.HEARTBEAT, role, counter };
    }
    case MessageType.ERROR: {
      if (bytes.byteLength < 4) return null;
      const role = idToWorkerRole(bytes[1]);
      if (!role) return null;
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      const msgLen = view.getUint16(2, true);
      if (bytes.byteLength !== 4 + msgLen) return null;
      const message = textDecoder.decode(bytes.subarray(4));
      return { type: MessageType.ERROR, role, message };
    }
    case MessageType.WASM_READY: {
      if (bytes.byteLength !== 7) return null;
      const role = idToWorkerRole(bytes[1]);
      if (!role) return null;
      const variant = idToWasmVariant(bytes[2]);
      if (!variant) return null;
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      const value = view.getInt32(3, true);
      return { type: MessageType.WASM_READY, role, variant, value };
    }
    default:
      return null;
  }
}
