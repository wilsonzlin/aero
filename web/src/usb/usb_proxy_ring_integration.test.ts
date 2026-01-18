import { afterEach, describe, expect, it, vi } from "vitest";

import { UsbBroker } from "./usb_broker";
import { WebUsbPassthroughRuntime, type UsbPassthroughBridgeLike } from "./webusb_passthrough_runtime";
import type { UsbHostAction, UsbHostCompletion, UsbRingAttachMessage } from "./usb_proxy_protocol";
import { createUsbProxyRingBuffer, USB_PROXY_RING_CTRL_BYTES, UsbProxyRing } from "./usb_proxy_ring";
import { unrefBestEffort } from "../unrefSafe";

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

class CloningFakePort {
  peer: CloningFakePort | null = null;
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
    const cloned = structuredClone(msg);
    const ev = { data: cloned } as MessageEvent<unknown>;
    for (const listener of peer.listeners) listener(ev);
  }
}

function createCloningChannel(): { brokerPort: CloningFakePort; workerPort: CloningFakePort } {
  const brokerPort = new CloningFakePort();
  const workerPort = new CloningFakePort();
  brokerPort.peer = workerPort;
  workerPort.peer = brokerPort;
  return { brokerPort, workerPort };
}

async function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<T>((_resolve, reject) => {
    timer = setTimeout(() => reject(new Error(`Timed out after ${ms}ms`)), ms);
    unrefBestEffort(timer);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function flushMicrotasks(iterations = 8): Promise<void> {
  // `await Promise.resolve()` yields to the microtask queue. Loop a few times so nested async/await
  // chains (like the broker's execute queue) have a chance to fully drain.
  for (let i = 0; i < iterations; i += 1) {
    await Promise.resolve();
  }
}

const originalCrossOriginIsolatedDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");

afterEach(() => {
  if (originalCrossOriginIsolatedDescriptor) {
    Object.defineProperty(globalThis, "crossOriginIsolated", originalCrossOriginIsolatedDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "crossOriginIsolated");
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

  it("does not send usb.ringAttach when attaching a port with attachRings=false", () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
    const { brokerPort } = createChannel();
    broker.attachWorkerPort(brokerPort as unknown as MessagePort, { attachRings: false });

    const ringAttach = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach");
    expect(ringAttach).toBeUndefined();
  });

  it("honors usb.ringAttachRequest even when attachRings=false", () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
    const { brokerPort, workerPort } = createChannel();
    broker.attachWorkerPort(brokerPort as unknown as MessagePort, { attachRings: false });
    brokerPort.sent.length = 0;

    workerPort.postMessage({ type: "usb.ringAttachRequest" });

    const ringAttach = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach")?.msg as
      | { actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    expect(ringAttach!.actionRing).toBeInstanceOf(SharedArrayBuffer);
    expect(ringAttach!.completionRing).toBeInstanceOf(SharedArrayBuffer);
  });

  it("re-sends usb.ringAttach on usb.ringAttachRequest (reusing the same ring handles)", () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
    const { brokerPort, workerPort } = createChannel();
    broker.attachWorkerPort(brokerPort as unknown as MessagePort);

    const first = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach")?.msg as
      | { actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
      | undefined;
    expect(first).toBeTruthy();

    brokerPort.sent.length = 0;
    workerPort.postMessage({ type: "usb.ringAttachRequest" });

    const second = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach")?.msg as
      | { actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
      | undefined;
    expect(second).toBeTruthy();
    expect(second!.actionRing).toBe(first!.actionRing);
    expect(second!.completionRing).toBe(first!.completionRing);
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

  it("requests usb.ringDetach when completion ring decoding fails and falls back to postMessage", async () => {
    const { brokerPort, workerPort } = createChannel();

    const actions: UsbHostAction[] = [{ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 }];
    const push_completion = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions),
      push_completion,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: workerPort as unknown as MessagePort, pollIntervalMs: 0 });
    workerPort.sent.length = 0;

    // Prepare rings with a corrupted completion record to force popCompletion() to throw.
    const actionRing = createUsbProxyRingBuffer(256);
    const completionRing = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(completionRing);
    expect(ring.pushCompletion({ kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) })).toBe(true);
    new Uint8Array(completionRing, USB_PROXY_RING_CTRL_BYTES)[0] = 0x99;

    brokerPort.postMessage({ type: "usb.ringAttach", actionRing, completionRing });

    await Promise.resolve();

    const detachMsgs = workerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.ringDetach");
    expect(detachMsgs).toHaveLength(1);

    brokerPort.addEventListener("message", (ev: MessageEvent<unknown>) => {
      const data = ev.data as { type?: unknown; action?: UsbHostAction };
      if (data.type !== "usb.action" || !data.action) return;
      brokerPort.postMessage({ type: "usb.completion", completion: { kind: data.action.kind, id: data.action.id, status: "stall" } });
    });

    workerPort.sent.length = 0;
    await withTimeout(runtime.pollOnce(), 250);

    const postedActions = workerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.action");
    expect(postedActions).toHaveLength(1);
    expect(push_completion).toHaveBeenCalledTimes(1);

    runtime.destroy();
  });

  it("disables rings and sends usb.ringDetach when action ring decoding fails", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
      const { brokerPort } = createChannel();
      broker.attachWorkerPort(brokerPort as unknown as MessagePort);

      const ringAttach = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach")?.msg as
        | { actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();

      const ring = new UsbProxyRing(ringAttach!.actionRing);
      expect(ring.pushAction({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 })).toBe(true);

      // Corrupt the kind tag so popAction() throws.
      new Uint8Array(ringAttach!.actionRing, USB_PROXY_RING_CTRL_BYTES)[0] = 0x99;

      brokerPort.sent.length = 0;
      await vi.advanceTimersByTimeAsync(1);

      const detach = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringDetach")?.msg as
        | { type: "usb.ringDetach"; reason?: string }
        | undefined;
      expect(detach).toBeTruthy();
      expect(detach!.reason).toMatch(/disabled/i);

      broker.detachWorkerPort(brokerPort as unknown as MessagePort);
    } finally {
      vi.useRealTimers();
    }
  });

  it("broadcasts completion ring data to multiple runtimes on the same port", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
    const { brokerPort, workerPort } = createChannel();

    const actions1: UsbHostAction[] = [{ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 }];
    const push_completion_1 = vi.fn();
    const bridge1: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions1),
      push_completion: push_completion_1,
      reset: vi.fn(),
      free: vi.fn(),
    };
    const runtime1 = new WebUsbPassthroughRuntime({ bridge: bridge1, port: workerPort as unknown as MessagePort, pollIntervalMs: 0 });

    const actions2: UsbHostAction[] = [{ kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 }];
    const push_completion_2 = vi.fn();
    const bridge2: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions2),
      push_completion: push_completion_2,
      reset: vi.fn(),
      free: vi.fn(),
    };
    const runtime2 = new WebUsbPassthroughRuntime({ bridge: bridge2, port: workerPort as unknown as MessagePort, pollIntervalMs: 0 });

    broker.attachWorkerPort(brokerPort as unknown as MessagePort);

    // Drop worker -> broker postMessage deliveries to ensure the ring path is used.
    workerPort.deliverToPeer = false;

    await withTimeout(Promise.all([runtime1.pollOnce(), runtime2.pollOnce()]), 250);

    // Neither runtime should have posted usb.action envelopes (broker would never receive them).
    const postedActions = workerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.action");
    expect(postedActions).toHaveLength(0);

    // Broker should not have posted usb.completion envelopes (completions delivered via ring).
    const postedCompletions = brokerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.completion");
    expect(postedCompletions).toHaveLength(0);

    expect(push_completion_1).toHaveBeenCalledTimes(1);
    expect(push_completion_2).toHaveBeenCalledTimes(1);
    expect(push_completion_1.mock.calls[0]?.[0]).toMatchObject({ kind: "bulkIn", id: 1 });
    expect(push_completion_2.mock.calls[0]?.[0]).toMatchObject({ kind: "bulkIn", id: 2 });

    runtime1.destroy();
    runtime2.destroy();
    broker.detachWorkerPort(brokerPort as unknown as MessagePort);
  });

  it("re-attaches completion ring subscriptions when usb.ringAttach is resent (SAB wrapper clones)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
    const { brokerPort, workerPort } = createCloningChannel();

    let lastRingAttach: unknown = null;
    const cacheListener: Listener = (ev) => {
      const data = ev.data as { type?: unknown };
      if (data && data.type === "usb.ringAttach") {
        lastRingAttach = ev.data;
      }
    };
    workerPort.addEventListener("message", cacheListener);

    broker.attachWorkerPort(brokerPort as unknown as MessagePort);
    expect(lastRingAttach).toBeTruthy();

    const ringAttach1 = lastRingAttach as UsbRingAttachMessage;

    // Prevent the runtime's constructor from triggering a ring resend: we want it to attach to
    // the first ringAttach payload and only later receive a re-sent attach (with cloned SAB wrappers).
    workerPort.deliverToPeer = false;

    const actions1: UsbHostAction[] = [{ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 }];
    const push_completion_1 = vi.fn();
    const bridge1: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions1),
      push_completion: push_completion_1,
      reset: vi.fn(),
      free: vi.fn(),
    };
    const runtime1 = new WebUsbPassthroughRuntime({
      bridge: bridge1,
      port: workerPort as unknown as MessagePort,
      pollIntervalMs: 0,
      initialRingAttach: ringAttach1,
    });

    // Trigger a ring resend. `structuredClone` will create new SharedArrayBuffer wrapper objects.
    workerPort.deliverToPeer = true;
    workerPort.postMessage({ type: "usb.ringAttachRequest" });
    expect(lastRingAttach).toBeTruthy();
    const ringAttach2 = lastRingAttach as UsbRingAttachMessage;
    expect(ringAttach2).not.toBe(ringAttach1);

    // Disable postMessage deliveries again so subsequent usb.action messages can't reach the broker.
    workerPort.deliverToPeer = false;

    const actions2: UsbHostAction[] = [{ kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 }];
    const push_completion_2 = vi.fn();
    const bridge2: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions2),
      push_completion: push_completion_2,
      reset: vi.fn(),
      free: vi.fn(),
    };
    const runtime2 = new WebUsbPassthroughRuntime({
      bridge: bridge2,
      port: workerPort as unknown as MessagePort,
      pollIntervalMs: 0,
      initialRingAttach: ringAttach2,
    });

    await withTimeout(Promise.all([runtime1.pollOnce(), runtime2.pollOnce()]), 250);

    expect(push_completion_1).toHaveBeenCalledTimes(1);
    expect(push_completion_2).toHaveBeenCalledTimes(1);

    runtime1.destroy();
    runtime2.destroy();
    broker.detachWorkerPort(brokerPort as unknown as MessagePort);
  });

  it("allows late-starting runtimes to request usb.ringAttach (so they can still use rings)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const broker = new UsbBroker({ ringDrainIntervalMs: 1 });
    const { brokerPort, workerPort } = createChannel();

    // Attach the worker port first: `usb.ringAttach` is emitted immediately, but there is no runtime
    // listener yet so it is effectively missed.
    broker.attachWorkerPort(brokerPort as unknown as MessagePort);
    brokerPort.sent.length = 0;

    const actions: UsbHostAction[] = [{ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 }];
    const push_completion = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions),
      push_completion,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: workerPort as unknown as MessagePort, pollIntervalMs: 0 });

    // Runtime constructor should have requested rings, triggering a re-send of usb.ringAttach.
    expect(brokerPort.sent.some((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach")).toBe(true);

    // Drop worker -> broker postMessage deliveries to ensure the ring path is used.
    workerPort.deliverToPeer = false;

    await withTimeout(runtime.pollOnce(), 250);

    const postedActions = workerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.action");
    expect(postedActions).toHaveLength(0);

    expect(push_completion).toHaveBeenCalledTimes(1);

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
    const actionId = (postedActions[0]?.msg as { action?: { id?: unknown } } | undefined)?.action?.id;
    expect(actionId).toBe(2);

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

  it("caps action ring drains per tick (record count)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const broker = new UsbBroker({
        ringDrainIntervalMs: 1,
        ringActionCapacityBytes: 8 * 1024,
        // Force completions to fall back to postMessage so we can count them.
        ringCompletionCapacityBytes: 4,
      });
      const { brokerPort } = createChannel();
      broker.attachWorkerPort(brokerPort as unknown as MessagePort);

      const ringAttach = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach")?.msg as
        | { actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();

      const ring = new UsbProxyRing(ringAttach!.actionRing);
      for (let i = 0; i < 300; i += 1) {
        expect(ring.pushAction({ kind: "bulkIn", id: i + 1, endpoint: 0x81, length: 8 })).toBe(true);
      }

      brokerPort.sent.length = 0;
      await vi.advanceTimersByTimeAsync(1);
      await flushMicrotasks();

      const completionsTick1 = brokerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.completion");
      expect(completionsTick1).toHaveLength(256);

      brokerPort.sent.length = 0;
      await vi.advanceTimersByTimeAsync(1);
      await flushMicrotasks();

      const completionsTick2 = brokerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.completion");
      expect(completionsTick2).toHaveLength(44);

      broker.detachWorkerPort(brokerPort as unknown as MessagePort);
    } finally {
      vi.useRealTimers();
    }
  });

  it("caps action ring drains per tick (payload bytes)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const broker = new UsbBroker({
        ringDrainIntervalMs: 1,
        // Large enough to store a couple of 1MiB bulkOut actions.
        ringActionCapacityBytes: 3 * 1024 * 1024,
        ringCompletionCapacityBytes: 4,
      });
      const { brokerPort } = createChannel();
      broker.attachWorkerPort(brokerPort as unknown as MessagePort);

      const ringAttach = brokerPort.sent.find((p) => (p.msg as { type?: unknown }).type === "usb.ringAttach")?.msg as
        | { actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();

      const ring = new UsbProxyRing(ringAttach!.actionRing);
      const payload = new Uint8Array(1024 * 1024);
      expect(ring.pushAction({ kind: "bulkOut", id: 1, endpoint: 1, data: payload })).toBe(true);
      expect(ring.pushAction({ kind: "bulkOut", id: 2, endpoint: 1, data: payload })).toBe(true);

      brokerPort.sent.length = 0;
      await vi.advanceTimersByTimeAsync(1);
      await flushMicrotasks();

      const completionsTick1 = brokerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.completion");
      expect(completionsTick1).toHaveLength(1);

      brokerPort.sent.length = 0;
      await vi.advanceTimersByTimeAsync(1);
      await flushMicrotasks();

      const completionsTick2 = brokerPort.sent.filter((p) => (p.msg as { type?: unknown }).type === "usb.completion");
      expect(completionsTick2).toHaveLength(1);

      broker.detachWorkerPort(brokerPort as unknown as MessagePort);
    } finally {
      vi.useRealTimers();
    }
  });
});
