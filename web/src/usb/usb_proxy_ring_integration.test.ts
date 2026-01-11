import { afterEach, describe, expect, it, vi } from "vitest";

import { UsbBroker } from "./usb_broker";
import { WebUsbPassthroughRuntime, type UsbPassthroughBridgeLike } from "./webusb_passthrough_runtime";
import type { UsbHostAction, UsbHostCompletion } from "./usb_proxy_protocol";

type Listener = (ev: MessageEvent<unknown>) => void;

class FakePort {
  peer: FakePort | null = null;
  deliverToPeer = true;
  readonly sent: Array<{ msg: unknown; transfer?: Transferable[] }> = [];
  private readonly listeners = new Set<Listener>();

  addEventListener(type: string, listener: Listener): void {
    if (type !== "message") return;
    this.listeners.add(listener);
  }

  removeEventListener(type: string, listener: Listener): void {
    if (type !== "message") return;
    this.listeners.delete(listener);
  }

  start(): void {
    // No-op; browsers require MessagePort.start() when using addEventListener.
  }

  postMessage(msg: unknown, transfer?: Transferable[]): void {
    this.sent.push({ msg, transfer });
    if (!this.deliverToPeer) return;
    const peer = this.peer;
    if (!peer) return;
    const ev = { data: msg } as MessageEvent<unknown>;
    for (const listener of peer.listeners) listener(ev);
  }
}

function createChannel(): { brokerPort: FakePort; workerPort: FakePort } {
  const brokerPort = new FakePort();
  const workerPort = new FakePort();
  brokerPort.peer = workerPort;
  workerPort.peer = brokerPort;
  return { brokerPort, workerPort };
}

async function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<T>((_resolve, reject) => {
    timer = setTimeout(() => reject(new Error(`Timed out after ${ms}ms`)), ms);
    (timer as unknown as { unref?: () => void }).unref?.();
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

const originalCrossOriginIsolatedDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");

afterEach(() => {
  if (originalCrossOriginIsolatedDescriptor) {
    Object.defineProperty(globalThis, "crossOriginIsolated", originalCrossOriginIsolatedDescriptor);
  } else {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    delete (globalThis as any).crossOriginIsolated;
  }
  vi.clearAllMocks();
  vi.resetModules();
});

describe("usb/WebUSB proxy SAB ring integration", () => {
  it("sends usb.ringAttach when SharedArrayBuffer is available and crossOriginIsolated", () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
    const { brokerPort } = createChannel();
    broker.attachWorkerPort(brokerPort as unknown as MessagePort);

    const ringAttach = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach")?.msg as
      | { actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    expect(ringAttach!.actionRing).toBeInstanceOf(SharedArrayBuffer);
    expect(ringAttach!.completionRing).toBeInstanceOf(SharedArrayBuffer);
  });

  it("flows actions/completions end-to-end via rings (no per-action postMessage required)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
    const { brokerPort, workerPort } = createChannel();

    // Drop worker -> broker postMessage deliveries to ensure the ring path is used.
    workerPort.deliverToPeer = false;

    const actions: UsbHostAction[] = [{ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 }];
    const push_completion = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions),
      push_completion,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: workerPort as unknown as MessagePort, pollIntervalMs: 0 });

    broker.attachWorkerPort(brokerPort as unknown as MessagePort);

    await withTimeout(runtime.pollOnce(), 250);

    // The runtime should not have sent usb.action envelopes (broker would never receive them).
    const postedActions = workerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.action");
    expect(postedActions).toHaveLength(0);

    // The broker should not have sent usb.completion envelopes (completion delivered via the ring).
    const postedCompletions = brokerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.completion");
    expect(postedCompletions).toHaveLength(0);

    expect(push_completion).toHaveBeenCalledTimes(1);
    const completion = push_completion.mock.calls[0]?.[0] as UsbHostCompletion;
    expect(completion).toMatchObject({ kind: "bulkIn", id: 1 });

    runtime.destroy();
    broker.detachWorkerPort(brokerPort as unknown as MessagePort);
  });

  it("falls back to usb.action postMessage when the action ring is full", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    // action ring is tiny: only one 16-byte bulkIn record fits.
    const broker = new UsbBroker({ ringActionCapacityBytes: 20, ringCompletionCapacityBytes: 1024, ringDrainIntervalMs: 1 });
    const { brokerPort, workerPort } = createChannel();

    const actions: UsbHostAction[] = [
      { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 },
      { kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 },
    ];
    const push_completion = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions),
      push_completion,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: workerPort as unknown as MessagePort, pollIntervalMs: 0 });
    broker.attachWorkerPort(brokerPort as unknown as MessagePort);

    await withTimeout(runtime.pollOnce(), 250);

    const postedActions = workerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.action");
    expect(postedActions).toHaveLength(1);
    expect((postedActions[0]!.msg as any).action.id).toBe(2);

    runtime.destroy();
    broker.detachWorkerPort(brokerPort as unknown as MessagePort);
  });

  it("falls back to usb.completion postMessage when the completion ring is full", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    // completion ring is tiny: only one error completion (~40 bytes) fits.
    const broker = new UsbBroker({ ringActionCapacityBytes: 1024, ringCompletionCapacityBytes: 64, ringDrainIntervalMs: 1 });
    const { brokerPort, workerPort } = createChannel();

    const actions: UsbHostAction[] = [
      { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 },
      { kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 },
    ];
    const push_completion = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions),
      push_completion,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: workerPort as unknown as MessagePort, pollIntervalMs: 0 });
    broker.attachWorkerPort(brokerPort as unknown as MessagePort);

    await withTimeout(runtime.pollOnce(), 250);

    const postedActions = workerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.action");
    expect(postedActions).toHaveLength(0);

    const postedCompletions = brokerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.completion");
    expect(postedCompletions).toHaveLength(1);

    runtime.destroy();
    broker.detachWorkerPort(brokerPort as unknown as MessagePort);
  });
});
