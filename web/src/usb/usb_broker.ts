import {
  getTransferablesForUsbCompletionMessage,
  isUsbActionMessage,
  isUsbGuestWebUsbStatusMessage,
  isUsbQuerySelectedMessage,
  isUsbRingAttachRequestMessage,
  isUsbRingDetachMessage,
  isUsbSelectDeviceMessage,
  usbErrorCompletion,
  type UsbActionMessage,
  type UsbCompletionMessage,
  type UsbGuestControllerMode,
  type UsbGuestControllerModeMessage,
  type UsbGuestWebUsbSnapshot,
  type UsbGuestWebUsbStatusMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbProxyActionOptions,
  type UsbRingAttachMessage,
  type UsbRingDetachMessage,
  type UsbSelectDeviceMessage,
  type UsbSelectedMessage,
} from "./usb_proxy_protocol";
import { WebUsbBackend, type WebUsbBackendOptions } from "./webusb_backend";
import { formatWebUsbError } from "../platform/webusb_troubleshooting";
import { createUsbProxyRingBuffer, UsbProxyRing } from "./usb_proxy_ring";
import { formatOneLineError } from "../text";

type UsbDeviceInfo = { vendorId: number; productId: number; productName?: string };

// Keep the per-interval ring drains bounded so a busy or malicious worker can't
// keep the main thread spinning (starving UI/rendering). Rings are an optional
// fast-path: when we hit these caps we continue draining on the next interval.
const MAX_USB_ACTION_RING_RECORDS_PER_DRAIN_TICK = 256;
// Approximate byte budget for per-tick action-ring drains. This is primarily
// meant to avoid draining many large bulkOut/controlOut payloads in a single
// tick, which would allocate large temporary buffers on the main thread.
const MAX_USB_ACTION_RING_BYTES_PER_DRAIN_TICK = 1024 * 1024;

// WebUSB transfers are serialized via the broker queue to match Chromium's
// WebUSB constraints. Keep this bounded so a stalled device/transfer cannot
// cause unbounded memory growth if the guest spams actions.
const DEFAULT_MAX_PENDING_USB_ACTIONS = 1024;
const DEFAULT_MAX_PENDING_USB_ACTION_BYTES = 32 * 1024 * 1024;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function extractActionId(action: unknown): number | null {
  if (!isRecord(action)) return null;
  const id = action.id;
  if (typeof id !== "number") return null;
  // Match `isUsbHostAction` (`usb_proxy_protocol.ts`): ids are u32 values encoded as JS numbers.
  if (!Number.isSafeInteger(id)) return null;
  if (id < 0 || id > 0xffff_ffff) return null;
  return id;
}

function extractActionKind(action: unknown): UsbHostAction["kind"] | null {
  if (!isRecord(action)) return null;
  const kind = action.kind;
  if (kind === "controlIn" || kind === "controlOut" || kind === "bulkIn" || kind === "bulkOut") return kind;
  return null;
}

function normalizeUsbProxyActionOptions(options: unknown): UsbProxyActionOptions | undefined {
  if (!isRecord(options)) return undefined;
  const raw = options.translateOtherSpeedConfigurationDescriptor;
  if (raw === undefined) return undefined;
  if (typeof raw !== "boolean") return undefined;
  return { translateOtherSpeedConfigurationDescriptor: raw };
}

type QueueItem = {
  action: UsbHostAction;
  options?: UsbProxyActionOptions;
  resolve: (completion: UsbHostCompletion) => void;
  port: MessagePort | Worker | null;
  payloadBytes: number;
};

type UsbForgettableDevice = USBDevice & { forget: () => Promise<void> };

function canForgetUsbDevice(device: USBDevice): device is UsbForgettableDevice {
  // `USBDevice.forget()` is currently Chromium-specific. Keep this check tolerant
  // so the broker continues to work on browsers without the API.
  return typeof (device as unknown as { forget?: unknown }).forget === "function";
}

function createDeferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function assertPositiveSafeInteger(name: string, value: number): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`invalid ${name}: ${value}`);
  }
  return value;
}

function actionPayloadBytes(action: UsbHostAction): number {
  switch (action.kind) {
    case "controlOut":
    case "bulkOut":
      return action.data.byteLength >>> 0;
    default:
      return 0;
  }
}

function wrapWithCause(message: string, cause: unknown): Error {
  const error = new Error(message);
  // Not all runtimes support the `ErrorOptions` constructor parameter, but
  // attaching `cause` is still useful for debugging and for our WebUSB
  // troubleshooting helper, which can walk `Error.cause` chains.
  try {
    (error as Error & { cause?: unknown }).cause = cause;
  } catch {
    // ignore
  }
  return error;
}

function getNavigatorUsb(): USB | null {
  // Keep the access tolerant so unit tests (node environment) can stub navigator.
  const nav = (globalThis as unknown as { navigator?: unknown }).navigator as (Navigator & { usb?: USB }) | undefined;
  return nav?.usb ?? null;
}

function resolveWebUsbBackendOptions(options?: WebUsbBackendOptions): Required<WebUsbBackendOptions> {
  return { translateOtherSpeedConfigurationDescriptor: options?.translateOtherSpeedConfigurationDescriptor ?? true };
}

type UsbBrokerAttachPortMessage = {
  type: "usb.broker.attachPort";
  port: MessagePort;
  attachRings?: boolean;
  backendOptions?: WebUsbBackendOptions;
};

function isMessagePortLike(value: unknown): value is MessagePort {
  if (!value || typeof value !== "object") return false;
  const v = value as { postMessage?: unknown; addEventListener?: unknown; removeEventListener?: unknown };
  return typeof v.postMessage === "function" && typeof v.addEventListener === "function" && typeof v.removeEventListener === "function";
}

function isUsbBrokerAttachPortMessage(value: unknown): value is UsbBrokerAttachPortMessage {
  if (!isRecord(value) || value.type !== "usb.broker.attachPort") return false;
  if (!isMessagePortLike(value.port)) return false;
  if (value.attachRings !== undefined && typeof value.attachRings !== "boolean") return false;
  if (value.backendOptions !== undefined) {
    if (!isRecord(value.backendOptions)) return false;
    const translate = value.backendOptions.translateOtherSpeedConfigurationDescriptor;
    if (translate !== undefined && typeof translate !== "boolean") return false;
  }
  return true;
}

export class UsbBroker {
  private device: USBDevice | null = null;
  private backendDefault: WebUsbBackend | null = null;
  private backendNoOtherSpeed: WebUsbBackend | null = null;
  private selectedInfo: UsbDeviceInfo | null = null;
  private guestStatus: UsbGuestWebUsbSnapshot | null = null;
  private guestControllerMode: UsbGuestControllerMode = "uhci";

  private disconnectError: string | null = null;
  private disconnectSignal = createDeferred<string>();

  private readonly ports = new Set<MessagePort | Worker>();
  private readonly portListeners = new Map<MessagePort | Worker, EventListener>();
  private readonly portBackendOptions = new Map<MessagePort | Worker, Required<WebUsbBackendOptions>>();
  // Ports can request attaching additional MessagePorts via the `usb.broker.attachPort` message.
  // These "child" ports are typically created inside workers (e.g. dedicated WebUSB runtimes that
  // need different backend options) and would otherwise be leaked if the parent worker is detached.
  private readonly portParents = new Map<MessagePort | Worker, MessagePort | Worker>();
  private readonly portChildren = new Map<MessagePort | Worker, Set<MessagePort | Worker>>();
  private readonly deviceChangeListeners = new Set<() => void>();

  private readonly ringDrainTimers = new Map<MessagePort | Worker, ReturnType<typeof setInterval>>();
  private readonly actionRings = new Map<MessagePort | Worker, UsbProxyRing>();
  private readonly completionRings = new Map<MessagePort | Worker, UsbProxyRing>();

  private readonly ringActionCapacityBytes: number;
  private readonly ringCompletionCapacityBytes: number;
  private readonly ringDrainIntervalMs: number;

  private readonly maxPendingActions: number;
  private readonly maxPendingActionBytes: number;
  private pendingActionBytes = 0;
  private inFlightCount = 0;

  private readonly queue: QueueItem[] = [];
  private processing = false;

  constructor(
    options: {
      ringActionCapacityBytes?: number;
      ringCompletionCapacityBytes?: number;
      ringDrainIntervalMs?: number;
      maxPendingActions?: number;
      maxPendingActionBytes?: number;
    } = {},
  ) {
    this.ringActionCapacityBytes = options.ringActionCapacityBytes ?? 256 * 1024;
    this.ringCompletionCapacityBytes = options.ringCompletionCapacityBytes ?? 256 * 1024;
    this.ringDrainIntervalMs = options.ringDrainIntervalMs ?? 8;
    this.maxPendingActions =
      options.maxPendingActions === undefined
        ? DEFAULT_MAX_PENDING_USB_ACTIONS
        : assertPositiveSafeInteger("maxPendingActions", options.maxPendingActions);
    this.maxPendingActionBytes =
      options.maxPendingActionBytes === undefined
        ? DEFAULT_MAX_PENDING_USB_ACTION_BYTES
        : assertPositiveSafeInteger("maxPendingActionBytes", options.maxPendingActionBytes);

    const usb = getNavigatorUsb();
    usb?.addEventListener?.("disconnect", (ev: Event) => {
      const device = (ev as unknown as { device?: USBDevice }).device;
      if (device && this.device && device === this.device) {
        this.handleDisconnect("WebUSB device disconnected.");
      }
      this.emitDeviceChange();
    });

    usb?.addEventListener?.("connect", () => {
      this.emitDeviceChange();
    });
  }

  subscribeToDeviceChanges(listener: () => void): () => void {
    this.deviceChangeListeners.add(listener);
    return () => {
      this.deviceChangeListeners.delete(listener);
    };
  }

  async getKnownDevices(): Promise<USBDevice[]> {
    const usb = getNavigatorUsb();
    if (!usb || typeof usb.getDevices !== "function") {
      throw new Error("WebUSB is unavailable (navigator.usb.getDevices missing).");
    }

    try {
      return await usb.getDevices();
    } catch (err) {
      const wrapped = new Error("navigator.usb.getDevices() failed.");
      try {
        (wrapped as Error & { cause?: unknown }).cause = err;
      } catch {
        // ignore
      }
      throw wrapped;
    }
  }

  async attachKnownDevice(device: USBDevice): Promise<UsbDeviceInfo> {
    const backend = new WebUsbBackend(device);

    try {
      await backend.ensureOpenAndClaimed();
    } catch (err) {
      try {
        await device.close();
      } catch {
        // Ignore close errors; failing to open/claim is the root issue.
      }
      throw err;
    }

    // Replace any previously-selected device and fail outstanding actions so they don't accidentally run on the new device.
    this.resetSelectedDevice("WebUSB device replaced.");

    this.device = device;
    this.backendDefault = backend;
    this.backendNoOtherSpeed = null;
    this.selectedInfo = {
      vendorId: device.vendorId,
      productId: device.productId,
      productName: device.productName ?? undefined,
    };
    this.disconnectError = null;
    this.disconnectSignal = createDeferred();

    const msg: UsbSelectedMessage = { type: "usb.selected", ok: true, info: this.selectedInfo };
    this.broadcast(msg);
    return this.selectedInfo;
  }

  getGuestControllerMode(): UsbGuestControllerMode {
    return this.guestControllerMode;
  }

  setGuestControllerMode(mode: UsbGuestControllerMode): void {
    if (mode === this.guestControllerMode) return;
    this.guestControllerMode = mode;
    this.broadcastGuestControllerMode({ type: "usb.guest.controller", mode } satisfies UsbGuestControllerModeMessage);
  }

  async detachSelectedDevice(reason = "WebUSB device detached."): Promise<void> {
    if (!this.device && !this.backendDefault && !this.selectedInfo) return;
    this.resetSelectedDevice(reason);
    this.broadcast({ type: "usb.selected", ok: false, error: reason } satisfies UsbSelectedMessage);
  }

  async getPermittedDevices(): Promise<USBDevice[]> {
    const usb = getNavigatorUsb();
    if (!usb || typeof usb.getDevices !== "function") {
      throw new Error("WebUSB is unavailable (navigator.usb.getDevices missing).");
    }

    try {
      return await usb.getDevices();
    } catch (err) {
      throw wrapWithCause("Failed to list permitted WebUSB devices.", err);
    }
  }

  async attachPermittedDevice(device: USBDevice): Promise<UsbDeviceInfo> {
    if (!device) {
      throw new Error("WebUSB device not provided.");
    }
    const usb = getNavigatorUsb();
    if (!usb) {
      throw new Error("WebUSB is unavailable (navigator.usb missing).");
    }
    return await this.adoptDevice(device);
  }

  async requestDevice(filters?: USBDeviceFilter[]): Promise<UsbDeviceInfo> {
    const usb = getNavigatorUsb();
    if (!usb || typeof usb.requestDevice !== "function") {
      throw new Error("WebUSB is unavailable (navigator.usb.requestDevice missing).");
    }

    // `requestDevice` must be called from a user gesture; callers should invoke this from a click handler.
    const attempts: Array<USBDeviceRequestOptions> = [];
    if (filters && filters.length > 0) {
      attempts.push({ filters });
    } else {
      // Chromium versions differ on whether `filters: []` or `filters: [{}]` are accepted. Try a handful of
      // "broad" options so local smoke tests work across more builds.
      attempts.push({ filters: [] });
      attempts.push({ filters: [{}] });
      attempts.push({ filters: [{ classCode: 0x00 }, { classCode: 0xff }] });
      attempts.push({ filters: [{ classCode: 0xff }] });
    }

    let device: USBDevice | null = null;
    let lastErr: unknown = null;
    for (const opts of attempts) {
      try {
        device = await usb.requestDevice(opts);
        break;
      } catch (err) {
        lastErr = err;

        // User cancelled the chooser (or no matching devices).
        if (err instanceof DOMException && err.name === "NotFoundError") {
          throw err;
        }

        // If the browser rejected the filter shape, try the next fallback.
        if (err instanceof TypeError) continue;
        if (err instanceof DOMException && err.name === "TypeError") continue;

        throw err;
      }
    }

    if (!device) {
      throw lastErr ?? new Error("WebUSB requestDevice failed.");
    }
    return await this.adoptDevice(device);
  }

  canForgetSelectedDevice(): boolean {
    return this.device !== null && canForgetUsbDevice(this.device);
  }

  async forgetSelectedDevice(): Promise<void> {
    const device = this.device;
    if (!device) {
      throw new Error("WebUSB device not selected.");
    }
    if (!canForgetUsbDevice(device)) {
      throw new Error("USBDevice.forget() is unavailable in this browser.");
    }

    // Prevent any further actions from racing with forget().
    this.resetSelectedDevice("WebUSB device forgotten.");

    // WebHID/WebUSB forget flows are more reliable when the device is closed first.
    try {
      await device.close();
    } catch {
      // Best-effort; proceed to forget().
    }

    await device.forget();

    // Return to the initial "no device selected" state (no sticky disconnect error).
    this.disconnectError = null;
    this.disconnectSignal = createDeferred<string>();

    this.broadcast({ type: "usb.selected", ok: false });
    this.emitDeviceChange();
  }

  async execute(action: UsbHostAction, options?: UsbProxyActionOptions): Promise<UsbHostCompletion> {
    return await this.executeForPort(null, action, options);
  }

  /**
   * Attach a worker/MessagePort to the broker.
   *
   * The port will receive `usb.selected` and `usb.guest.status` broadcasts, and can send `usb.action`
   * requests for the broker to execute on the currently-selected WebUSB device.
   *
   * By default, the broker also allocates SharedArrayBuffer USB proxy rings and sends `usb.ringAttach`
   * to enable the high-throughput fast path when `crossOriginIsolated` is available. UI-only ports
   * (which only listen for status updates) should pass `{ attachRings: false }` to avoid allocating
   * ring buffers and per-port drain timers.
   */
  attachWorkerPort(port: MessagePort | Worker, options: { attachRings?: boolean; backendOptions?: WebUsbBackendOptions } = {}): void {
    const isNew = !this.ports.has(port);
    if (isNew) this.ports.add(port);

    // Record per-port backend behaviour even when the port was already attached so callers can
    // adjust options for re-used MessagePorts (e.g. worker-provided subports).
    this.portBackendOptions.set(port, resolveWebUsbBackendOptions(options.backendOptions));

    if (isNew) {
      const onMessage: EventListener = (ev) => {
        const data = (ev as MessageEvent<unknown>).data;
        if (isUsbBrokerAttachPortMessage(data)) {
          this.attachWorkerPort(data.port, {
            attachRings: data.attachRings,
            backendOptions: data.backendOptions,
          });
          this.linkChildPort(port, data.port);
          return;
        }
        if (isUsbActionMessage(data)) {
          const execOptions = normalizeUsbProxyActionOptions(data.options);
          void this.executeForPort(port, data.action, execOptions).then((completion) => {
            const msg: UsbCompletionMessage = { type: "usb.completion", completion };
            this.postToPort(port, msg);
          });
          return;
        }

        if (isUsbQuerySelectedMessage(data)) {
          // Reply with the current selection state so the worker can synchronize.
          // (This is distinct from `usb.selectDevice`, which triggers a chooser and
          // requires a user activation.)
          this.postToPort(port, this.currentSelectedMessage());
          return;
        }

        if (isUsbRingAttachRequestMessage(data)) {
          // Best-effort: late-starting runtimes may have missed the initial `usb.ringAttach`
          // message. Resend the ring handles so they can switch to the shared-memory fast path.
          this.attachRings(port);
          return;
        }

        if (isUsbRingDetachMessage(data)) {
          // Ring buffers are an optional fast path. When a worker detects corruption (or wants to
          // reduce overhead) it can request disabling the rings and falling back to postMessage.
          this.detachRingsForPort(port);
          this.postToPort(port, { type: "usb.ringDetach", reason: data.reason } satisfies UsbRingDetachMessage);
          return;
        }

        if (isUsbGuestWebUsbStatusMessage(data)) {
          this.guestStatus = data.snapshot;
          this.broadcastGuestStatus({ type: "usb.guest.status", snapshot: data.snapshot });
          return;
        }

        // If the message uses the `usb.action` envelope but fails schema validation, synthesize an
        // error completion (when possible) so worker-side runtimes don't deadlock waiting for a reply.
        if (isRecord(data) && data.type === "usb.action") {
          const actionRaw = data.action;
          const id = extractActionId(actionRaw);
          const kind = extractActionKind(actionRaw);
          if (id !== null && kind !== null) {
            const completion = usbErrorCompletion(kind, id, "Invalid UsbHostAction received from worker.");
            this.postToPort(port, { type: "usb.completion", completion } satisfies UsbCompletionMessage);
          }
          return;
        }

        if (isUsbSelectDeviceMessage(data)) {
          void this.handleSelectDevice(port, data);
        }
      };
      this.portListeners.set(port, onMessage);
      port.addEventListener("message", onMessage);

      // When using addEventListener() MessagePorts need start() to begin dispatch.
      (port as unknown as { start?: () => void }).start?.();

      if (options.attachRings !== false) {
        this.attachRings(port);
      }

      // Newly attached ports should learn the current selection/disconnect state.
      this.postToPort(
        port,
        { type: "usb.guest.controller", mode: this.guestControllerMode } satisfies UsbGuestControllerModeMessage,
      );
      if (this.selectedInfo && !this.disconnectError) {
        this.postToPort(port, { type: "usb.selected", ok: true, info: this.selectedInfo } satisfies UsbSelectedMessage);
      } else if (this.disconnectError) {
        this.postToPort(port, { type: "usb.selected", ok: false, error: this.disconnectError } satisfies UsbSelectedMessage);
      }

      if (this.guestStatus) {
        this.postToPort(
          port,
          { type: "usb.guest.status", snapshot: this.guestStatus } satisfies UsbGuestWebUsbStatusMessage,
        );
      }
    }
  }

  private linkChildPort(parent: MessagePort | Worker, child: MessagePort | Worker): void {
    if (parent === child) return;

    const prevParent = this.portParents.get(child);
    if (prevParent && prevParent !== parent) {
      const siblings = this.portChildren.get(prevParent);
      if (siblings) {
        siblings.delete(child);
        if (siblings.size === 0) this.portChildren.delete(prevParent);
      }
    }

    this.portParents.set(child, parent);
    let children = this.portChildren.get(parent);
    if (!children) {
      children = new Set();
      this.portChildren.set(parent, children);
    }
    children.add(child);
  }

  detachWorkerPort(port: MessagePort | Worker): void {
    // Detach any child ports that were created by (and therefore owned by) this port.
    const children = this.portChildren.get(port);
    if (children) {
      this.portChildren.delete(port);
      for (const child of Array.from(children)) {
        this.detachWorkerPort(child);
      }
    }

    // If this port itself is a child, unlink it from its parent.
    const parent = this.portParents.get(port);
    if (parent) {
      this.portParents.delete(port);
      const siblings = this.portChildren.get(parent);
      if (siblings) {
        siblings.delete(port);
        if (siblings.size === 0) this.portChildren.delete(parent);
      }
    }

    const timer = this.ringDrainTimers.get(port);
    if (timer) {
      clearInterval(timer);
      this.ringDrainTimers.delete(port);
    }
    this.actionRings.delete(port);
    this.completionRings.delete(port);
    this.portBackendOptions.delete(port);

    const listener = this.portListeners.get(port);
    if (listener) {
      this.portListeners.delete(port);
      port.removeEventListener("message", listener);
    }
    this.ports.delete(port);
  }

  private canUseSharedMemory(): boolean {
    // SharedArrayBuffer requires cross-origin isolation in browsers. Node/Vitest may still provide it,
    // but keep the check aligned with the browser contract so behaviour matches production.
    if ((globalThis as unknown as { crossOriginIsolated?: unknown }).crossOriginIsolated !== true) return false;
    if (typeof SharedArrayBuffer === "undefined") return false;
    if (typeof Atomics === "undefined") return false;
    return true;
  }

  private attachRings(port: MessagePort | Worker): void {
    if (!this.canUseSharedMemory()) return;

    let actionRing = this.actionRings.get(port) ?? null;
    let completionRing = this.completionRings.get(port) ?? null;

    if (!actionRing || !completionRing) {
      const actionSab = createUsbProxyRingBuffer(this.ringActionCapacityBytes);
      const completionSab = createUsbProxyRingBuffer(this.ringCompletionCapacityBytes);
      actionRing = new UsbProxyRing(actionSab);
      completionRing = new UsbProxyRing(completionSab);
      this.actionRings.set(port, actionRing);
      this.completionRings.set(port, completionRing);

      const timer = setInterval(() => this.drainActionRing(port), this.ringDrainIntervalMs);
      (timer as unknown as { unref?: () => void }).unref?.();
      this.ringDrainTimers.set(port, timer);
    }

    // Always send the ring handles so late-starting runtimes can attach.
    const msg: UsbRingAttachMessage = {
      type: "usb.ringAttach",
      actionRing: actionRing.buffer(),
      completionRing: completionRing.buffer(),
    };
    this.postToPort(port, msg);
  }

  private drainActionRing(port: MessagePort | Worker): void {
    const actionRing = this.actionRings.get(port);
    if (!actionRing) return;

    let remainingRecords = MAX_USB_ACTION_RING_RECORDS_PER_DRAIN_TICK;
    let remainingBytes = MAX_USB_ACTION_RING_BYTES_PER_DRAIN_TICK;

    while (remainingRecords > 0 && remainingBytes > 0) {
      // When no WebUSB device is selected (or we've already transitioned into a disconnect error state),
      // we still want to drain queued actions so the worker can deliver error completions and unblock the
      // guest. However, in that state we should avoid copying large `bulkOut`/`controlOut` payloads out of
      // the SharedArrayBuffer ring (there is no device to send them to).
      if (!this.device || this.disconnectError) {
        let info: { kind: UsbHostAction["kind"]; id: number; options?: UsbProxyActionOptions; payloadBytes: number } | null = null;
        try {
          info = actionRing.popActionInfo();
        } catch (err) {
          this.disableRingsForPort(port, err);
          break;
        }
        if (!info) break;

        remainingRecords -= 1;
        remainingBytes -= info.payloadBytes;

        const message = this.disconnectError ?? "WebUSB device not selected.";
        const completion = usbErrorCompletion(info.kind, info.id, message);

        const activeCompletionRing = this.completionRings.get(port) ?? null;
        if (activeCompletionRing) {
          try {
            if (activeCompletionRing.pushCompletion(completion)) {
              continue;
            }
          } catch (err) {
            this.disableRingsForPort(port, err);
          }
        }

        const msg: UsbCompletionMessage = { type: "usb.completion", completion };
        this.postToPort(port, msg);
        continue;
      }

      // If a real device is selected and the execute queue is already full, stop
      // draining here to apply backpressure to the ring producer instead of
      // pulling unbounded work onto the main thread.
      let nextPayloadBytes: number | null = null;
      try {
        nextPayloadBytes = actionRing.peekNextActionPayloadBytes();
      } catch (err) {
        this.disableRingsForPort(port, err);
        break;
      }
      if (nextPayloadBytes === null) break;
      if (!this.hasQueueCapacity(nextPayloadBytes)) break;

      let record: { action: UsbHostAction; options?: UsbProxyActionOptions } | null = null;
      try {
        record = actionRing.popActionRecord();
      } catch (err) {
        this.disableRingsForPort(port, err);
        break;
      }
      if (!record) break;
      const { action, options } = record;

      remainingRecords -= 1;
      remainingBytes -= actionPayloadBytes(action);

      void this.executeForPort(port, action, options).then((completion) => {
        // The port may have been detached (or rings disabled) while the completion was in-flight.
        const activeCompletionRing = this.completionRings.get(port) ?? null;
        if (activeCompletionRing) {
          try {
            if (activeCompletionRing.pushCompletion(completion)) return;
          } catch (err) {
            this.disableRingsForPort(port, err);
          }
        }
        const msg: UsbCompletionMessage = { type: "usb.completion", completion };
        this.postToPort(port, msg);
      });
    }
  }

  private async handleSelectDevice(port: MessagePort | Worker, msg: UsbSelectDeviceMessage): Promise<void> {
    try {
      await this.requestDevice(msg.filters);
    } catch (err) {
      const message = formatWebUsbError(err);
      this.postToPort(port, { type: "usb.selected", ok: false, error: message } satisfies UsbSelectedMessage);
    }
  }

  private currentSelectedMessage(): UsbSelectedMessage {
    if (this.selectedInfo && !this.disconnectError) {
      return { type: "usb.selected", ok: true, info: this.selectedInfo } satisfies UsbSelectedMessage;
    }
    if (this.disconnectError) {
      return { type: "usb.selected", ok: false, error: this.disconnectError } satisfies UsbSelectedMessage;
    }
    return { type: "usb.selected", ok: false } satisfies UsbSelectedMessage;
  }

  private async adoptDevice(device: USBDevice): Promise<UsbDeviceInfo> {
    if (device === this.device && this.backendDefault && !this.disconnectError && this.selectedInfo) {
      return this.selectedInfo;
    }

    const backend = new WebUsbBackend(device);

    try {
      await backend.ensureOpenAndClaimed();
    } catch (err) {
      try {
        await device.close();
      } catch {
        // Ignore close errors; failing to open/claim is the root issue.
      }
      throw err;
    }

    // Replace any previously-selected device and fail outstanding actions so they don't accidentally run on the new device.
    this.resetSelectedDevice("WebUSB device replaced.");

    this.device = device;
    this.backendDefault = backend;
    this.backendNoOtherSpeed = null;
    this.selectedInfo = {
      vendorId: device.vendorId,
      productId: device.productId,
      productName: device.productName ?? undefined,
    };
    this.disconnectError = null;
    this.disconnectSignal = createDeferred();

    const msg: UsbSelectedMessage = { type: "usb.selected", ok: true, info: this.selectedInfo };
    this.broadcast(msg);
    return this.selectedInfo;
  }

  private kickQueue(): void {
    if (this.processing) return;
    this.processing = true;
    void this.drainQueue();
  }

  private async drainQueue(): Promise<void> {
    // Chromium's WebUSB implementation does not handle concurrent transfers well for all device/OS combinations.
    // Serializing actions here also matches the upstream UHCI "action queue" contract (one in-flight action at a time).
    while (this.queue.length) {
      const item = this.queue.shift()!;
      this.inFlightCount = 1;

      if (this.disconnectError) {
        this.pendingActionBytes -= item.payloadBytes;
        this.inFlightCount = 0;
        item.resolve(usbErrorCompletion(item.action.kind, item.action.id, this.disconnectError));
        continue;
      }

      const backend = this.getBackendForPort(item.port);
      if (!backend || !this.device) {
        this.pendingActionBytes -= item.payloadBytes;
        this.inFlightCount = 0;
        item.resolve(usbErrorCompletion(item.action.kind, item.action.id, "WebUSB device not selected."));
        continue;
      }

      const backendPromise = backend.execute(item.action, item.options);
      // If the device disconnects, the race below resolves and we drop the backend promise.
      // Avoid unhandled rejections if the backend rejects after disconnect.
      backendPromise.catch(() => undefined);

      let completion: UsbHostCompletion;
      try {
        completion = await Promise.race([
          backendPromise,
          this.disconnectSignal.promise.then((reason) => usbErrorCompletion(item.action.kind, item.action.id, reason)),
        ]);
      } catch (err) {
        completion = usbErrorCompletion(item.action.kind, item.action.id, formatWebUsbError(err));
      }

      this.pendingActionBytes -= item.payloadBytes;
      this.inFlightCount = 0;
      item.resolve(completion);

      if (this.disconnectError) {
        // `handleDisconnect` already flushed queued requests.
        break;
      }
    }

    this.inFlightCount = 0;
    this.processing = false;
  }

  private hasQueueCapacity(nextPayloadBytes: number): boolean {
    const pendingCount = this.queue.length + this.inFlightCount;
    if (pendingCount >= this.maxPendingActions) return false;
    if (this.pendingActionBytes + nextPayloadBytes > this.maxPendingActionBytes) return false;
    return true;
  }

  private resetSelectedDevice(reason: string, options: { closeDevice?: boolean } = {}): void {
    const prevDevice = this.device;
    if (this.backendDefault || this.device) {
      // Resolve in-flight actions (if any) via the disconnect signal.
      this.disconnectError = reason;
      this.disconnectSignal.resolve(reason);
    }

    this.backendDefault = null;
    this.backendNoOtherSpeed = null;
    this.device = null;
    this.selectedInfo = null;

    const shouldCloseDevice = options.closeDevice !== false;
    if (prevDevice && shouldCloseDevice) {
      void prevDevice.close?.().catch(() => undefined);
    }

    // Fail any queued actions immediately.
    while (this.queue.length) {
      const item = this.queue.shift()!;
      this.pendingActionBytes -= item.payloadBytes;
      item.resolve(usbErrorCompletion(item.action.kind, item.action.id, reason));
    }
  }

  private getBackendForPort(port: MessagePort | Worker | null): WebUsbBackend | null {
    const device = this.device;
    if (!device) return null;

    const opts = port ? this.portBackendOptions.get(port) : null;
    const translateOtherSpeed = opts?.translateOtherSpeedConfigurationDescriptor ?? true;

    if (translateOtherSpeed) {
      return this.backendDefault;
    }

    if (this.backendNoOtherSpeed) return this.backendNoOtherSpeed;
    const backend = new WebUsbBackend(device, { translateOtherSpeedConfigurationDescriptor: false });
    this.backendNoOtherSpeed = backend;
    return backend;
  }

  private async executeForPort(
    port: MessagePort | Worker | null,
    action: UsbHostAction,
    options?: UsbProxyActionOptions,
  ): Promise<UsbHostCompletion> {
    if (this.disconnectError) return usbErrorCompletion(action.kind, action.id, this.disconnectError);
    if (!this.device) return usbErrorCompletion(action.kind, action.id, "WebUSB device not selected.");

    return await new Promise<UsbHostCompletion>((resolve) => {
      const payloadBytes = actionPayloadBytes(action);
      if (!this.hasQueueCapacity(payloadBytes)) {
        resolve(usbErrorCompletion(action.kind, action.id, "WebUSB broker queue full (too many pending actions)."));
        return;
      }
      this.pendingActionBytes += payloadBytes;
      this.queue.push({ action, options, resolve, port, payloadBytes });
      this.kickQueue();
    });
  }

  private handleDisconnect(reason: string): void {
    this.resetSelectedDevice(reason);
    this.broadcast({ type: "usb.selected", ok: false, error: reason } satisfies UsbSelectedMessage);
  }

  private postToPort(
    port: MessagePort | Worker,
    msg:
      | UsbCompletionMessage
      | UsbSelectedMessage
      | UsbGuestControllerModeMessage
      | UsbGuestWebUsbStatusMessage
      | UsbRingAttachMessage
      | UsbRingDetachMessage,
  ): void {
    const transfer = msg.type === "usb.completion" ? getTransferablesForUsbCompletionMessage(msg) : undefined;
    if (transfer) {
      try {
        port.postMessage(msg, transfer);
        return;
      } catch {
        // Some ArrayBuffers (e.g. WebAssembly.Memory.buffer) cannot be transferred.
        // Fall back to a regular structured clone (copy) before treating the port as dead.
        try {
          port.postMessage(msg);
          return;
        } catch {
          this.detachWorkerPort(port);
          return;
        }
      }
    }

    try {
      port.postMessage(msg);
    } catch {
      this.detachWorkerPort(port);
    }
  }

  private broadcast(msg: UsbSelectedMessage): void {
    for (const port of this.ports) {
      this.postToPort(port, msg);
    }
  }

  private broadcastGuestControllerMode(msg: UsbGuestControllerModeMessage): void {
    for (const port of this.ports) {
      this.postToPort(port, msg);
    }
  }

  private broadcastGuestStatus(msg: UsbGuestWebUsbStatusMessage): void {
    for (const port of this.ports) {
      this.postToPort(port, msg);
    }
  }

  private detachRingsForPort(port: MessagePort | Worker): void {
    const timer = this.ringDrainTimers.get(port);
    if (timer) {
      clearInterval(timer);
      this.ringDrainTimers.delete(port);
    }
    this.actionRings.delete(port);
    this.completionRings.delete(port);
  }

  private disableRingsForPort(port: MessagePort | Worker, err: unknown): void {
    // Avoid spamming `usb.ringDetach` if multiple callbacks notice the failure.
    if (!this.actionRings.has(port) && !this.completionRings.has(port)) return;

    const message = formatOneLineError(err, 512);
    this.detachRingsForPort(port);
    this.postToPort(
      port,
      {
        type: "usb.ringDetach",
        reason: `USB proxy rings disabled: ${message}`,
      } satisfies UsbRingDetachMessage,
    );
  }

  private emitDeviceChange(): void {
    for (const listener of this.deviceChangeListeners) {
      try {
        listener();
      } catch (err) {
        console.warn("UsbBroker device-change listener failed", err);
      }
    }
  }
}
