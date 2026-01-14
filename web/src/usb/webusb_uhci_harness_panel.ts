import type { PlatformFeatureReport } from "../platform/features";
import { explainWebUsbError, formatWebUsbError } from "../platform/webusb_troubleshooting";
import type { WasmInitResult } from "../runtime/wasm_loader";
import { WebUsbBackend, type SetupPacket, type UsbHostAction, type UsbHostCompletion } from "./webusb_backend";
import { formatHexBytes, hex16 } from "./usb_hex";
import { isUsbSetupPacket } from "./usb_proxy_protocol";

export interface UhciHarnessLike {
  drain_actions(): unknown;
  push_completion(completion: UsbHostCompletion): void;
}

export interface UsbHostActionExecutor {
  execute(action: UsbHostAction): Promise<UsbHostCompletion>;
}

export type UhciHarnessDrainResult = {
  readonly actions: UsbHostAction[];
  readonly completions: UsbHostCompletion[];
};

function normalizeActionId(value: unknown): number {
  const maxU32 = 0xffff_ffff;
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || value < 0 || value > maxU32) {
      throw new Error(`Expected action id to be uint32, got ${String(value)}`);
    }
    return value;
  }
  if (typeof value === "bigint") {
    if (value < 0n) {
      throw new Error(`Expected action id to be non-negative, got ${value.toString()}`);
    }
    if (value > BigInt(maxU32)) {
      throw new Error(`Expected action id to fit in uint32, got ${value.toString()}`);
    }
    return Number(value);
  }
  throw new Error(`Expected action id to be number or bigint, got ${typeof value}`);
}

function normalizeBytes(value: unknown): Uint8Array {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (typeof SharedArrayBuffer !== "undefined" && value instanceof SharedArrayBuffer) {
    return new Uint8Array(value);
  }
  if (Array.isArray(value)) {
    if (
      !value.every(
        (v) => typeof v === "number" && Number.isFinite(v) && Number.isInteger(v) && v >= 0 && v <= 0xff,
      )
    ) {
      throw new Error("Expected byte array to contain only uint8 numbers");
    }
    return Uint8Array.from(value as number[]);
  }
  throw new Error(
    `Expected bytes to be Uint8Array, ArrayBuffer, SharedArrayBuffer, or number[]; got ${typeof value}`,
  );
}

function normalizeU8(value: unknown): number {
  const asNum = typeof value === "number" ? value : typeof value === "bigint" ? Number(value) : NaN;
  if (!Number.isFinite(asNum) || !Number.isInteger(asNum) || asNum < 0 || asNum > 0xff) {
    throw new Error(`Expected uint8, got ${String(value)}`);
  }
  return asNum;
}

function normalizeU32(value: unknown): number {
  const asNum = typeof value === "number" ? value : typeof value === "bigint" ? Number(value) : NaN;
  if (!Number.isFinite(asNum) || !Number.isInteger(asNum) || asNum < 0 || asNum > 0xffff_ffff) {
    throw new Error(`Expected uint32, got ${String(value)}`);
  }
  return asNum;
}

function isUsbEndpointAddress(value: number): boolean {
  // `value` should be a USB endpoint address, not just an endpoint number:
  // - bit7 = direction (IN=1, OUT=0)
  // - bits4..6 must be 0 (endpoint numbers are 0..=15)
  // - endpoint 0 is the control pipe and should not be used for bulk/interrupt actions
  return (value & 0x70) === 0 && (value & 0x0f) !== 0;
}

function assertUsbInEndpointAddress(value: number): void {
  if (!isUsbEndpointAddress(value) || (value & 0x80) === 0) {
    throw new Error(`Expected IN endpoint address (e.g. 0x81), got 0x${value.toString(16)}`);
  }
}

function assertUsbOutEndpointAddress(value: number): void {
  if (!isUsbEndpointAddress(value) || (value & 0x80) !== 0) {
    throw new Error(`Expected OUT endpoint address (e.g. 0x02), got 0x${value.toString(16)}`);
  }
}

function normalizeUsbHostAction(raw: unknown): UsbHostAction {
  if (!raw || typeof raw !== "object") {
    throw new Error(`Expected USB action to be object, got ${raw === null ? "null" : typeof raw}`);
  }
  const obj = raw as Record<string, unknown>;
  const kind = obj.kind;
  const id = normalizeActionId(obj.id);
  if (typeof kind !== "string") throw new Error("USB action missing kind");

  switch (kind as UsbHostAction["kind"]) {
    case "controlIn": {
      const setup = obj.setup;
      if (!isUsbSetupPacket(setup)) throw new Error("controlIn missing/invalid setup packet");
      return { kind: "controlIn", id, setup };
    }
    case "controlOut": {
      const setup = obj.setup;
      if (!isUsbSetupPacket(setup)) throw new Error("controlOut missing/invalid setup packet");
      return { kind: "controlOut", id, setup, data: normalizeBytes(obj.data) };
    }
    case "bulkIn": {
      const endpoint = normalizeU8(obj.endpoint);
      assertUsbInEndpointAddress(endpoint);
      return { kind: "bulkIn", id, endpoint, length: normalizeU32(obj.length) };
    }
    case "bulkOut": {
      const endpoint = normalizeU8(obj.endpoint);
      assertUsbOutEndpointAddress(endpoint);
      return { kind: "bulkOut", id, endpoint, data: normalizeBytes(obj.data) };
    }
    default:
      throw new Error(`Unknown USB action kind: ${String(kind)}`);
  }
}

function asUsbHostActions(raw: unknown): UsbHostAction[] {
  if (raw === null || raw === undefined) return [];
  if (!Array.isArray(raw)) {
    throw new Error(`Expected harness.drain_actions() to return an array, got ${typeof raw}`);
  }
  return raw.map((action) => normalizeUsbHostAction(action));
}

export async function bridgeHarnessDrainActions(
  harness: UhciHarnessLike,
  backend: UsbHostActionExecutor,
): Promise<UhciHarnessDrainResult> {
  const anyHarness = harness as unknown as Record<string, unknown>;
  // Backwards compatibility: accept both snake_case and camelCase harness exports and always invoke
  // extracted methods via `.call(harness, ...)` to avoid wasm-bindgen `this` binding pitfalls.
  const drainActions = anyHarness.drain_actions ?? anyHarness.drainActions;
  const pushCompletion = anyHarness.push_completion ?? anyHarness.pushCompletion;
  if (typeof drainActions !== "function") throw new Error("UHCI harness missing drain_actions/drainActions export.");
  if (typeof pushCompletion !== "function") throw new Error("UHCI harness missing push_completion/pushCompletion export.");

  const actions = asUsbHostActions((drainActions as () => unknown).call(harness));
  const completions: UsbHostCompletion[] = [];

  for (const action of actions) {
    const completion = await backend.execute(action);
    (pushCompletion as (completion: UsbHostCompletion) => void).call(harness, completion);
    completions.push(completion);
  }

  return { actions, completions };
}

const GET_DESCRIPTOR = 0x06;
const DESCRIPTOR_TYPE_DEVICE = 0x01;
const DESCRIPTOR_TYPE_CONFIGURATION = 0x02;

type DescriptorCapture = {
  deviceDescriptor: Uint8Array | null;
  configDescriptor: Uint8Array | null;
};

function classifyDescriptorRequest(setup: SetupPacket): "device" | "config" | null {
  if ((setup.bRequest & 0xff) !== GET_DESCRIPTOR) return null;
  const descType = (setup.wValue >> 8) & 0xff;
  if (descType === DESCRIPTOR_TYPE_DEVICE) return "device";
  if (descType === DESCRIPTOR_TYPE_CONFIGURATION) return "config";
  return null;
}

function maybeCaptureDescriptors(
  capture: DescriptorCapture,
  action: UsbHostAction,
  completion: UsbHostCompletion,
): void {
  if (action.kind !== "controlIn") return;
  if (completion.kind !== "controlIn") return;
  if (completion.status !== "success") return;

  const cls = classifyDescriptorRequest(action.setup);
  if (!cls) return;

  const bytes = completion.data;
  if (cls === "device") {
    // Prefer longer descriptors if the harness retries with a longer wLength.
    if (!capture.deviceDescriptor || bytes.byteLength >= capture.deviceDescriptor.byteLength) {
      capture.deviceDescriptor = bytes;
    }
    return;
  }

  if (cls === "config") {
    // Configuration descriptors are frequently requested twice (first the 9-byte header,
    // then the full wTotalLength). Keep the most complete one.
    if (!capture.configDescriptor || bytes.byteLength >= capture.configDescriptor.byteLength) {
      capture.configDescriptor = bytes;
    }
  }
}

function maybeCaptureDescriptorsFromHarnessStatus(capture: DescriptorCapture, status: unknown): void {
  if (!status || typeof status !== "object") return;
  const obj = status as Record<string, unknown>;

  const deviceDesc = obj.deviceDescriptor;
  if (deviceDesc !== undefined && deviceDesc !== null) {
    const bytes = normalizeBytes(deviceDesc);
    if (!capture.deviceDescriptor || bytes.byteLength >= capture.deviceDescriptor.byteLength) {
      capture.deviceDescriptor = bytes;
    }
  }

  const configDesc = obj.configDescriptor;
  if (configDesc !== undefined && configDesc !== null) {
    const bytes = normalizeBytes(configDesc);
    if (!capture.configDescriptor || bytes.byteLength >= capture.configDescriptor.byteLength) {
      capture.configDescriptor = bytes;
    }
  }
}

function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value, (_key, v) => (typeof v === "bigint" ? v.toString() : v));
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return `[unserializable: ${message}]`;
  }
}

function describeUsbDevice(device: USBDevice): string {
  const parts = [`${hex16(device.vendorId)}:${hex16(device.productId)}`];
  if (device.manufacturerName) parts.push(device.manufacturerName);
  if (device.productName) parts.push(device.productName);
  if (device.serialNumber) parts.push(`sn=${device.serialNumber}`);
  if (device.opened) parts.push("(opened)");
  return parts.join(" ");
}

async function requestUsbDevice(usb: USB): Promise<USBDevice> {
  // Chromium versions differ on whether `filters: []` and/or `filters: [{}]` are allowed.
  // Try a few reasonable fallbacks so the smoke test works in more environments.
  const attempts: USBDeviceRequestOptions[] = [
    { filters: [] },
    { filters: [{}] },
    { filters: [{ classCode: 0x00 }, { classCode: 0xff }] },
    { filters: [{ classCode: 0xff }] },
  ];

  let lastErr: unknown = null;
  for (const options of attempts) {
    try {
      return await usb.requestDevice(options);
    } catch (err) {
      lastErr = err;

      // User canceled the chooser (or there were no matching devices).
      if (err instanceof DOMException && err.name === "NotFoundError") {
        throw err;
      }

      // If the browser rejected the filter shape, try the next fallback.
      if (err instanceof TypeError) continue;
      if (err instanceof DOMException && err.name === "TypeError") continue;

      throw err;
    }
  }

  throw lastErr ?? new Error("USB requestDevice failed");
}

async function yieldToEventLoop(): Promise<void> {
  await new Promise<void>((resolve) => {
    const raf = (globalThis as unknown as { requestAnimationFrame?: (cb: () => void) => number }).requestAnimationFrame;
    if (typeof raf === "function") {
      raf(() => resolve());
      return;
    }
    setTimeout(resolve, 0);
  });
}

function readHarnessState(harness: unknown): string {
  if (!harness || typeof harness !== "object") return "n/a";
  const h = harness as Record<string, unknown>;

  const candidates = ["state", "status", "debug_state", "debugState"];
  for (const key of candidates) {
    const v = h[key];
    if (typeof v === "function") {
      try {
        const out = (v as () => unknown)();
        if (typeof out === "string") return out;
        return safeJson(out);
      } catch (err) {
        return `error reading ${key}(): ${err instanceof Error ? err.message : String(err)}`;
      }
    }
    if (typeof v === "string") return v;
  }

  return "unknown";
}

function safeFree(obj: unknown): void {
  if (!obj || typeof obj !== "object") return;
  const free = (obj as { free?: unknown }).free;
  if (typeof free === "function") {
    try {
      free.call(obj);
    } catch {
      // ignore
    }
  }
}

export function renderWebUsbUhciHarnessPanel(
  report: PlatformFeatureReport,
  wasmInitPromise: Promise<WasmInitResult>,
): HTMLElement {
  const panel = document.createElement("div");
  panel.className = "panel";

  const title = document.createElement("h2");
  title.textContent = "UHCI passthrough harness (WebUSB)";

  const note = document.createElement("div");
  note.className = "hint";
  note.textContent =
    "Manual end-to-end smoke test: select a physical WebUSB device, then run the WASM UHCI enumeration harness. " +
    "The harness emits UsbHostAction requests which are executed via WebUSB and fed back as completions.";

  const requestButton = document.createElement("button");
  requestButton.type = "button";
  requestButton.textContent = "Request USB device";

  const listButton = document.createElement("button");
  listButton.type = "button";
  listButton.textContent = "List permitted devices";

  const runButton = document.createElement("button");
  runButton.type = "button";
  runButton.textContent = "Run harness";
  runButton.disabled = true;

  const stopButton = document.createElement("button");
  stopButton.type = "button";
  stopButton.textContent = "Stop/Reset";
  stopButton.disabled = true;

  const actionsRow = document.createElement("div");
  actionsRow.className = "row";
  actionsRow.append(requestButton, listButton, runButton, stopButton);

  const deviceStatus = document.createElement("pre");
  deviceStatus.className = "mono";

  const permittedList = document.createElement("ul");

  const harnessStatus = document.createElement("pre");
  harnessStatus.className = "mono";

  const deviceDescPre = document.createElement("pre");
  deviceDescPre.className = "mono";

  const configDescPre = document.createElement("pre");
  configDescPre.className = "mono";

  const errorTitle = document.createElement("div");
  errorTitle.className = "bad";

  const errorDetails = document.createElement("div");
  errorDetails.className = "hint";

  const errorRaw = document.createElement("pre");
  errorRaw.className = "mono";

  const errorHints = document.createElement("ul");

  panel.append(
    title,
    note,
    actionsRow,
    deviceStatus,
    permittedList,
    harnessStatus,
    document.createTextNode("Device descriptor (latest):"),
    deviceDescPre,
    document.createTextNode("Configuration descriptor (latest):"),
    configDescPre,
    errorTitle,
    errorDetails,
    errorRaw,
    errorHints,
  );

  let selected: USBDevice | null = null;
  let backend: WebUsbBackend | null = null;

  let harness: unknown = null;
  let abortController: AbortController | null = null;
  let runPromise: Promise<void> | null = null;

  let wasmInitErr: unknown = null;
  let hasHarnessExport: boolean | null = null;

  const capture: DescriptorCapture = {
    deviceDescriptor: null,
    configDescriptor: null,
  };

  let tickCount = 0;
  let actionCount = 0;
  let completionCount = 0;
  let lastAction: UsbHostAction | null = null;
  let lastCompletion: UsbHostCompletion | null = null;

  const clearError = () => {
    errorTitle.textContent = "";
    errorDetails.textContent = "";
    errorRaw.textContent = "";
    errorHints.replaceChildren();
  };

  const showError = (err: unknown) => {
    const explained = explainWebUsbError(err);
    errorTitle.textContent = explained.title;
    errorDetails.textContent = explained.details ?? "";
    errorRaw.textContent = formatWebUsbError(err);
    errorHints.replaceChildren(
      ...explained.hints.map((hint) => {
        const li = document.createElement("li");
        li.textContent = hint;
        return li;
      }),
    );
  };

  const resetHarnessUiState = () => {
    tickCount = 0;
    actionCount = 0;
    completionCount = 0;
    lastAction = null;
    lastCompletion = null;
    capture.deviceDescriptor = null;
    capture.configDescriptor = null;
  };

  const freeHarness = () => {
    safeFree(harness);
    harness = null;
  };

  const refreshUi = () => {
    const env = [
      `webusb=${report.webusb}`,
      `wasmHarnessExport=${hasHarnessExport === null ? "loading" : hasHarnessExport}`,
      wasmInitErr ? `wasmInitError=${wasmInitErr instanceof Error ? wasmInitErr.message : String(wasmInitErr)}` : null,
    ]
      .filter((line): line is string => typeof line === "string" && line.length > 0)
      .join("\n");

    if (!report.webusb) {
      deviceStatus.textContent = `WebUSB is unavailable in this context.\n\n${env}`;
      requestButton.disabled = true;
      listButton.disabled = true;
      runButton.disabled = true;
      stopButton.disabled = true;
    } else if (!selected || !backend) {
      deviceStatus.textContent = `No WebUSB device selected.\n\n${env}`;
      requestButton.disabled = false;
      listButton.disabled = false;
      runButton.disabled = true;
      stopButton.disabled = true;
    } else {
      deviceStatus.textContent = `Selected device: ${describeUsbDevice(selected)}\n\n${env}`;
      const running = runPromise !== null;
      requestButton.disabled = running;
      listButton.disabled = running;
      runButton.disabled = running || hasHarnessExport !== true;
      stopButton.disabled = !running && tickCount === 0 && actionCount === 0 && completionCount === 0;
    }

    const running = runPromise !== null;
    const stopRequested = abortController?.signal.aborted ?? false;
    if (running) {
      stopButton.textContent = stopRequested ? "Stopping…" : "Stop";
      stopButton.disabled = stopRequested;
    } else {
      stopButton.textContent = "Reset";
    }
    const harnessStateText = harness ? readHarnessState(harness) : "n/a";
    harnessStatus.textContent =
      `Harness: ${running ? "running" : "stopped"}\n` +
      `state: ${harnessStateText}\n` +
      `ticks: ${tickCount}\n` +
      `actions: ${actionCount}\n` +
      `completions: ${completionCount}\n` +
      `lastAction: ${lastAction ? safeJson(lastAction) : "n/a"}\n` +
      `lastCompletion: ${lastCompletion ? safeJson(lastCompletion) : "n/a"}\n`;

    deviceDescPre.textContent = capture.deviceDescriptor ? formatHexBytes(capture.deviceDescriptor) : "(none yet)";
    configDescPre.textContent = capture.configDescriptor ? formatHexBytes(capture.configDescriptor) : "(none yet)";
  };

  refreshUi();

  void wasmInitPromise
    .then((wasm) => {
      hasHarnessExport = typeof wasm.api.WebUsbUhciPassthroughHarness === "function";
      wasmInitErr = null;
      refreshUi();
    })
    .catch((err) => {
      wasmInitErr = err;
      hasHarnessExport = false;
      refreshUi();
    });

  const stopAndWait = async () => {
    abortController?.abort();
    const pending = runPromise;
    if (!pending) return;
    try {
      await pending;
    } catch {
      // ignore; errors are rendered to the UI inside the runner.
    }
  };

  if (typeof navigator !== "undefined" && "usb" in navigator) {
    navigator.usb?.addEventListener?.("disconnect", (ev) => {
      const device = (ev as unknown as { device?: USBDevice }).device;
      if (!device || !selected) return;
      if (device !== selected) return;
      void (async () => {
        await stopAndWait();
        freeHarness();
        selected = null;
        backend = null;
        resetHarnessUiState();
        refreshUi();
      })();
    });
  }

  const selectDevice = async (device: USBDevice) => {
    await stopAndWait();
    freeHarness();
    resetHarnessUiState();
    clearError();
    permittedList.replaceChildren();

    const newBackend = new WebUsbBackend(device);
    try {
      await newBackend.ensureOpenAndClaimed();
    } catch (err) {
      showError(err);
      console.error(err);
      return;
    }

    selected = device;
    backend = newBackend;
    refreshUi();
  };

  requestButton.onclick = async () => {
    clearError();
    permittedList.replaceChildren();

    if (!report.webusb) {
      errorTitle.textContent = "WebUSB is unavailable in this context.";
      refreshUi();
      return;
    }

    try {
      const usb = navigator.usb;
      if (!usb) throw new Error("navigator.usb is unavailable");
      const device = await requestUsbDevice(usb);
      await selectDevice(device);
    } catch (err) {
      showError(err);
      console.error(err);
    } finally {
      refreshUi();
    }
  };

  listButton.onclick = async () => {
    clearError();
    permittedList.replaceChildren();

    if (!report.webusb) {
      errorTitle.textContent = "WebUSB is unavailable in this context.";
      refreshUi();
      return;
    }

    try {
      const usb = navigator.usb;
      if (!usb) throw new Error("navigator.usb is unavailable");
      const devices = await usb.getDevices();
      if (devices.length === 0) {
        const li = document.createElement("li");
        li.textContent = "(none)";
        permittedList.append(li);
        return;
      }

      for (const device of devices) {
        const li = document.createElement("li");
        const btn = document.createElement("button");
        btn.type = "button";
        btn.textContent = "Select";
        btn.onclick = () => {
          void selectDevice(device);
        };
        li.append(btn, " ", describeUsbDevice(device));
        permittedList.append(li);
      }
    } catch (err) {
      showError(err);
      console.error(err);
    } finally {
      refreshUi();
    }
  };

  runButton.onclick = () => {
    if (!backend || !selected) return;
    if (runPromise) return;

    clearError();
    resetHarnessUiState();
    freeHarness();

    const controller = new AbortController();
    abortController = controller;
    const { signal } = controller;
    refreshUi();

    runPromise = (async () => {
      let wasm: WasmInitResult;
      try {
        wasm = await wasmInitPromise;
      } catch (err) {
        wasmInitErr = err;
        showError(err);
        return;
      }

      const ctor = wasm.api.WebUsbUhciPassthroughHarness;
      if (typeof ctor !== "function") {
        showError(new Error("WASM export missing: WebUsbUhciPassthroughHarness"));
        return;
      }

      try {
        // eslint-disable-next-line @typescript-eslint/no-unsafe-call, @typescript-eslint/no-unsafe-assignment
        harness = new (ctor as new () => unknown)();
      } catch (err) {
        showError(err);
        return;
      }

      try {
        while (!signal.aborted) {
          tickCount += 1;

          // eslint-disable-next-line @typescript-eslint/no-unsafe-call
          // eslint-disable-next-line @typescript-eslint/no-unsafe-assignment
          const tickStatus = (harness as unknown as { tick: () => unknown }).tick();
          maybeCaptureDescriptorsFromHarnessStatus(capture, tickStatus);

          const { actions, completions } = await bridgeHarnessDrainActions(harness as UhciHarnessLike, backend);
          if (actions.length > 0) {
            actionCount += actions.length;
            lastAction = actions[actions.length - 1] ?? null;
          }
          if (completions.length > 0) {
            completionCount += completions.length;
            lastCompletion = completions[completions.length - 1] ?? null;
          }
          for (let i = 0; i < actions.length; i += 1) {
            const action = actions[i];
            const completion = completions[i];
            if (!action || !completion) continue;
            maybeCaptureDescriptors(capture, action, completion);
          }

          const stateText = readHarnessState(harness);
          refreshUi();
          if (stateText.startsWith("Done") || stateText.startsWith("Error")) {
            break;
          }
          await yieldToEventLoop();
        }
      } catch (err) {
        showError(err);
        console.error(err);
      }
    })()
      .catch((err) => {
        // Shouldn't happen (runner catches), but keep this defensive so we never
        // surface an unhandled rejection in the browser console.
        showError(err);
        console.error(err);
      })
      .finally(() => {
        abortController = null;
        runPromise = null;
        refreshUi();
      });
  };

  stopButton.onclick = () => {
    if (runPromise) {
      abortController?.abort();
      refreshUi();
      return;
    }
    clearError();
    freeHarness();
    resetHarnessUiState();
    refreshUi();
  };

  return panel;
}
