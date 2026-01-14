import { decodeInputBackendStatus } from "../input/input_backend_status";
import type { InputBackend } from "../input/input_backend_selection";
import { StatusIndex } from "../runtime/shared_layout";

export type InputDiagnosticsSnapshot = {
  keyboardBackend: InputBackend;
  mouseBackend: InputBackend;
  virtioKeyboardDriverOk: boolean;
  virtioMouseDriverOk: boolean;
  syntheticUsbKeyboardConfigured: boolean;
  syntheticUsbMouseConfigured: boolean;
  keyboardLedsUsbMask: number;
  keyboardLedsVirtioMask: number;
  keyboardLedsPs2Mask: number;
  mouseButtonsMask: number;
  /**
   * Total number of "keyboard-like" inputs currently held down.
   *
   * Note: this currently includes both:
   * - keyboard HID usages (usage page 0x07), and
   * - Consumer Control usages (usage page 0x0C, e.g. volume/media keys),
   *
   * because backend switching is gated on *either* being held.
   */
  keyboardHeldCount: number;
  batchesReceived: number;
  batchesProcessed: number;
  batchesDropped: number;
  eventsProcessed: number;
  keyboardBackendSwitches: number;
  mouseBackendSwitches: number;
  batchSendLatencyUs: number;
  batchSendLatencyEwmaUs: number;
  batchSendLatencyMaxUs: number;
  eventLatencyAvgUs: number;
  eventLatencyEwmaUs: number;
  eventLatencyMaxUs: number;
};

export function readInputDiagnosticsSnapshotFromStatus(status: Int32Array): InputDiagnosticsSnapshot {
  const keyboardBackend =
    decodeInputBackendStatus(Atomics.load(status, StatusIndex.IoInputKeyboardBackend)) ?? "ps2";
  const mouseBackend = decodeInputBackendStatus(Atomics.load(status, StatusIndex.IoInputMouseBackend)) ?? "ps2";
  return {
    keyboardBackend,
    mouseBackend,
    virtioKeyboardDriverOk: Atomics.load(status, StatusIndex.IoInputVirtioKeyboardDriverOk) !== 0,
    virtioMouseDriverOk: Atomics.load(status, StatusIndex.IoInputVirtioMouseDriverOk) !== 0,
    syntheticUsbKeyboardConfigured: Atomics.load(status, StatusIndex.IoInputUsbKeyboardOk) !== 0,
    syntheticUsbMouseConfigured: Atomics.load(status, StatusIndex.IoInputUsbMouseOk) !== 0,
    keyboardLedsUsbMask: Atomics.load(status, StatusIndex.IoInputKeyboardLedsUsb) >>> 0,
    keyboardLedsVirtioMask: Atomics.load(status, StatusIndex.IoInputKeyboardLedsVirtio) >>> 0,
    keyboardLedsPs2Mask: Atomics.load(status, StatusIndex.IoInputKeyboardLedsPs2) >>> 0,
    mouseButtonsMask: Atomics.load(status, StatusIndex.IoInputMouseButtonsHeldMask) >>> 0,
    keyboardHeldCount: Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0,
    batchesReceived: Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0,
    batchesProcessed: Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0,
    batchesDropped: Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0,
    eventsProcessed: Atomics.load(status, StatusIndex.IoInputEventCounter) >>> 0,
    keyboardBackendSwitches: Atomics.load(status, StatusIndex.IoKeyboardBackendSwitchCounter) >>> 0,
    mouseBackendSwitches: Atomics.load(status, StatusIndex.IoMouseBackendSwitchCounter) >>> 0,
    batchSendLatencyUs: Atomics.load(status, StatusIndex.IoInputBatchSendLatencyUs) >>> 0,
    batchSendLatencyEwmaUs: Atomics.load(status, StatusIndex.IoInputBatchSendLatencyEwmaUs) >>> 0,
    batchSendLatencyMaxUs: Atomics.load(status, StatusIndex.IoInputBatchSendLatencyMaxUs) >>> 0,
    eventLatencyAvgUs: Atomics.load(status, StatusIndex.IoInputEventLatencyAvgUs) >>> 0,
    eventLatencyEwmaUs: Atomics.load(status, StatusIndex.IoInputEventLatencyEwmaUs) >>> 0,
    eventLatencyMaxUs: Atomics.load(status, StatusIndex.IoInputEventLatencyMaxUs) >>> 0,
  };
}

export type InputDiagnosticsPanelApi = {
  setSnapshot: (snapshot: InputDiagnosticsSnapshot | null) => void;
};

function formatYesNo(value: boolean): string {
  return value ? "yes" : "no";
}

function formatHex32(value: number): string {
  return `0x${(value >>> 0).toString(16).padStart(8, "0")}`;
}

function formatMouseButtonsHeld(mask: number): string {
  const names: string[] = [];
  if ((mask & 0x01) !== 0) names.push("left");
  if ((mask & 0x02) !== 0) names.push("right");
  if ((mask & 0x04) !== 0) names.push("middle");
  if ((mask & 0x08) !== 0) names.push("back");
  if ((mask & 0x10) !== 0) names.push("forward");
  return names.length ? names.join(",") : "(none)";
}

function formatKeyboardLeds(mask: number): string {
  const m = mask & 0x1f;
  const names: string[] = [];
  if ((m & 0x01) !== 0) names.push("num");
  if ((m & 0x02) !== 0) names.push("caps");
  if ((m & 0x04) !== 0) names.push("scroll");
  if ((m & 0x08) !== 0) names.push("compose");
  if ((m & 0x10) !== 0) names.push("kana");
  return names.length ? names.join(",") : "(none)";
}

function formatLatencyUs(value: number): string {
  const us = value >>> 0;
  const ms = us / 1000;
  return `${us} us (${ms.toFixed(3)} ms)`;
}

export function mountInputDiagnosticsPanel(container: HTMLElement, opts?: { initial?: InputDiagnosticsSnapshot | null }): InputDiagnosticsPanelApi {
  const fieldset = document.createElement("fieldset");
  const legend = document.createElement("legend");
  legend.textContent = "Input diagnostics";
  fieldset.append(legend);

  const help = document.createElement("div");
  help.className = "hint";
  help.textContent =
    "Shows current input backend selection and held state. " +
    "Enable via ?input=1 (or ?input) to debug stuck keys and backend switching.";

  const pre = document.createElement("pre");
  pre.className = "mono";

  fieldset.append(help, pre);
  container.replaceChildren(fieldset);

  const setSnapshot = (snapshot: InputDiagnosticsSnapshot | null): void => {
    if (!snapshot) {
      pre.textContent = "No data (VM not running).";
      return;
    }

    pre.textContent = [
      `keyboard_backend=${snapshot.keyboardBackend}`,
      `mouse_backend=${snapshot.mouseBackend}`,
      `virtio_keyboard.driver_ok=${formatYesNo(snapshot.virtioKeyboardDriverOk)}`,
      `virtio_mouse.driver_ok=${formatYesNo(snapshot.virtioMouseDriverOk)}`,
      `synthetic_usb_keyboard.configured=${formatYesNo(snapshot.syntheticUsbKeyboardConfigured)}`,
      `synthetic_usb_mouse.configured=${formatYesNo(snapshot.syntheticUsbMouseConfigured)}`,
      `mouse_buttons_mask=${formatHex32(snapshot.mouseButtonsMask)}`,
      `mouse_buttons_held=${formatMouseButtonsHeld(snapshot.mouseButtonsMask)}`,
      `keyboard_held_count=${snapshot.keyboardHeldCount >>> 0}`,
      `keyboard_leds_usb=${formatHex32(snapshot.keyboardLedsUsbMask)} ${formatKeyboardLeds(snapshot.keyboardLedsUsbMask)}`,
      `keyboard_leds_virtio=${formatHex32(snapshot.keyboardLedsVirtioMask)} ${formatKeyboardLeds(snapshot.keyboardLedsVirtioMask)}`,
      `keyboard_leds_ps2=${formatHex32(snapshot.keyboardLedsPs2Mask)} ${formatKeyboardLeds(snapshot.keyboardLedsPs2Mask)}`,
      `io.batches_received=${snapshot.batchesReceived >>> 0}`,
      `io.batches_processed=${snapshot.batchesProcessed >>> 0}`,
      `io.batches_dropped=${snapshot.batchesDropped >>> 0}`,
      `io.events_processed=${snapshot.eventsProcessed >>> 0}`,
      `io.keyboard_backend_switches=${snapshot.keyboardBackendSwitches >>> 0}`,
      `io.mouse_backend_switches=${snapshot.mouseBackendSwitches >>> 0}`,
      `io.batch_send_latency_us=${formatLatencyUs(snapshot.batchSendLatencyUs)}`,
      `io.batch_send_latency_ewma_us=${formatLatencyUs(snapshot.batchSendLatencyEwmaUs)}`,
      `io.batch_send_latency_max_us=${formatLatencyUs(snapshot.batchSendLatencyMaxUs)}`,
      `io.event_latency_avg_us=${formatLatencyUs(snapshot.eventLatencyAvgUs)}`,
      `io.event_latency_ewma_us=${formatLatencyUs(snapshot.eventLatencyEwmaUs)}`,
      `io.event_latency_max_us=${formatLatencyUs(snapshot.eventLatencyMaxUs)}`,
    ].join("\n");
  };

  setSnapshot(opts?.initial ?? null);

  return { setSnapshot };
}
