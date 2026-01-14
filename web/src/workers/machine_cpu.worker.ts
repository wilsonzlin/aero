/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { createSharedMemoryViews, ringRegionsForWorker, setReadyFlag, StatusIndex, type WorkerRole } from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import { RingBuffer } from "../ipc/ring_buffer";

/**
 * Minimal "machine CPU" worker entrypoint.
 *
 * This worker participates in the coordinator's standard `config.update` + `init` protocol and
 * must be robust in environments where WASM builds are unavailable (e.g. CI runs with `--skip-wasm`).
 *
 * NOTE: The current implementation only validates bootstrap wiring and does not yet drive a
 * full-system VM loop; it exists so the worker lifecycle and init contract can be tested via
 * `node:worker_threads` without depending on a built WASM module.
 */

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let role: WorkerRole = "cpu";
let status: Int32Array | null = null;
let commandRing: RingBuffer | null = null;
let eventRing: RingBuffer | null = null;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

function post(msg: ProtocolMessage | ConfigAckMessage): void {
  ctx.postMessage(msg);
}

function pushEvent(evt: Event): void {
  const ring = eventRing;
  if (!ring) return;
  try {
    ring.tryPush(encodeEvent(evt));
  } catch {
    // best-effort
  }
}

ctx.onmessage = (ev) => {
  const msg = ev.data as unknown;

  if ((msg as { kind?: unknown }).kind === "config.update") {
    const update = msg as ConfigUpdateMessage;
    currentConfig = update.config;
    currentConfigVersion = update.version;
    post({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  const init = msg as Partial<WorkerInitMessage>;
  if (init?.kind === "init") {
    void initAndRun(init as WorkerInitMessage);
  }
};

async function initAndRun(init: WorkerInitMessage): Promise<void> {
  role = init.role ?? "cpu";

  try {
    const segments = {
      control: init.controlSab,
      guestMemory: init.guestMemory,
      vram: init.vram,
      vgaFramebuffer: init.vgaFramebuffer,
      scanoutState: init.scanoutState,
      scanoutStateOffsetBytes: init.scanoutStateOffsetBytes ?? 0,
      cursorState: init.cursorState,
      cursorStateOffsetBytes: init.cursorStateOffsetBytes ?? 0,
      ioIpc: init.ioIpcSab,
      sharedFramebuffer: init.sharedFramebuffer,
      sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
    };

    const views = createSharedMemoryViews(segments);
    status = views.status;

    const regions = ringRegionsForWorker(role);
    commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
    eventRing = new RingBuffer(segments.control, regions.event.byteOffset);

    // Emit READY immediately; WASM initialization is best-effort and should not prevent the
    // worker from participating in the coordinator lifecycle (mirrors cpu.worker.ts behavior).
    setReadyFlag(status, role, true);
    post({ type: MessageType.READY, role } satisfies ProtocolMessage);

    // Kick off WASM init in the background. It may fail when the wasm-pack output is absent
    // (e.g. CI runs with `--skip-wasm`); keep the worker alive regardless.
    void initWasmInBackground(init, init.guestMemory);

    void runLoop();
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (status) setReadyFlag(status, role, false);
    post({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  }
}

async function runLoop(): Promise<void> {
  const ring = commandRing;
  const st = status;
  if (!ring || !st) return;

  try {
    while (Atomics.load(st, StatusIndex.StopRequested) !== 1) {
      // Drain all pending commands.
      while (true) {
        const payload = ring.tryPop();
        if (!payload) break;
        let cmd: Command;
        try {
          cmd = decodeCommand(payload);
        } catch {
          // Corrupt or unknown command; ignore.
          continue;
        }

        switch (cmd.kind) {
          case "nop":
            pushEvent({ kind: "ack", seq: cmd.seq } satisfies Event);
            break;
          case "shutdown":
            Atomics.store(st, StatusIndex.StopRequested, 1);
            break;
          default:
            // Ignore other commands for now; the machine CPU worker currently only exists to
            // validate worker lifecycle wiring under Node worker_threads.
            break;
        }
      }

      await ring.waitForDataAsync(250);
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    pushEvent({ kind: "panic", message } satisfies Event);
    setReadyFlag(st, role, false);
    post({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  } finally {
    setReadyFlag(st, role, false);
  }
}

async function initWasmInBackground(init: WorkerInitMessage, guestMemory: WebAssembly.Memory): Promise<void> {
  // `initWasmForContext` (via `wasm_loader.ts`) relies on Vite-only `import.meta.glob`.
  // When this worker is executed directly under Node (e.g. in worker_threads tests),
  // `import.meta.glob` is not defined, so skip WASM init entirely.
  if (typeof (import.meta as unknown as { glob?: unknown }).glob !== "function") return;

  try {
    const { initWasmForContext } = await import("../runtime/wasm_context");
    const { api, variant } = await initWasmForContext({
      variant: init.wasmVariant,
      module: init.wasmModule,
      memory: guestMemory,
    });
    const value = typeof api.add === "function" ? api.add(20, 22) : 0;
    const st = status;
    if (st && Atomics.load(st, StatusIndex.StopRequested) === 1) return;
    post({ type: MessageType.WASM_READY, role, variant, value } satisfies ProtocolMessage);
  } catch (err) {
    // Best-effort; do not crash on missing wasm assets.
    // Use a guarded log to avoid throwing in environments without console.
    try {
      // eslint-disable-next-line no-console
      console.warn("[machine_cpu.worker] WASM init failed (continuing without WASM):", err);
    } catch {
      // ignore
    }
  }
}
