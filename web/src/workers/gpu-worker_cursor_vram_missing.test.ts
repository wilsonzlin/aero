import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { VRAM_BASE_PADDR } from "../arch/guest_phys.ts";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { CURSOR_FORMAT_B8G8R8A8, publishCursorState, wrapCursorState } from "../ipc/cursor_state.ts";
import { aerogpuFormatToString } from "../../../emulator/protocol/aerogpu/aerogpu_pci.ts";

async function waitForWorkerMessage(
  worker: Worker,
  predicate: (msg: unknown) => boolean,
  timeoutMs: number,
): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out after ${timeoutMs}ms waiting for worker message`));
    }, timeoutMs);
    (timer as unknown as { unref?: () => void }).unref?.();

    const onMessage = (msg: unknown) => {
      // Surface runtime worker errors eagerly.
      const maybeProtocol = msg as Partial<ProtocolMessage> | undefined;
      if (maybeProtocol?.type === MessageType.ERROR) {
        cleanup();
        const rawMsg = (maybeProtocol as { message?: unknown }).message;
        const errMsg = typeof rawMsg === "string" ? rawMsg : "";
        reject(new Error(`worker reported error${errMsg ? `: ${errMsg}` : ""}`));
        return;
      }
      try {
        if (!predicate(msg)) return;
      } catch (err) {
        cleanup();
        reject(err instanceof Error ? err : new Error(String(err)));
        return;
      }
      cleanup();
      resolve(msg);
    };

    const onError = (err: unknown) => {
      cleanup();
      reject(err instanceof Error ? err : new Error(String(err)));
    };

    const onExit = (code: number) => {
      cleanup();
      reject(new Error(`worker exited before emitting the expected message (code=${code})`));
    };

    function cleanup(): void {
      clearTimeout(timer);
      worker.off("message", onMessage);
      worker.off("error", onError);
      worker.off("exit", onExit);
    }

    worker.on("message", onMessage);
    worker.on("error", onError);
    worker.on("exit", onExit);
  });
}

describe("workers/gpu-worker cursor VRAM missing diagnostics", () => {
  it("emits a structured CursorReadback event when the hardware cursor points into VRAM but no VRAM SAB is attached", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    const vramBasePaddr = VRAM_BASE_PADDR >>> 0;
    const vramSizeBytes = 0x2000;
    const cursorBasePaddr = (vramBasePaddr + 0x1000) >>> 0;
    const expectedSnippet = "Cursor: base_paddr points into VRAM but VRAM is unavailable";

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
        // Intentionally omit `vram` but still advertise a VRAM aperture.
        vramBasePaddr,
        vramSizeBytes,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        20_000,
      );

      // Publish a cursor descriptor pointing into VRAM. We don't care about the backing bytes; the
      // diagnostic should fire before any successful readback.
      const cursorWords = wrapCursorState(segments.cursorState!, segments.cursorStateOffsetBytes ?? 0);
      publishCursorState(cursorWords, {
        enable: 1,
        x: 0,
        y: 0,
        hotX: 0,
        hotY: 0,
        width: 1,
        height: 1,
        pitchBytes: 4,
        format: CURSOR_FORMAT_B8G8R8A8,
        basePaddrLo: cursorBasePaddr,
        basePaddrHi: 0,
      });

      const eventsPromise = waitForWorkerMessage(
        worker,
        (msg) => {
           const m = msg as { protocol?: unknown; type?: unknown; events?: unknown[] } | undefined;
           if (m?.protocol !== GPU_PROTOCOL_NAME || m.type !== "events") return false;
           const events = Array.isArray(m.events) ? m.events : [];
           return events.some(
            (ev) =>
              (ev as { category?: unknown; message?: unknown } | null | undefined)?.category === "CursorReadback" &&
              String((ev as { message?: unknown }).message).includes(expectedSnippet),
           );
         },
          20_000,
        );

      // Drive a tick so the worker polls CursorState and attempts a cursor readback.
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const eventsMsgRaw = await eventsPromise;

      const eventsMsg = eventsMsgRaw as { events?: unknown[] };
      const ev = (eventsMsg.events ?? []).find(
        (e) => (e as { category?: unknown } | null | undefined)?.category === "CursorReadback",
      ) as { severity?: unknown; message?: unknown; details?: unknown } | undefined;
      expect(ev).toBeTruthy();
      if (!ev) throw new Error("expected CursorReadback event");
      expect(ev.severity).toBe("warn");
       expect(String(ev.message)).toContain(expectedSnippet);
       expect(ev.details).toMatchObject({
         vram_base_paddr: `0x${vramBasePaddr.toString(16)}`,
         vram_size_bytes: vramSizeBytes,
         cursor: {
           format: CURSOR_FORMAT_B8G8R8A8,
           format_str: aerogpuFormatToString(CURSOR_FORMAT_B8G8R8A8),
         },
       });
      } finally {
        await worker.terminate();
      }
    }, 60_000);
});
