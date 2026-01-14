import { describe, expect, it } from "vitest";

import { initWasm } from "../runtime/wasm_loader";
import { assertWasmMemoryWiring } from "../runtime/wasm_memory_probe";
import { computeGuestRamLayout } from "../runtime/shared_layout";
import {
  USB_HID_BOOT_KEYBOARD_REPORT_DESCRIPTOR,
  USB_HID_BOOT_MOUSE_REPORT_DESCRIPTOR,
  USB_HID_CONSUMER_CONTROL_REPORT_DESCRIPTOR,
  USB_HID_GAMEPAD_REPORT_DESCRIPTOR,
  USB_HID_INTERFACE_PROTOCOL_KEYBOARD,
  USB_HID_INTERFACE_PROTOCOL_MOUSE,
  USB_HID_INTERFACE_SUBCLASS_BOOT,
} from "./hid_descriptors";
import {
  DEFAULT_EXTERNAL_HUB_PORT_COUNT,
  EXTERNAL_HUB_ROOT_PORT,
  UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT,
  UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT,
  UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT,
  UHCI_SYNTHETIC_HID_HUB_PORT_COUNT,
  UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT,
  UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT,
} from "./uhci_external_hub";

const REG_USBCMD = 0x00;
const REG_USBINTR = 0x04;
const REG_FLBASEADD = 0x08;
const REG_PORTSC1 = 0x10;

const USBCMD_RS = 1 << 0;
const USBCMD_MAXP = 1 << 7;
const USBINTR_IOC = 1 << 2;

const PORTSC_PR = 1 << 9;

const LINK_PTR_T = 1 << 0;
const LINK_PTR_Q = 1 << 1;

const TD_CTRL_ACTIVE = 1 << 23;
const TD_CTRL_IOC = 1 << 24;
const TD_CTRL_ACTLEN_MASK = 0x7ff;

const PID_SETUP = 0x2d;
const PID_IN = 0x69;
const PID_OUT = 0xe1;

function actlen(ctrlSts: number): number {
  const field = ctrlSts & TD_CTRL_ACTLEN_MASK;
  if (field === 0x7ff) return 0;
  return field + 1;
}

function tdCtrl(active: boolean, ioc: boolean): number {
  let v = 0x7ff;
  if (active) v |= TD_CTRL_ACTIVE;
  if (ioc) v |= TD_CTRL_IOC;
  return v >>> 0;
}

function tdToken(pid: number, addr: number, ep: number, toggle: boolean, maxLen: number): number {
  const maxLenField = maxLen === 0 ? 0x7ff : (maxLen - 1) & 0x7ff;
  return (
    (pid & 0xff) |
    ((addr & 0x7f) << 8) |
    ((ep & 0x0f) << 15) |
    (toggle ? 1 << 19 : 0) |
    (maxLenField << 21)
  ) >>> 0;
}

class Alloc {
  #next: number;
  constructor(base: number) {
    this.#next = base >>> 0;
  }

  alloc(size: number, align: number): number {
    const a = Math.max(1, align >>> 0);
    const mask = a - 1;
    const aligned = (this.#next + mask) & ~mask;
    this.#next = (aligned + (size >>> 0)) >>> 0;
    return aligned >>> 0;
  }
}

type SetupPacket = { bmRequestType: number; bRequest: number; wValue: number; wIndex: number; wLength: number };

function encodeSetupPacket(setup: SetupPacket): Uint8Array {
  const out = new Uint8Array(8);
  out[0] = setup.bmRequestType & 0xff;
  out[1] = setup.bRequest & 0xff;
  out[2] = setup.wValue & 0xff;
  out[3] = (setup.wValue >>> 8) & 0xff;
  out[4] = setup.wIndex & 0xff;
  out[5] = (setup.wIndex >>> 8) & 0xff;
  out[6] = setup.wLength & 0xff;
  out[7] = (setup.wLength >>> 8) & 0xff;
  return out;
}

function writeTd(view: DataView, guestBase: number, addr: number, link: number, ctrlSts: number, token: number, buffer: number): void {
  const o = guestBase + (addr >>> 0);
  view.setUint32(o, link >>> 0, true);
  view.setUint32(o + 4, ctrlSts >>> 0, true);
  view.setUint32(o + 8, token >>> 0, true);
  view.setUint32(o + 12, buffer >>> 0, true);
}

function writeQh(view: DataView, guestBase: number, addr: number, head: number, element: number): void {
  const o = guestBase + (addr >>> 0);
  view.setUint32(o, head >>> 0, true);
  view.setUint32(o + 4, element >>> 0, true);
}

function installFrameList(view: DataView, guestBase: number, flBase: number, qhAddr: number): void {
  const base = guestBase + (flBase >>> 0);
  const entry = (qhAddr | LINK_PTR_Q) >>> 0;
  for (let i = 0; i < 1024; i += 1) {
    view.setUint32(base + i * 4, entry, true);
  }
}

function tick1ms(uhci: unknown): void {
  const u = uhci as { tick_1ms?: () => void; step_frame?: () => void };
  if (typeof u.step_frame === "function") {
    u.step_frame();
    return;
  }
  u.tick_1ms?.();
}

function controlNoData(opts: {
  uhci: { io_write(offset: number, size: number, value: number): void };
  view: DataView;
  guestBase: number;
  alloc: Alloc;
  flBase: number;
  devAddr: number;
  setup: SetupPacket;
}): void {
  const { uhci, view, guestBase, alloc, flBase, devAddr, setup } = opts;

  const qhAddr = alloc.alloc(0x20, 0x10);
  const setupBuf = alloc.alloc(8, 0x10);
  const setupTd = alloc.alloc(0x20, 0x10);
  const statusTd = alloc.alloc(0x20, 0x10);

  const bytes = encodeSetupPacket(setup);
  new Uint8Array(view.buffer).set(bytes, guestBase + setupBuf);

  writeTd(view, guestBase, setupTd, statusTd, tdCtrl(true, false), tdToken(PID_SETUP, devAddr, 0, false, 8), setupBuf);
  writeTd(view, guestBase, statusTd, LINK_PTR_T, tdCtrl(true, true), tdToken(PID_IN, devAddr, 0, true, 0), 0);
  writeQh(view, guestBase, qhAddr, LINK_PTR_T, setupTd);
  installFrameList(view, guestBase, flBase, qhAddr);

  // The controller must be running for the schedule to be processed; assume the caller already set USBCMD.RS.
  uhci.io_write(REG_FLBASEADD, 4, flBase >>> 0);
  tick1ms(uhci);
}

function controlIn(opts: {
  uhci: { io_write(offset: number, size: number, value: number): void };
  view: DataView;
  guestBase: number;
  alloc: Alloc;
  flBase: number;
  devAddr: number;
  setup: SetupPacket;
}): Uint8Array {
  const { uhci, view, guestBase, alloc, flBase, devAddr, setup } = opts;

  const qhAddr = alloc.alloc(0x20, 0x10);
  const setupBuf = alloc.alloc(8, 0x10);
  const dataBuf = alloc.alloc(setup.wLength, 0x10);
  const setupTd = alloc.alloc(0x20, 0x10);
  const dataTd = alloc.alloc(0x20, 0x10);
  const statusTd = alloc.alloc(0x20, 0x10);

  const bytes = encodeSetupPacket(setup);
  new Uint8Array(view.buffer).set(bytes, guestBase + setupBuf);

  writeTd(view, guestBase, setupTd, dataTd, tdCtrl(true, false), tdToken(PID_SETUP, devAddr, 0, false, 8), setupBuf);
  writeTd(
    view,
    guestBase,
    dataTd,
    statusTd,
    tdCtrl(true, false),
    tdToken(PID_IN, devAddr, 0, true, setup.wLength),
    dataBuf,
  );
  writeTd(view, guestBase, statusTd, LINK_PTR_T, tdCtrl(true, true), tdToken(PID_OUT, devAddr, 0, true, 0), 0);
  writeQh(view, guestBase, qhAddr, LINK_PTR_T, setupTd);
  installFrameList(view, guestBase, flBase, qhAddr);

  uhci.io_write(REG_FLBASEADD, 4, flBase >>> 0);
  tick1ms(uhci);

  const ctrlSts = view.getUint32(guestBase + dataTd + 4, true);
  expect(ctrlSts & TD_CTRL_ACTIVE).toBe(0);
  const gotLen = actlen(ctrlSts);
  const out = new Uint8Array(gotLen);
  out.set(new Uint8Array(view.buffer).subarray(guestBase + dataBuf, guestBase + dataBuf + gotLen));
  return out;
}

function interruptIn(opts: {
  uhci: { io_write(offset: number, size: number, value: number): void };
  view: DataView;
  guestBase: number;
  alloc: Alloc;
  flBase: number;
  devAddr: number;
  ep: number;
  len: number;
}): Uint8Array {
  const { uhci, view, guestBase, alloc, flBase, devAddr, ep, len } = opts;
  const qhAddr = alloc.alloc(0x20, 0x10);
  const buf = alloc.alloc(len, 0x10);
  const td = alloc.alloc(0x20, 0x10);

  writeTd(view, guestBase, td, LINK_PTR_T, tdCtrl(true, true), tdToken(PID_IN, devAddr, ep, true, len), buf);
  writeQh(view, guestBase, qhAddr, LINK_PTR_T, td);
  installFrameList(view, guestBase, flBase, qhAddr);

  uhci.io_write(REG_FLBASEADD, 4, flBase >>> 0);
  tick1ms(uhci);

  const ctrlSts = view.getUint32(guestBase + td + 4, true);
  expect(ctrlSts & TD_CTRL_ACTIVE).toBe(0);
  const gotLen = actlen(ctrlSts);
  const out = new Uint8Array(gotLen);
  out.set(new Uint8Array(view.buffer).subarray(guestBase + buf, guestBase + buf + gotLen));
  return out;
}

describe("usb/UHCI synthetic HID passthrough integration (WASM)", () => {
  it("delivers UsbHidBridge reports through UsbHidPassthroughBridge via UHCI + external hub", async () => {
    const desiredGuestBytes = 2 * 1024 * 1024;
    const layoutHint = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layoutHint.wasm_pages, maximum: layoutHint.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "uhci_synthetic_hid_integration.test" });
    if (!api.UhciControllerBridge) return;
    if (!api.UsbHidPassthroughBridge) return;

    const layout = api.guest_ram_layout(desiredGuestBytes);
    const guestBase = layout.guest_base >>> 0;
    const guestSize = layout.guest_size >>> 0;
    expect(guestBase).toBeGreaterThan(0);
    expect(guestSize).toBeGreaterThan(0x20000);

    const UhciCtor = api.UhciControllerBridge as unknown as { new (...args: number[]): any; length: number };
    let uhci: any;
    try {
      uhci = UhciCtor.length >= 2 ? new UhciCtor(guestBase, guestSize) : new UhciCtor(guestBase);
    } catch (err) {
      // Older wasm-bindgen outputs might enforce arity. Retry with the opposite signature.
      uhci = UhciCtor.length >= 2 ? new UhciCtor(guestBase) : new UhciCtor(guestBase, guestSize);
    }

    // `UhciControllerBridge.step_frame/step_frames` gates DMA behind PCI command bit2 (Bus Master
    // Enable). These unit tests drive the bridge directly (without the PCI bus wrapper), so mirror
    // what a real guest would do by enabling bus mastering here.
    if (typeof uhci.set_pci_command === "function") {
      try {
        uhci.set_pci_command(0x0004);
      } catch {
        // ignore
      }
    }

    if (typeof uhci.attach_hub !== "function" || typeof uhci.attach_usb_hid_passthrough_device !== "function") return;

    // Root port 0: external hub with enough ports for synthetic devices.
    uhci.attach_hub(EXTERNAL_HUB_ROOT_PORT, DEFAULT_EXTERNAL_HUB_PORT_COUNT);

    const HidBridge = api.UsbHidPassthroughBridge;
    const keyboardDev = new HidBridge(
      0x1234,
      0x0001,
      "Aero",
      "Keyboard",
      undefined,
      USB_HID_BOOT_KEYBOARD_REPORT_DESCRIPTOR,
      false,
      USB_HID_INTERFACE_SUBCLASS_BOOT,
      USB_HID_INTERFACE_PROTOCOL_KEYBOARD,
    );
    const mouseDev = new HidBridge(
      0x1234,
      0x0002,
      "Aero",
      "Mouse",
      undefined,
      USB_HID_BOOT_MOUSE_REPORT_DESCRIPTOR,
      false,
      USB_HID_INTERFACE_SUBCLASS_BOOT,
      USB_HID_INTERFACE_PROTOCOL_MOUSE,
    );
    const gamepadDev = new HidBridge(0x1234, 0x0003, "Aero", "Gamepad", undefined, USB_HID_GAMEPAD_REPORT_DESCRIPTOR, false);
    const consumerDev = new HidBridge(
      0x1234,
      0x0004,
      "Aero",
      "Consumer Control",
      undefined,
      USB_HID_CONSUMER_CONTROL_REPORT_DESCRIPTOR,
      false,
    );

    uhci.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT], keyboardDev);
    uhci.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT], mouseDev);
    uhci.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT], gamepadDev);
    uhci.attach_usb_hid_passthrough_device(
      [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT],
      consumerDev,
    );

    // Minimal guest memory map for UHCI TD/QH traversal.
    const view = new DataView(memory.buffer);
    const alloc = new Alloc(0x2000);
    const flBase = 0x1000;

    // Enable IOC interrupts (not strictly required for this test, but mirrors real guests).
    uhci.io_write(REG_USBINTR, 2, USBINTR_IOC);

    // Reset root port 0 and wait the UHCI-mandated ~50ms so the hub becomes enabled.
    uhci.io_write(REG_PORTSC1, 2, PORTSC_PR);
    for (let i = 0; i < 50; i += 1) tick1ms(uhci);

    // Start the controller (RS) with 64-byte max packet size.
    uhci.io_write(REG_USBCMD, 2, USBCMD_RS | USBCMD_MAXP);

    // Enumerate + configure the hub itself at address 0 -> 1.
    controlNoData({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 0,
      setup: { bmRequestType: 0x00, bRequest: 0x05, wValue: 1, wIndex: 0, wLength: 0 },
    });
    controlNoData({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 1,
      setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 1, wIndex: 0, wLength: 0 },
    });

    // Hub class requests.
    const setupHubSetPortFeature = (port: number, feature: number): SetupPacket => ({
      bmRequestType: 0x23,
      bRequest: 0x03,
      wValue: feature,
      wIndex: port,
      wLength: 0,
    });
    const setupHubClearPortFeature = (port: number, feature: number): SetupPacket => ({
      bmRequestType: 0x23,
      bRequest: 0x01,
      wValue: feature,
      wIndex: port,
      wLength: 0,
    });

    // Power+reset synthetic device ports and enumerate each device.
    const ports = [
      UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT,
      UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT,
      UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT,
      UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT,
    ];
    const addrs = [5, 6, 7, 8];
    expect(ports).toHaveLength(UHCI_SYNTHETIC_HID_HUB_PORT_COUNT);
    for (let i = 0; i < ports.length; i += 1) {
      const port = ports[i]!;
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubSetPortFeature(port, 8) }); // PORT_POWER
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubSetPortFeature(port, 4) }); // PORT_RESET
      for (let j = 0; j < 50; j += 1) tick1ms(uhci);

      // Clear change bits.
      for (const feature of [20, 16, 17]) {
        controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubClearPortFeature(port, feature) });
      }

      const addr = addrs[i]!;
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 0, setup: { bmRequestType: 0x00, bRequest: 0x05, wValue: addr, wIndex: 0, wLength: 0 } });
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: addr, setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 1, wIndex: 0, wLength: 0 } });
    }

    expect(keyboardDev.configured()).toBe(true);
    expect(mouseDev.configured()).toBe(true);
    expect(gamepadDev.configured()).toBe(true);
    expect(consumerDev.configured()).toBe(true);

    const usbHid = new api.UsbHidBridge();

    usbHid.keyboard_event(0x04, true); // KeyA
    const kbReport = usbHid.drain_next_keyboard_report();
    expect(kbReport).toBeInstanceOf(Uint8Array);
    keyboardDev.push_input_report(0, kbReport!);

    usbHid.mouse_move(10, 5);
    const mouseReport = usbHid.drain_next_mouse_report();
    expect(mouseReport).toBeInstanceOf(Uint8Array);
    mouseDev.push_input_report(0, mouseReport!);

    usbHid.gamepad_report(0x04030201, 0x00070605);
    const padReport = usbHid.drain_next_gamepad_report();
    expect(padReport).toBeInstanceOf(Uint8Array);
    gamepadDev.push_input_report(0, padReport!);

    // Consumer Control report format: 2 bytes, little-endian u16 usage ID (Usage Page 0x0C).
    // Newer WASM builds can generate this via `UsbHidBridge.consumer_event`; fall back to
    // constructing the report bytes manually for back-compat.
    let consumerReport: Uint8Array | null = null;
    try {
      usbHid.consumer_event?.(0x00e9, true); // Volume Up
      consumerReport = usbHid.drain_next_consumer_report?.() ?? null;
    } catch {
      consumerReport = null;
    }
    if (!(consumerReport instanceof Uint8Array)) {
      consumerReport = new Uint8Array([0xe9, 0x00]);
    }
    consumerDev.push_input_report(0, consumerReport);

    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[0]!, ep: 1, len: 8 })).toEqual(kbReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[1]!, ep: 1, len: 5 })).toEqual(mouseReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[2]!, ep: 1, len: 8 })).toEqual(padReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[3]!, ep: 1, len: 2 })).toEqual(consumerReport);
  });

  it("preserves externally attached UsbHidPassthroughBridge devices across UhciRuntime.load_state", async () => {
    const desiredGuestBytes = 2 * 1024 * 1024;
    const layoutHint = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layoutHint.wasm_pages, maximum: layoutHint.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "uhci_synthetic_hid_integration.test (load_state)" });
    if (!api.UhciRuntime) return;
    if (!api.UsbHidPassthroughBridge) return;

    const layout = api.guest_ram_layout(desiredGuestBytes);
    const guestBase = layout.guest_base >>> 0;
    const guestSize = layout.guest_size >>> 0;
    expect(guestBase).toBeGreaterThan(0);
    expect(guestSize).toBeGreaterThan(0x20000);

    const runtime = new api.UhciRuntime(guestBase, guestSize);
    if (typeof runtime.attach_usb_hid_passthrough_device !== "function") return;
    const save =
      (runtime as unknown as { save_state?: unknown }).save_state ?? (runtime as unknown as { snapshot_state?: unknown }).snapshot_state;
    const load =
      (runtime as unknown as { load_state?: unknown }).load_state ?? (runtime as unknown as { restore_state?: unknown }).restore_state;
    if (typeof save !== "function" || typeof load !== "function") return;

    const HidBridge = api.UsbHidPassthroughBridge;
    const keyboardDev = new HidBridge(
      0x1234,
      0x0001,
      "Aero",
      "Keyboard",
      undefined,
      USB_HID_BOOT_KEYBOARD_REPORT_DESCRIPTOR,
      false,
      USB_HID_INTERFACE_SUBCLASS_BOOT,
      USB_HID_INTERFACE_PROTOCOL_KEYBOARD,
    );
    const mouseDev = new HidBridge(
      0x1234,
      0x0002,
      "Aero",
      "Mouse",
      undefined,
      USB_HID_BOOT_MOUSE_REPORT_DESCRIPTOR,
      false,
      USB_HID_INTERFACE_SUBCLASS_BOOT,
      USB_HID_INTERFACE_PROTOCOL_MOUSE,
    );
    const gamepadDev = new HidBridge(0x1234, 0x0003, "Aero", "Gamepad", undefined, USB_HID_GAMEPAD_REPORT_DESCRIPTOR, false);
    const consumerDev = new HidBridge(
      0x1234,
      0x0004,
      "Aero",
      "Consumer Control",
      undefined,
      USB_HID_CONSUMER_CONTROL_REPORT_DESCRIPTOR,
      false,
    );

    runtime.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT], keyboardDev);
    runtime.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT], mouseDev);
    runtime.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT], gamepadDev);
    runtime.attach_usb_hid_passthrough_device(
      [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT],
      consumerDev,
    );

    // Wrap UhciRuntime into the same shape the helper functions expect.
    const uhci: { io_write(offset: number, size: number, value: number): void; step_frame(): void; tick_1ms(): void } = {
      io_write: (offset, size, value) => runtime.port_write(offset >>> 0, size >>> 0, value >>> 0),
      step_frame: () => runtime.step_frame(),
      tick_1ms: () => runtime.tick_1ms(),
    };

    const view = new DataView(memory.buffer);
    const alloc = new Alloc(0x2000);
    const flBase = 0x1000;

    // Enable IOC interrupts.
    uhci.io_write(REG_USBINTR, 2, USBINTR_IOC);

    // Reset root port 0 and wait the UHCI-mandated ~50ms so the hub becomes enabled.
    uhci.io_write(REG_PORTSC1, 2, PORTSC_PR);
    for (let i = 0; i < 50; i += 1) tick1ms(uhci);

    // Start the controller (RS) with 64-byte max packet size.
    uhci.io_write(REG_USBCMD, 2, USBCMD_RS | USBCMD_MAXP);

    // Enumerate + configure the hub itself at address 0 -> 1.
    controlNoData({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 0,
      setup: { bmRequestType: 0x00, bRequest: 0x05, wValue: 1, wIndex: 0, wLength: 0 },
    });
    controlNoData({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 1,
      setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 1, wIndex: 0, wLength: 0 },
    });

    const setupHubSetPortFeature = (port: number, feature: number): SetupPacket => ({
      bmRequestType: 0x23,
      bRequest: 0x03,
      wValue: feature,
      wIndex: port,
      wLength: 0,
    });
    const setupHubClearPortFeature = (port: number, feature: number): SetupPacket => ({
      bmRequestType: 0x23,
      bRequest: 0x01,
      wValue: feature,
      wIndex: port,
      wLength: 0,
    });

    const addrs = [5, 6, 7, 8];
    for (let port = 1; port <= UHCI_SYNTHETIC_HID_HUB_PORT_COUNT; port += 1) {
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubSetPortFeature(port, 8) }); // PORT_POWER
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubSetPortFeature(port, 4) }); // PORT_RESET
      for (let i = 0; i < 50; i += 1) tick1ms(uhci);

      for (const feature of [20, 16, 17]) {
        controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubClearPortFeature(port, feature) });
      }

      const addr = addrs[port - 1]!;
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 0, setup: { bmRequestType: 0x00, bRequest: 0x05, wValue: addr, wIndex: 0, wLength: 0 } });
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: addr, setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 1, wIndex: 0, wLength: 0 } });
    }

    expect(keyboardDev.configured()).toBe(true);
    expect(mouseDev.configured()).toBe(true);
    expect(gamepadDev.configured()).toBe(true);
    expect(consumerDev.configured()).toBe(true);

    const snapshotBytes = save.call(runtime) as unknown;
    expect(snapshotBytes).toBeInstanceOf(Uint8Array);

    // Mutate passthrough device state after the snapshot so we can verify restore rolls it back.
    // (Without explicit snapshotting of `UsbHidPassthroughHandle` state, configuration would stay 0.)
    for (const addr of addrs) {
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: addr, setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 0, wIndex: 0, wLength: 0 } });
    }
    expect(keyboardDev.configured()).toBe(false);
    expect(mouseDev.configured()).toBe(false);
    expect(gamepadDev.configured()).toBe(false);
    expect(consumerDev.configured()).toBe(false);

    load.call(runtime, snapshotBytes);

    // The runtime should preserve externally attached devices during snapshot restore so they remain
    // connected to the JS-side `UsbHidPassthroughBridge` handles.
    expect(keyboardDev.configured()).toBe(true);
    expect(mouseDev.configured()).toBe(true);
    expect(gamepadDev.configured()).toBe(true);
    expect(consumerDev.configured()).toBe(true);

    const usbHid = new api.UsbHidBridge();
    usbHid.keyboard_event(0x04, true); // KeyA
    const kbReport = usbHid.drain_next_keyboard_report();
    expect(kbReport).toBeInstanceOf(Uint8Array);
    keyboardDev.push_input_report(0, kbReport!);

    usbHid.mouse_move(10, 5);
    const mouseReport = usbHid.drain_next_mouse_report();
    expect(mouseReport).toBeInstanceOf(Uint8Array);
    mouseDev.push_input_report(0, mouseReport!);

    usbHid.gamepad_report(0x04030201, 0x00070605);
    const padReport = usbHid.drain_next_gamepad_report();
    expect(padReport).toBeInstanceOf(Uint8Array);
    gamepadDev.push_input_report(0, padReport!);

    let consumerReport: Uint8Array | null = null;
    try {
      usbHid.consumer_event?.(0x00e9, true);
      consumerReport = usbHid.drain_next_consumer_report?.() ?? null;
    } catch {
      consumerReport = null;
    }
    if (!(consumerReport instanceof Uint8Array)) {
      consumerReport = new Uint8Array([0xe9, 0x00]);
    }
    consumerDev.push_input_report(0, consumerReport);

    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[0]!, ep: 1, len: 8 })).toEqual(kbReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[1]!, ep: 1, len: 5 })).toEqual(mouseReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[2]!, ep: 1, len: 8 })).toEqual(padReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[3]!, ep: 1, len: 2 })).toEqual(consumerReport);
  });

  it("preserves externally attached UsbHidPassthroughBridge devices across external hub growth", async () => {
    const desiredGuestBytes = 2 * 1024 * 1024;
    const layoutHint = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layoutHint.wasm_pages, maximum: layoutHint.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    if (!api.UhciRuntime) return;
    if (!api.UsbHidPassthroughBridge) return;

    const layout = api.guest_ram_layout(desiredGuestBytes);
    const guestBase = layout.guest_base >>> 0;
    const guestSize = layout.guest_size >>> 0;
    expect(guestBase).toBeGreaterThan(0);
    expect(guestSize).toBeGreaterThan(0x20000);

    const runtime = new api.UhciRuntime(guestBase, guestSize);
    const attach = (runtime as unknown as { attach_usb_hid_passthrough_device?: unknown }).attach_usb_hid_passthrough_device;
    if (typeof attach !== "function") return;
    const grow = (runtime as unknown as { webhid_attach_hub?: unknown }).webhid_attach_hub;
    if (typeof grow !== "function") return;

    const HidBridge = api.UsbHidPassthroughBridge;
    const keyboardDev = new HidBridge(
      0x1234,
      0x0001,
      "Aero",
      "Keyboard",
      undefined,
      USB_HID_BOOT_KEYBOARD_REPORT_DESCRIPTOR,
      false,
      USB_HID_INTERFACE_SUBCLASS_BOOT,
      USB_HID_INTERFACE_PROTOCOL_KEYBOARD,
    );
    const mouseDev = new HidBridge(
      0x1234,
      0x0002,
      "Aero",
      "Mouse",
      undefined,
      USB_HID_BOOT_MOUSE_REPORT_DESCRIPTOR,
      false,
      USB_HID_INTERFACE_SUBCLASS_BOOT,
      USB_HID_INTERFACE_PROTOCOL_MOUSE,
    );
    const gamepadDev = new HidBridge(0x1234, 0x0003, "Aero", "Gamepad", undefined, USB_HID_GAMEPAD_REPORT_DESCRIPTOR, false);
    const consumerDev = new HidBridge(
      0x1234,
      0x0004,
      "Aero",
      "Consumer Control",
      undefined,
      USB_HID_CONSUMER_CONTROL_REPORT_DESCRIPTOR,
      false,
    );

    attach.call(runtime, [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT], keyboardDev);
    attach.call(runtime, [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT], mouseDev);
    attach.call(runtime, [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT], gamepadDev);
    attach.call(runtime, [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT], consumerDev);

    // Force an external hub resize after devices are attached. Without runtime-side bookkeeping,
    // this would disconnect host-managed devices (synthetic keyboard/mouse/gamepad/consumer-control).
    grow.call(runtime, [EXTERNAL_HUB_ROOT_PORT], 32);

    const uhci: { io_write(offset: number, size: number, value: number): void; step_frame(): void; tick_1ms(): void } = {
      io_write: (offset, size, value) => runtime.port_write(offset >>> 0, size >>> 0, value >>> 0),
      step_frame: () => runtime.step_frame(),
      tick_1ms: () => runtime.tick_1ms(),
    };

    const view = new DataView(memory.buffer);
    const alloc = new Alloc(0x2000);
    const flBase = 0x1000;

    uhci.io_write(REG_USBINTR, 2, USBINTR_IOC);

    // Reset root port 0 and wait the UHCI-mandated ~50ms so the hub becomes enabled.
    uhci.io_write(REG_PORTSC1, 2, PORTSC_PR);
    for (let i = 0; i < 50; i += 1) tick1ms(uhci);

    // Start the controller (RS) with 64-byte max packet size.
    uhci.io_write(REG_USBCMD, 2, USBCMD_RS | USBCMD_MAXP);

    // Enumerate + configure the hub itself at address 0 -> 1.
    controlNoData({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 0,
      setup: { bmRequestType: 0x00, bRequest: 0x05, wValue: 1, wIndex: 0, wLength: 0 },
    });
    controlNoData({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 1,
      setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 1, wIndex: 0, wLength: 0 },
    });

    const setupHubSetPortFeature = (port: number, feature: number): SetupPacket => ({
      bmRequestType: 0x23,
      bRequest: 0x03,
      wValue: feature,
      wIndex: port,
      wLength: 0,
    });
    const setupHubClearPortFeature = (port: number, feature: number): SetupPacket => ({
      bmRequestType: 0x23,
      bRequest: 0x01,
      wValue: feature,
      wIndex: port,
      wLength: 0,
    });

    const ports = [
      UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT,
      UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT,
      UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT,
      UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT,
    ];
    const addrs = [5, 6, 7, 8];
    for (let i = 0; i < ports.length; i += 1) {
      const port = ports[i]!;
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubSetPortFeature(port, 8) }); // PORT_POWER
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubSetPortFeature(port, 4) }); // PORT_RESET
      for (let j = 0; j < 50; j += 1) tick1ms(uhci);

      for (const feature of [20, 16, 17]) {
        controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 1, setup: setupHubClearPortFeature(port, feature) });
      }

      const addr = addrs[i]!;
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: 0, setup: { bmRequestType: 0x00, bRequest: 0x05, wValue: addr, wIndex: 0, wLength: 0 } });
      controlNoData({ uhci, view, guestBase, alloc, flBase, devAddr: addr, setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 1, wIndex: 0, wLength: 0 } });
    }

    expect(keyboardDev.configured()).toBe(true);
    expect(mouseDev.configured()).toBe(true);
    expect(gamepadDev.configured()).toBe(true);
    expect(consumerDev.configured()).toBe(true);

    const usbHid = new api.UsbHidBridge();

    usbHid.keyboard_event(0x04, true);
    const kbReport = usbHid.drain_next_keyboard_report();
    expect(kbReport).toBeInstanceOf(Uint8Array);
    keyboardDev.push_input_report(0, kbReport!);

    usbHid.mouse_move(10, 5);
    const mouseReport = usbHid.drain_next_mouse_report();
    expect(mouseReport).toBeInstanceOf(Uint8Array);
    mouseDev.push_input_report(0, mouseReport!);

    usbHid.gamepad_report(0x04030201, 0x00070605);
    const padReport = usbHid.drain_next_gamepad_report();
    expect(padReport).toBeInstanceOf(Uint8Array);
    gamepadDev.push_input_report(0, padReport!);

    let consumerReport: Uint8Array | null = null;
    try {
      usbHid.consumer_event?.(0x00e9, true);
      consumerReport = usbHid.drain_next_consumer_report?.() ?? null;
    } catch {
      consumerReport = null;
    }
    if (!(consumerReport instanceof Uint8Array)) {
      consumerReport = new Uint8Array([0xe9, 0x00]);
    }
    consumerDev.push_input_report(0, consumerReport);

    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[0]!, ep: 1, len: 8 })).toEqual(kbReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[1]!, ep: 1, len: 5 })).toEqual(mouseReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[2]!, ep: 1, len: 8 })).toEqual(padReport);
    expect(interruptIn({ uhci, view, guestBase, alloc, flBase, devAddr: addrs[3]!, ep: 1, len: 2 })).toEqual(consumerReport);
  });

  it("does not resurrect extra UsbHidPassthroughBridge devices not present in the snapshot after restore + hub reset", async () => {
    const desiredGuestBytes = 2 * 1024 * 1024;
    const layoutHint = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layoutHint.wasm_pages, maximum: layoutHint.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "uhci_synthetic_hid_integration.test (snapshot extra device)" });
    if (!api.UhciRuntime) return;
    if (!api.UsbHidPassthroughBridge) return;

    const layout = api.guest_ram_layout(desiredGuestBytes);
    const guestBase = layout.guest_base >>> 0;
    const guestSize = layout.guest_size >>> 0;
    expect(guestBase).toBeGreaterThan(0);
    expect(guestSize).toBeGreaterThan(0x20000);

    const runtime = new api.UhciRuntime(guestBase, guestSize);
    if (typeof runtime.attach_usb_hid_passthrough_device !== "function") return;
    const save =
      (runtime as unknown as { save_state?: unknown }).save_state ?? (runtime as unknown as { snapshot_state?: unknown }).snapshot_state;
    const load =
      (runtime as unknown as { load_state?: unknown }).load_state ?? (runtime as unknown as { restore_state?: unknown }).restore_state;
    if (typeof save !== "function" || typeof load !== "function") return;
    const grow = (runtime as unknown as { webhid_attach_hub?: unknown }).webhid_attach_hub;

    const HidBridge = api.UsbHidPassthroughBridge;
    const keyboardDev = new HidBridge(
      0x1234,
      0x0001,
      "Aero",
      "Keyboard",
      undefined,
      USB_HID_BOOT_KEYBOARD_REPORT_DESCRIPTOR,
      false,
      USB_HID_INTERFACE_SUBCLASS_BOOT,
      USB_HID_INTERFACE_PROTOCOL_KEYBOARD,
    );
    const mouseDev = new HidBridge(
      0x1234,
      0x0002,
      "Aero",
      "Mouse",
      undefined,
      USB_HID_BOOT_MOUSE_REPORT_DESCRIPTOR,
      false,
      USB_HID_INTERFACE_SUBCLASS_BOOT,
      USB_HID_INTERFACE_PROTOCOL_MOUSE,
    );
    const gamepadDev = new HidBridge(0x1234, 0x0003, "Aero", "Gamepad", undefined, USB_HID_GAMEPAD_REPORT_DESCRIPTOR, false);
    const consumerDev = new HidBridge(
      0x1234,
      0x0004,
      "Aero",
      "Consumer Control",
      undefined,
      USB_HID_CONSUMER_CONTROL_REPORT_DESCRIPTOR,
      false,
    );

    runtime.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT], keyboardDev);
    runtime.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT], mouseDev);
    runtime.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT], gamepadDev);
    runtime.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT], consumerDev);

    const snapshotBytes = save.call(runtime) as unknown;
    expect(snapshotBytes).toBeInstanceOf(Uint8Array);

    // Attach an extra passthrough device after the snapshot is taken. The runtime will retain the
    // handle across restores, but it should not reattach it unless the snapshot includes it.
    const extraDev = new HidBridge(0x1234, 0x0005, "Aero", "Extra", undefined, USB_HID_GAMEPAD_REPORT_DESCRIPTOR, false);
    runtime.attach_usb_hid_passthrough_device([EXTERNAL_HUB_ROOT_PORT, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT], extraDev);

    load.call(runtime, snapshotBytes);

    // Grow the external hub after restore to ensure passthrough bookkeeping doesn't reattach
    // devices that were not present in the snapshot.
    if (typeof grow === "function") {
      grow.call(runtime, [EXTERNAL_HUB_ROOT_PORT], 32);
    }

    const uhci: { io_write(offset: number, size: number, value: number): void; step_frame(): void; tick_1ms(): void } = {
      io_write: (offset, size, value) => runtime.port_write(offset >>> 0, size >>> 0, value >>> 0),
      step_frame: () => runtime.step_frame(),
      tick_1ms: () => runtime.tick_1ms(),
    };

    const view = new DataView(memory.buffer);
    const alloc = new Alloc(0x2000);
    const flBase = 0x1000;

    // Trigger an upstream bus reset of the hub via the UHCI root port. A buggy restore path can
    // resurrect devices that were not present in the snapshot when the hub recomputes
    // port.connected from port.device.is_some().
    uhci.io_write(REG_PORTSC1, 2, PORTSC_PR);
    for (let i = 0; i < 50; i += 1) tick1ms(uhci);

    uhci.io_write(REG_USBCMD, 2, USBCMD_RS | USBCMD_MAXP);

    // Enumerate + configure the hub at address 0 -> 1 after the bus reset.
    controlNoData({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 0,
      setup: { bmRequestType: 0x00, bRequest: 0x05, wValue: 1, wIndex: 0, wLength: 0 },
    });
    controlNoData({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 1,
      setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 1, wIndex: 0, wLength: 0 },
    });

    // Hub class request: GetPortStatus (USB 2.0 hub spec 11.24.2.7).
    const portStatus = controlIn({
      uhci,
      view,
      guestBase,
      alloc,
      flBase,
      devAddr: 1,
      setup: { bmRequestType: 0xa3, bRequest: 0x00, wValue: 0, wIndex: UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT, wLength: 4 },
    });
    expect(portStatus.length).toBe(4);
    const st = portStatus[0]! | (portStatus[1]! << 8);
    expect(st & 0x0001).toBe(0);
  });

  it("rejects nested USB topology paths for UhciRuntime.attach_usb_hid_passthrough_device", async () => {
    const desiredGuestBytes = 2 * 1024 * 1024;
    const layoutHint = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layoutHint.wasm_pages, maximum: layoutHint.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "uhci_synthetic_hid_integration.test (nested path)" });
    if (!api.UhciRuntime) return;
    if (!api.UsbHidPassthroughBridge) return;

    const layout = api.guest_ram_layout(desiredGuestBytes);
    const guestBase = layout.guest_base >>> 0;
    const guestSize = layout.guest_size >>> 0;

    const runtime = new api.UhciRuntime(guestBase, guestSize);
    // TypeScript doesn't narrow optional methods across closures; capture a stable function handle.
    const attach = runtime.attach_usb_hid_passthrough_device;
    if (typeof attach !== "function") return;

    const HidBridge = api.UsbHidPassthroughBridge;
    const dev = new HidBridge(0x1234, 0x0004, "Aero", "Nested", undefined, USB_HID_GAMEPAD_REPORT_DESCRIPTOR, false);

    expect(() => attach.call(runtime, [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT, 1], dev)).toThrow();
  });

  it("rejects UsbHidPassthroughBridge attachment onto a WebHID-occupied external hub port", async () => {
    const desiredGuestBytes = 2 * 1024 * 1024;
    const layoutHint = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layoutHint.wasm_pages, maximum: layoutHint.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes("Missing single-thread WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "uhci_synthetic_hid_integration.test (webhid collision)" });
    if (!api.UhciRuntime) return;
    if (!api.UsbHidPassthroughBridge) return;

    const layout = api.guest_ram_layout(desiredGuestBytes);
    const guestBase = layout.guest_base >>> 0;
    const guestSize = layout.guest_size >>> 0;

    const runtime = new api.UhciRuntime(guestBase, guestSize);
    // TypeScript doesn't narrow optional methods across closures; capture a stable function handle.
    const attach = runtime.attach_usb_hid_passthrough_device;
    if (typeof attach !== "function") return;
    if (typeof (runtime as unknown as { webhid_attach_at_path?: unknown }).webhid_attach_at_path !== "function") return;

    const collections = [
      {
        usagePage: 0x01,
        usage: 0x02,
        collectionType: 1,
        children: [],
        inputReports: [
          {
            reportId: 0,
            items: [
              {
                usagePage: 0x01,
                usages: [0x30],
                usageMinimum: 0,
                usageMaximum: 0,
                reportSize: 8,
                reportCount: 1,
                unitExponent: 0,
                unit: 0,
                logicalMinimum: 0,
                logicalMaximum: 127,
                physicalMinimum: 0,
                physicalMaximum: 0,
                strings: [],
                stringMinimum: 0,
                stringMaximum: 0,
                designators: [],
                designatorMinimum: 0,
                designatorMaximum: 0,
                isAbsolute: true,
                isArray: false,
                isBufferedBytes: false,
                isConstant: false,
                isLinear: true,
                isRange: false,
                isRelative: false,
                isVolatile: false,
                hasNull: false,
                hasPreferredState: true,
                isWrapped: false,
              },
            ],
          },
        ],
        outputReports: [],
        featureReports: [],
      },
    ];

    // Attach a WebHID-backed device at the first dynamic hub port (avoid the synthetic HID range).
    (runtime as unknown as { webhid_attach_at_path: (...args: unknown[]) => void }).webhid_attach_at_path(
      1,
      0x1234,
      0x0001,
      "WebHID",
      collections as unknown,
      [EXTERNAL_HUB_ROOT_PORT, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT],
    );

    const HidBridge = api.UsbHidPassthroughBridge;
    const dev = new HidBridge(0x1234, 0x0002, "Aero", "Collision", undefined, USB_HID_GAMEPAD_REPORT_DESCRIPTOR, false);
    expect(() => attach.call(runtime, [EXTERNAL_HUB_ROOT_PORT, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT], dev)).toThrow();
  });
});
