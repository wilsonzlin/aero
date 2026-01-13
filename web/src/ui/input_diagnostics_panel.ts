import type { InputBackend } from "../input/input_backend_selection";

export type InputDiagnosticsSnapshot = {
  keyboardBackend: InputBackend;
  mouseBackend: InputBackend;
  virtioKeyboardDriverOk: boolean;
  virtioMouseDriverOk: boolean;
  syntheticUsbKeyboardConfigured: boolean;
  syntheticUsbMouseConfigured: boolean;
  mouseButtonsMask: number;
  pressedKeyboardHidUsageCount: number;
  batchesReceived: number;
  batchesProcessed: number;
  batchesDropped: number;
  eventsProcessed: number;
  keyboardBackendSwitches: number;
  mouseBackendSwitches: number;
};

export type InputDiagnosticsPanelApi = {
  setSnapshot: (snapshot: InputDiagnosticsSnapshot | null) => void;
};

function formatYesNo(value: boolean): string {
  return value ? "yes" : "no";
}

function formatHex32(value: number): string {
  return `0x${(value >>> 0).toString(16).padStart(8, "0")}`;
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
    "Enable via ?input=1 (dev-only) to debug stuck keys and backend switching.";

  const pre = document.createElement("pre");
  pre.className = "mono";

  fieldset.append(help, pre);
  container.replaceChildren(fieldset);

  const setSnapshot = (snapshot: InputDiagnosticsSnapshot | null): void => {
    if (!snapshot) {
      pre.textContent = "No data (I/O worker not running).";
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
      `pressed_hid_usage_count=${snapshot.pressedKeyboardHidUsageCount >>> 0}`,
      `io.batches_received=${snapshot.batchesReceived >>> 0}`,
      `io.batches_processed=${snapshot.batchesProcessed >>> 0}`,
      `io.batches_dropped=${snapshot.batchesDropped >>> 0}`,
      `io.events_processed=${snapshot.eventsProcessed >>> 0}`,
      `io.keyboard_backend_switches=${snapshot.keyboardBackendSwitches >>> 0}`,
      `io.mouse_backend_switches=${snapshot.mouseBackendSwitches >>> 0}`,
    ].join("\n");
  };

  setSnapshot(opts?.initial ?? null);

  return { setSnapshot };
}
