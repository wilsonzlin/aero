import { describe, expect, it } from "vitest";
import { vi } from "vitest";

import type { WasmApi } from "../runtime/wasm_loader";
import { serializeRuntimeDiskSnapshot } from "../storage/runtime_disk_snapshot";
import { restoreIoWorkerVmSnapshotFromOpfs, saveIoWorkerVmSnapshotToOpfs, restoreUsbDeviceState } from "./io_worker_vm_snapshot";
import {
  decodeUsbSnapshotContainer,
  encodeUsbSnapshotContainer,
  USB_SNAPSHOT_TAG_EHCI,
  USB_SNAPSHOT_TAG_UHCI,
  USB_SNAPSHOT_TAG_XHCI,
} from "./usb_snapshot_container";
import {
  IO_WORKER_RUNTIME_DISK_SNAPSHOT_KIND,
  appendRuntimeDiskWorkerSnapshotDeviceBlob,
  restoreRuntimeDiskWorkerSnapshotFromDeviceBlobs,
} from "./io_worker_runtime_disk_snapshot";
import { pauseIoWorkerSnapshotAndDrainDiskIo } from "./io_worker_snapshot_pause";
import {
  VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA,
  VM_SNAPSHOT_DEVICE_ID_AUDIO_VIRTIO_SND,
  VM_SNAPSHOT_DEVICE_ID_E1000,
  VM_SNAPSHOT_DEVICE_ID_I8042,
  VM_SNAPSHOT_DEVICE_ID_NET_STACK,
  VM_SNAPSHOT_DEVICE_ID_USB,
  VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID,
  VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND,
} from "./vm_snapshot_wasm";

const VM_SNAPSHOT_DEVICE_PCI_CFG_KIND = `${VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID}14`;
const VM_SNAPSHOT_DEVICE_PCI_LEGACY_KIND = `${VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID}5`;
const VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_ID_KIND = `${VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID}24`;
function makeAeroIoSnapshotHeader(tag: string): Uint8Array {
  const bytes = new Uint8Array(16);
  bytes[0] = 0x41;
  bytes[1] = 0x45;
  bytes[2] = 0x52;
  bytes[3] = 0x4f;
  const padded = (tag + "____").slice(0, 4);
  bytes[8] = padded.charCodeAt(0) & 0xff;
  bytes[9] = padded.charCodeAt(1) & 0xff;
  bytes[10] = padded.charCodeAt(2) & 0xff;
  bytes[11] = padded.charCodeAt(3) & 0xff;
  // major=1 minor=0
  bytes[12] = 0x01;
  bytes[13] = 0x00;
  bytes[14] = 0x00;
  bytes[15] = 0x00;
  return bytes;
}

const VINP_TAG_KEYBOARD = 1;
const VINP_TAG_MOUSE = 2;

function encodeVinpSnapshot(entries: Array<{ tag: number; bytes: Uint8Array }>): Uint8Array {
  const sorted = [...entries].sort((a, b) => a.tag - b.tag);
  let total = 16;
  for (const e of sorted) total += 6 + e.bytes.byteLength;
  const out = new Uint8Array(total);
  // magic "AERO"
  out[0] = 0x41;
  out[1] = 0x45;
  out[2] = 0x52;
  out[3] = 0x4f;
  // format version 1.0
  out[4] = 0x01;
  out[5] = 0x00;
  out[6] = 0x00;
  out[7] = 0x00;
  // device id "VINP"
  out[8] = 0x56;
  out[9] = 0x49;
  out[10] = 0x4e;
  out[11] = 0x50;
  // device version 1.0
  out[12] = 0x01;
  out[13] = 0x00;
  out[14] = 0x00;
  out[15] = 0x00;

  let off = 16;
  for (const e of sorted) {
    const tag = e.tag >>> 0;
    const len = e.bytes.byteLength >>> 0;
    out[off] = tag & 0xff;
    out[off + 1] = (tag >>> 8) & 0xff;
    out[off + 2] = len & 0xff;
    out[off + 3] = (len >>> 8) & 0xff;
    out[off + 4] = (len >>> 16) & 0xff;
    out[off + 5] = (len >>> 24) & 0xff;
    off += 6;
    out.set(e.bytes, off);
    off += len;
  }
  return out;
}

function decodeVinpSnapshot(bytes: Uint8Array): Map<number, Uint8Array> {
  if (bytes.byteLength < 16) throw new Error("VINP snapshot too small");
  if (!(bytes[0] === 0x41 && bytes[1] === 0x45 && bytes[2] === 0x52 && bytes[3] === 0x4f)) {
    throw new Error("VINP snapshot missing AERO magic");
  }
  if (!(bytes[8] === 0x56 && bytes[9] === 0x49 && bytes[10] === 0x4e && bytes[11] === 0x50)) {
    throw new Error("VINP snapshot missing VINP device id");
  }

  const out = new Map<number, Uint8Array>();
  let off = 16;
  while (off < bytes.byteLength) {
    if (off + 6 > bytes.byteLength) throw new Error("VINP snapshot truncated");
    const tag = (bytes[off]! | (bytes[off + 1]! << 8)) >>> 0;
    const len =
      (bytes[off + 2]! | (bytes[off + 3]! << 8) | (bytes[off + 4]! << 16) | (bytes[off + 5]! << 24)) >>> 0;
    off += 6;
    const end = off + len;
    if (!Number.isSafeInteger(end) || end < off || end > bytes.byteLength) throw new Error("VINP snapshot field out of bounds");
    if (out.has(tag)) throw new Error("VINP snapshot duplicate tag");
    out.set(tag, bytes.subarray(off, end));
    off = end;
  }
  return out;
}

describe("snapshot usb: workers/io_worker_vm_snapshot", () => {
  it("forwards device blobs to vm_snapshot_save_to_opfs when save_state hooks exist", async () => {
    const calls: Array<{ path: string; cpu: Uint8Array; mmu: Uint8Array; devices: unknown }> = [];
    const api = {
      vm_snapshot_save_to_opfs: (path: string, cpu: Uint8Array, mmu: Uint8Array, devices: unknown) => {
        calls.push({ path, cpu, mmu, devices });
      },
    } as unknown as WasmApi;

    const usbState = new Uint8Array([0x01, 0x02]);
    const i8042State = new Uint8Array([0x02]);
    const virtioKeyboardState = new Uint8Array([0x03]);
    const virtioMouseState = new Uint8Array([0x04]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const virtioSndState = new Uint8Array([0x03, 0x03, 0x03]);
    const pciState = new Uint8Array([0x80, 0x81]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const usbUhciRuntime = { save_state: () => usbState };
    const i8042 = { save_state: () => i8042State };
    const virtioInputKeyboard = { save_state: () => virtioKeyboardState };
    const virtioInputMouse = { save_state: () => virtioMouseState };
    const audioHda = { save_state: () => hdaState };
    const audioVirtioSnd = { saveState: () => virtioSndState };
    const pciBus = { saveState: () => pciState };
    const netE1000 = { save_state: () => e1000State };
    // Exercise the alternate `snapshot_state` spelling.
    const netStack = { snapshot_state: () => stackState };

    const cpu = new ArrayBuffer(4);
    const mmu = new ArrayBuffer(8);

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/test.snap",
      cpu,
      mmu,
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime,
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        i8042,
        virtioInputKeyboard,
        virtioInputMouse,
        audioHda,
        audioVirtioSnd,
        pciBus,
        netE1000,
        netStack,
      },
    });

    expect(calls).toHaveLength(1);
    expect(calls[0]!.path).toBe("state/test.snap");
    expect(calls[0]!.cpu).toBeInstanceOf(Uint8Array);
    expect(calls[0]!.mmu).toBeInstanceOf(Uint8Array);

    // The IO worker should forward device blobs as an array of `{ kind, bytes: Uint8Array }`.
    // Note: for free-function wasm exports we use a `device.<id>` kind spelling so newer device
    // blobs can still roundtrip through older bindings.
    const devices = calls[0]!.devices as Array<{ kind: string; bytes: Uint8Array }>;
    expect(devices.map((d) => d.kind)).toEqual([
      `device.${VM_SNAPSHOT_DEVICE_ID_USB}`,
      `device.${VM_SNAPSHOT_DEVICE_ID_I8042}`,
      VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_ID_KIND,
      `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA}`,
      `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_VIRTIO_SND}`,
      VM_SNAPSHOT_DEVICE_PCI_CFG_KIND,
      `device.${VM_SNAPSHOT_DEVICE_ID_E1000}`,
      `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`,
    ]);

    expect(devices.find((d) => d.kind === `device.${VM_SNAPSHOT_DEVICE_ID_USB}`)?.bytes).toBe(usbState);
    expect(devices.find((d) => d.kind === `device.${VM_SNAPSHOT_DEVICE_ID_I8042}`)?.bytes).toBe(i8042State);
    expect(devices.find((d) => d.kind === `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA}`)?.bytes).toBe(hdaState);
    expect(devices.find((d) => d.kind === `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_VIRTIO_SND}`)?.bytes).toBe(virtioSndState);
    expect(devices.find((d) => d.kind === VM_SNAPSHOT_DEVICE_PCI_CFG_KIND)?.bytes).toBe(pciState);
    expect(devices.find((d) => d.kind === `device.${VM_SNAPSHOT_DEVICE_ID_E1000}`)?.bytes).toBe(e1000State);
    expect(devices.find((d) => d.kind === `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`)?.bytes).toBe(stackState);

    const vinp = devices.find((d) => d.kind === VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_ID_KIND);
    expect(vinp).not.toBeNull();
    const fields = decodeVinpSnapshot(vinp!.bytes);
    expect(fields.get(VINP_TAG_KEYBOARD)).toEqual(virtioKeyboardState);
    expect(fields.get(VINP_TAG_MOUSE)).toEqual(virtioMouseState);
  });

  it("snapshots UHCI + xHCI as a single USB container (no duplicate USB entries)", async () => {
    const calls: Array<{ devices: unknown }> = [];
    const api = {
      vm_snapshot_save_to_opfs: (_path: string, _cpu: Uint8Array, _mmu: Uint8Array, devices: unknown) => {
        calls.push({ devices });
      },
    } as unknown as WasmApi;

    const xhciState = new Uint8Array([0xaa, 0xbb]);
    const uhciState = new Uint8Array([0x01, 0x02]);
    const usbXhciControllerBridge = { save_state: () => xhciState };
    const usbUhciRuntime = { save_state: () => uhciState };

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/test.snap",
      cpu: new ArrayBuffer(4),
      mmu: new ArrayBuffer(8),
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge,
        usbUhciRuntime,
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
    });

    expect(calls).toHaveLength(1);
    const devices = calls[0]!.devices as Array<{ kind: string; bytes: Uint8Array }>;
    expect(devices).toHaveLength(1);
    expect(devices[0]!.kind).toBe(`device.${VM_SNAPSHOT_DEVICE_ID_USB}`);

    const decoded = decodeUsbSnapshotContainer(devices[0]!.bytes);
    expect(decoded).not.toBeNull();
    expect(decoded!.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_XHCI)?.bytes).toEqual(xhciState);
    expect(decoded!.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_UHCI)?.bytes).toEqual(uhciState);
  });

  it("normalizes device.<id> kinds on restore and applies net.stack TCP restore policy=drop", async () => {
    const usbState = new Uint8Array([0x01, 0x02]);
    const i8042State = new Uint8Array([0x02]);
    const virtioKeyboardState = new Uint8Array([0x03]);
    const virtioMouseState = new Uint8Array([0x04]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const virtioSndState = new Uint8Array([0x03, 0x03, 0x03]);
    const pciState = new Uint8Array([0x80, 0x81]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);
    const virtioInputState = encodeVinpSnapshot([
      { tag: VINP_TAG_KEYBOARD, bytes: virtioKeyboardState },
      { tag: VINP_TAG_MOUSE, bytes: virtioMouseState },
    ]);

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      devices: [
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_USB}`, bytes: usbState },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_I8042}`, bytes: i8042State },
        { kind: VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_ID_KIND, bytes: virtioInputState },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA}`, bytes: hdaState },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_VIRTIO_SND}`, bytes: virtioSndState },
        { kind: VM_SNAPSHOT_DEVICE_PCI_CFG_KIND, bytes: pciState },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_E1000}`, bytes: e1000State },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`, bytes: stackState },
      ],
    }));

    const api = { vm_snapshot_restore_from_opfs: restore } as unknown as WasmApi;

    const usbLoad = vi.fn();
    const i8042Load = vi.fn();
    const virtioKeyboardLoad = vi.fn();
    const virtioMouseLoad = vi.fn();
    const hdaLoad = vi.fn();
    const virtioSndLoad = vi.fn();
    const pciLoad = vi.fn();
    const e1000Load = vi.fn();
    const stackLoad = vi.fn();
    const stackPolicy = vi.fn();

    const res = await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime: { load_state: usbLoad },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        i8042: { load_state: i8042Load },
        virtioInputKeyboard: { load_state: virtioKeyboardLoad },
        virtioInputMouse: { load_state: virtioMouseLoad },
        audioHda: { load_state: hdaLoad },
        audioVirtioSnd: { loadState: virtioSndLoad },
        pciBus: { loadState: pciLoad },
        netE1000: { load_state: e1000Load },
        netStack: { load_state: stackLoad, apply_tcp_restore_policy: stackPolicy },
      },
    });

    expect(restore).toHaveBeenCalledWith("state/test.snap");
    expect(usbLoad).toHaveBeenCalledWith(usbState);
    expect(i8042Load).toHaveBeenCalledWith(i8042State);
    expect(virtioKeyboardLoad).toHaveBeenCalledWith(virtioKeyboardState);
    expect(virtioMouseLoad).toHaveBeenCalledWith(virtioMouseState);
    expect(hdaLoad).toHaveBeenCalledWith(hdaState);
    expect(virtioSndLoad).toHaveBeenCalledWith(virtioSndState);
    expect(pciLoad).toHaveBeenCalledWith(pciState);
    expect(e1000Load).toHaveBeenCalledWith(e1000State);
    expect(stackLoad).toHaveBeenCalledWith(stackState);
    expect(stackPolicy).toHaveBeenCalledWith("drop");

    // Returned blob kinds should be canonical (not device.<id>).
    expect(res.devices?.map((d) => d.kind)).toEqual([
      "usb",
      "input.i8042",
      VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND,
      "audio.hda",
      "audio.virtio_snd",
      VM_SNAPSHOT_DEVICE_PCI_CFG_KIND,
      "net.e1000",
      "net.stack",
    ]);
  });

  it("ignores corrupt AUSB container blobs instead of attempting legacy raw restore", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      // AUSB header + 1 trailing byte (insufficient for an entry header) -> decode fails.
      const corrupt = new Uint8Array([0x41, 0x55, 0x53, 0x42, 0x01, 0x00, 0x00, 0x00, 0xff]);
      const xhciLoad = vi.fn();
      const uhciLoad = vi.fn();

      restoreUsbDeviceState(
        {
          usbXhciControllerBridge: { load_state: xhciLoad },
          usbUhciRuntime: { load_state: uhciLoad },
          usbUhciControllerBridge: null,
          usbEhciControllerBridge: null,
        },
        corrupt,
      );

      expect(xhciLoad).not.toHaveBeenCalled();
      expect(uhciLoad).not.toHaveBeenCalled();
      expect(warn.mock.calls.some((args) => String(args[0]).includes("AUSB"))).toBe(true);
    } finally {
      warn.mockRestore();
    }
  });

  it("restores USB state from legacy usb.uhci kinds", async () => {
    const usbState = new Uint8Array([0x01, 0x02]);

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      devices: [{ kind: "usb.uhci", bytes: usbState }],
    }));

    const api = { vm_snapshot_restore_from_opfs: restore } as unknown as WasmApi;
    const usbLoad = vi.fn();

    const res = await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime: { load_state: usbLoad },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
    });

    expect(usbLoad).toHaveBeenCalledWith(usbState);
    // Return kind should be canonicalized even when restoring legacy aliases.
    expect(res.devices?.map((d) => d.kind)).toEqual(["usb"]);
  });

  it("prefers canonical USB kind when both canonical + legacy entries are present", async () => {
    const canonicalBytes = new Uint8Array([0x10]);
    const legacyBytes = new Uint8Array([0x20]);

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      // Put the legacy entry *after* the canonical one to ensure restore precedence is stable.
      devices: [
        { kind: "usb", bytes: canonicalBytes },
        { kind: "usb.uhci", bytes: legacyBytes },
      ],
    }));

    const api = { vm_snapshot_restore_from_opfs: restore } as unknown as WasmApi;
    const usbLoad = vi.fn();

    await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime: { load_state: usbLoad },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
    });

    expect(usbLoad).toHaveBeenCalledWith(canonicalBytes);
  });

  it("restores legacy raw UHCI USB blobs into UHCI when available (even if xHCI exists)", async () => {
    const usbState = makeAeroIoSnapshotHeader("UHRT");

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      devices: [{ kind: `device.${VM_SNAPSHOT_DEVICE_ID_USB}`, bytes: usbState }],
    }));

    const api = { vm_snapshot_restore_from_opfs: restore } as unknown as WasmApi;

    const xhciLoad = vi.fn();
    const uhciLoad = vi.fn();

    await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: { load_state: xhciLoad },
        usbUhciRuntime: { load_state: uhciLoad },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
    });

    expect(uhciLoad).toHaveBeenCalledWith(usbState);
    expect(xhciLoad).not.toHaveBeenCalled();
  });

  it("restores UHCI + xHCI controller blobs from a USB container (AUSB)", async () => {
    const uhciState = new Uint8Array([0x01, 0x02]);
    const xhciState = new Uint8Array([0xaa, 0xbb, 0xcc]);
    const container = encodeUsbSnapshotContainer([
      { tag: USB_SNAPSHOT_TAG_UHCI, bytes: uhciState },
      { tag: USB_SNAPSHOT_TAG_XHCI, bytes: xhciState },
    ]);

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      devices: [{ kind: `device.${VM_SNAPSHOT_DEVICE_ID_USB}`, bytes: container }],
    }));

    const api = { vm_snapshot_restore_from_opfs: restore } as unknown as WasmApi;

    const xhciLoad = vi.fn();
    const uhciLoad = vi.fn();

    await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: { load_state: xhciLoad },
        usbUhciRuntime: { load_state: uhciLoad },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
    });

    expect(uhciLoad).toHaveBeenCalledWith(uhciState);
    expect(xhciLoad).toHaveBeenCalledWith(xhciState);
  });

  it("does not let corrupt cached AUSB USB bytes affect a subsequent save", async () => {
    const xhciState = new Uint8Array([0xaa]);
    const corrupt = new Uint8Array([0x41, 0x55, 0x53, 0x42, 0x01, 0x00, 0x00, 0x00, 0xff]);

    const calls: Array<{ devices: unknown }> = [];
    const api = {
      vm_snapshot_save_to_opfs: (_path: string, _cpu: Uint8Array, _mmu: Uint8Array, devices: unknown) => {
        calls.push({ devices });
      },
    } as unknown as WasmApi;

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/test.snap",
      cpu: new ArrayBuffer(4),
      mmu: new ArrayBuffer(8),
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: { save_state: () => xhciState },
        usbUhciRuntime: null,
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
      restoredDevices: [{ kind: "usb", bytes: corrupt }],
    });

    expect(calls).toHaveLength(1);
    const devices = calls[0]!.devices as Array<{ kind: string; bytes: Uint8Array }>;
    expect(devices).toHaveLength(1);
    expect(devices[0]!.kind).toBe(`device.${VM_SNAPSHOT_DEVICE_ID_USB}`);
    // The IO worker should ignore corrupt cached USB bytes and emit a valid container from the
    // fresh snapshot.
    const decoded = decodeUsbSnapshotContainer(devices[0]!.bytes);
    expect(decoded).not.toBeNull();
    expect(decoded!.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_XHCI)?.bytes).toEqual(xhciState);
  });

  it("forwards device blobs to WorkerVmSnapshot builder when free-function exports are absent", async () => {
    const addCalls: Array<{ id: number; version: number; flags: number; data: Uint8Array }> = [];
    const saveCalls: Array<{ path: string }> = [];

    class FakeBuilder {
      set_cpu_state_v2(_cpu: Uint8Array, _mmu: Uint8Array): void {
        // ignore
      }

      add_device_state(id: number, version: number, flags: number, data: Uint8Array): void {
        addCalls.push({ id, version, flags, data });
      }

      async snapshot_full_to_opfs(path: string): Promise<void> {
        saveCalls.push({ path });
      }

      free(): void {
        // ignore
      }
    }

    const api = { WorkerVmSnapshot: FakeBuilder } as unknown as WasmApi;

    const usbState = new Uint8Array([0x01, 0x02]);
    const i8042State = new Uint8Array([0x02]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const virtioSndState = new Uint8Array([0x03, 0x03, 0x03]);
    const pciState = new Uint8Array([0x80, 0x81]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const cpu = new ArrayBuffer(4);
    const mmu = new ArrayBuffer(8);

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/test.snap",
      cpu,
      mmu,
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime: { save_state: () => usbState },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        i8042: { save_state: () => i8042State },
        audioHda: { save_state: () => hdaState },
        audioVirtioSnd: { saveState: () => virtioSndState },
        pciBus: { saveState: () => pciState },
        netE1000: { save_state: () => e1000State },
        netStack: { save_state: () => stackState },
      },
    });

    expect(saveCalls).toEqual([{ path: "state/test.snap" }]);
    // Ensure device IDs are mapped via vmSnapshotDeviceKindToId (not `device.<id>` strings).
    expect(addCalls.map((c) => c.id)).toEqual([
      VM_SNAPSHOT_DEVICE_ID_USB,
      VM_SNAPSHOT_DEVICE_ID_I8042,
      VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA,
      VM_SNAPSHOT_DEVICE_ID_AUDIO_VIRTIO_SND,
      14,
      VM_SNAPSHOT_DEVICE_ID_E1000,
      VM_SNAPSHOT_DEVICE_ID_NET_STACK,
    ]);
  });

  it("applies device blobs from WorkerVmSnapshot builder restore", async () => {
    const usbState = new Uint8Array([0x01, 0x02]);
    const i8042State = new Uint8Array([0x02]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const virtioSndState = new Uint8Array([0x03, 0x03, 0x03]);
    const pciState = new Uint8Array([0x80, 0x81]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const usbLoad = vi.fn();
    const i8042Load = vi.fn();
    const hdaLoad = vi.fn();
    const virtioSndLoad = vi.fn();
    const pciLoad = vi.fn();
    const e1000Load = vi.fn();
    const stackLoad = vi.fn();
    const stackPolicy = vi.fn();

    class FakeBuilder {
      async restore_snapshot_from_opfs(_path: string): Promise<unknown> {
        return {
          cpu: new Uint8Array([0xaa]),
          mmu: new Uint8Array([0xbb]),
          devices: [
            { id: VM_SNAPSHOT_DEVICE_ID_USB, version: 1, flags: 0, data: usbState },
            { id: VM_SNAPSHOT_DEVICE_ID_I8042, version: 1, flags: 0, data: i8042State },
            { id: VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA, version: 1, flags: 0, data: hdaState },
            { id: VM_SNAPSHOT_DEVICE_ID_AUDIO_VIRTIO_SND, version: 1, flags: 0, data: virtioSndState },
            { id: 14, version: 1, flags: 0, data: pciState },
            { id: VM_SNAPSHOT_DEVICE_ID_E1000, version: 1, flags: 0, data: e1000State },
            { id: VM_SNAPSHOT_DEVICE_ID_NET_STACK, version: 1, flags: 0, data: stackState },
          ],
        };
      }

      free(): void {
        // ignore
      }
    }

    const api = { WorkerVmSnapshot: FakeBuilder } as unknown as WasmApi;

    const res = await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime: { load_state: usbLoad },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        i8042: { load_state: i8042Load },
        audioHda: { load_state: hdaLoad },
        audioVirtioSnd: { loadState: virtioSndLoad },
        pciBus: { loadState: pciLoad },
        netE1000: { load_state: e1000Load },
        netStack: { load_state: stackLoad, apply_tcp_restore_policy: stackPolicy },
      },
    });

    expect(usbLoad).toHaveBeenCalledWith(usbState);
    expect(i8042Load).toHaveBeenCalledWith(i8042State);
    expect(hdaLoad).toHaveBeenCalledWith(hdaState);
    expect(virtioSndLoad).toHaveBeenCalledWith(virtioSndState);
    expect(pciLoad).toHaveBeenCalledWith(pciState);
    expect(e1000Load).toHaveBeenCalledWith(e1000State);
    expect(stackLoad).toHaveBeenCalledWith(stackState);
    expect(stackPolicy).toHaveBeenCalledWith("drop");

    expect(res.devices?.map((d) => d.kind)).toEqual([
      "usb",
      "input.i8042",
      "audio.hda",
      "audio.virtio_snd",
      VM_SNAPSHOT_DEVICE_PCI_CFG_KIND,
      "net.e1000",
      "net.stack",
    ]);
  });

  it("merges coordinator + restored device blobs when saving (fresh overrides restored)", async () => {
    const calls: Array<{ devices: unknown }> = [];
    const api = {
      vm_snapshot_save_to_opfs: (_path: string, _cpu: Uint8Array, _mmu: Uint8Array, devices: unknown) => {
        calls.push({ devices });
      },
    } as unknown as WasmApi;

    const cachedUnknown777 = new Uint8Array([0x77]);
    const cachedOldI8042 = new Uint8Array([0x00]);
    const cachedUnknown999 = new Uint8Array([0x99]);

    const freshI8042 = new Uint8Array([0x02]);

    const coord999Buf = new Uint8Array([0x88]).buffer;
    const cpuInternalBuf = new Uint8Array([0x42]).buffer;

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/test.snap",
      cpu: new ArrayBuffer(4),
      mmu: new ArrayBuffer(8),
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime: null,
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        i8042: { save_state: () => freshI8042 },
        audioHda: null,
        audioVirtioSnd: null,
        pciBus: null,
        netE1000: null,
        netStack: null,
      },
      restoredDevices: [
        { kind: "device.777", bytes: cachedUnknown777 },
        { kind: "input.i8042", bytes: cachedOldI8042 },
        { kind: "device.999", bytes: cachedUnknown999 },
      ],
      coordinatorDevices: [
        { kind: "device.999", bytes: coord999Buf },
        { kind: "device.9", bytes: cpuInternalBuf },
      ],
    });

    expect(calls).toHaveLength(1);
    expect(calls[0]!.devices).toEqual([
      { kind: "device.777", bytes: cachedUnknown777 },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_I8042}`, bytes: freshI8042 },
      { kind: "device.999", bytes: new Uint8Array(coord999Buf) },
      { kind: "device.9", bytes: new Uint8Array(cpuInternalBuf) },
    ]);
  });

  it("uses CPU_INTERNAL v2 header overrides when saving coordinator blobs via WorkerVmSnapshot builder", async () => {
    const addCalls: Array<{ id: number; version: number; flags: number }> = [];

    class FakeBuilder {
      set_cpu_state_v2(_cpu: Uint8Array, _mmu: Uint8Array): void {
        // ignore
      }

      add_device_state(id: number, version: number, flags: number, _data: Uint8Array): void {
        addCalls.push({ id, version, flags });
      }

      async snapshot_full_to_opfs(_path: string): Promise<void> {
        // ignore
      }

      free(): void {
        // ignore
      }
    }

    const api = { WorkerVmSnapshot: FakeBuilder } as unknown as WasmApi;

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/test.snap",
      cpu: new ArrayBuffer(4),
      mmu: new ArrayBuffer(8),
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime: null,
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
      coordinatorDevices: [{ kind: "device.9", bytes: new Uint8Array([0x01, 0x02]).buffer }],
    });

    expect(addCalls).toEqual([{ id: 9, version: 2, flags: 0 }]);
  });

  it("normalizes legacy PCI device.5 blobs to pci.cfg kind when payload matches PCIB header", async () => {
    const pciBytes = new Uint8Array([0x41, 0x45, 0x52, 0x4f, 0, 0, 0, 0, 0x50, 0x43, 0x49, 0x42]);

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      devices: [{ kind: VM_SNAPSHOT_DEVICE_PCI_LEGACY_KIND, bytes: pciBytes }],
    }));

    const api = { vm_snapshot_restore_from_opfs: restore } as unknown as WasmApi;

    const pciLoad = vi.fn();

    const res = await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: null,
        usbUhciRuntime: null,
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        pciBus: { loadState: pciLoad },
        netE1000: null,
        netStack: null,
      },
    });

    expect(pciLoad).toHaveBeenCalledWith(pciBytes);
    expect(res.devices?.map((d) => d.kind)).toEqual([VM_SNAPSHOT_DEVICE_PCI_CFG_KIND]);
  });

  it("warns + ignores net.stack restore blobs when net.stack runtime is unavailable", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const stackState = new Uint8Array([0x06]);
      const api = {
        vm_snapshot_restore_from_opfs: () => ({
          cpu: new Uint8Array([0xaa]),
          mmu: new Uint8Array([0xbb]),
          devices: [{ kind: `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`, bytes: stackState }],
        }),
      } as unknown as WasmApi;

      await expect(
        restoreIoWorkerVmSnapshotFromOpfs({
          api,
          path: "state/test.snap",
          guestBase: 0,
          guestSize: 0x1000,
          runtimes: {
            usbXhciControllerBridge: null,
            usbUhciRuntime: null,
            usbUhciControllerBridge: null,
            usbEhciControllerBridge: null,
            netE1000: null,
            netStack: null,
          },
        }),
      ).resolves.toMatchObject({ cpu: expect.any(ArrayBuffer), mmu: expect.any(ArrayBuffer) });

      expect(warn.mock.calls.some((args) => String(args[0]).includes("net.stack"))).toBe(true);
    } finally {
      warn.mockRestore();
    }
  });

  it("warns + ignores UHCI USB controller blobs when UHCI runtime is unavailable", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const uhciState = new Uint8Array([0x01, 0x02]);
      const container = encodeUsbSnapshotContainer([{ tag: USB_SNAPSHOT_TAG_UHCI, bytes: uhciState }]);
      restoreUsbDeviceState(
        {
          usbXhciControllerBridge: null,
          usbUhciRuntime: null,
          usbUhciControllerBridge: null,
          usbEhciControllerBridge: null,
        },
        container,
      );

      expect(warn.mock.calls.some((args) => String(args[0]).includes("UHCI"))).toBe(true);
    } finally {
      warn.mockRestore();
    }
  });

  it("restores USB state from a snapshot container with UHCI + EHCI + xHCI controller blobs", () => {
    const uhciBytes = new Uint8Array([0x01, 0x02, 0x03]);
    const ehciBytes = new Uint8Array([0x04, 0x05]);
    const xhciBytes = new Uint8Array([0x06]);
    const container = encodeUsbSnapshotContainer([
      { tag: USB_SNAPSHOT_TAG_UHCI, bytes: uhciBytes },
      { tag: USB_SNAPSHOT_TAG_EHCI, bytes: ehciBytes },
      { tag: USB_SNAPSHOT_TAG_XHCI, bytes: xhciBytes },
    ]);

    const uhciLoad = vi.fn();
    const ehciLoad = vi.fn();
    const xhciLoad = vi.fn();
    restoreUsbDeviceState(
      {
        usbXhciControllerBridge: { load_state: xhciLoad },
        usbUhciRuntime: { load_state: uhciLoad },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: { load_state: ehciLoad },
      },
      container,
    );

    expect(uhciLoad).toHaveBeenCalledWith(expect.any(Uint8Array));
    expect((uhciLoad.mock.calls[0]![0] as Uint8Array)).toEqual(uhciBytes);
    expect(ehciLoad).toHaveBeenCalledWith(expect.any(Uint8Array));
    expect((ehciLoad.mock.calls[0]![0] as Uint8Array)).toEqual(ehciBytes);
    expect(xhciLoad).toHaveBeenCalledWith(expect.any(Uint8Array));
    expect((xhciLoad.mock.calls[0]![0] as Uint8Array)).toEqual(xhciBytes);
  });

  it("does not attempt to add two USB device blobs when both xHCI + UHCI controllers are present (WorkerVmSnapshot builder)", async () => {
    const addCalls: Array<{ id: number; version: number; flags: number; data: Uint8Array }> = [];
    class FakeBuilder {
      set_cpu_state_v2(_cpu: Uint8Array, _mmu: Uint8Array): void {
        // ignore
      }
      add_device_state(id: number, version: number, flags: number, data: Uint8Array): void {
        addCalls.push({ id, version, flags, data });
      }
      async snapshot_full_to_opfs(_path: string): Promise<void> {
        // ignore
      }
      free(): void {
        // ignore
      }
    }
    const api = { WorkerVmSnapshot: FakeBuilder } as unknown as WasmApi;

    const xhciBytes = new Uint8Array([0x06]);
    const uhciBytes = new Uint8Array([0x01]);
    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/test.snap",
      cpu: new ArrayBuffer(4),
      mmu: new ArrayBuffer(8),
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbXhciControllerBridge: { save_state: () => xhciBytes },
        usbUhciRuntime: { save_state: () => uhciBytes },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
    });

    const usbAdds = addCalls.filter((c) => c.id === VM_SNAPSHOT_DEVICE_ID_USB);
    expect(usbAdds).toHaveLength(1);
    const decoded = decodeUsbSnapshotContainer(usbAdds[0]!.data);
    expect(decoded).not.toBeNull();
    expect(decoded!.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_XHCI)?.bytes).toEqual(xhciBytes);
    expect(decoded!.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_UHCI)?.bytes).toEqual(uhciBytes);
  });

  it("ignores xHCI USB restore blobs when xHCI runtime is unavailable", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const xhciBytes = makeAeroIoSnapshotHeader("XHCB");
      const uhciLoad = vi.fn();
      restoreUsbDeviceState(
        {
          usbXhciControllerBridge: null,
          usbUhciRuntime: { load_state: uhciLoad },
          usbUhciControllerBridge: null,
          usbEhciControllerBridge: null,
        },
        xhciBytes,
      );
      expect(uhciLoad).not.toHaveBeenCalled();
      expect(warn.mock.calls.some((args) => String(args[0]).includes("xHCI") && String(args[0]).includes("ignoring"))).toBe(true);
    } finally {
      warn.mockRestore();
    }
  });

  it("preserves unknown device blobs across restore → save (device list merge semantics)", async () => {
    const unknownBytes = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
    const usbOld = new Uint8Array([0x01]);
    const usbFresh = new Uint8Array([0x02]);

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      devices: [
        { kind: "usb.uhci", bytes: usbOld },
        { kind: "device.123", bytes: unknownBytes },
      ],
    }));

    const saveCalls: Array<{ devices: unknown }> = [];
    const save = vi.fn((_path: string, _cpu: Uint8Array, _mmu: Uint8Array, devices: unknown) => {
      saveCalls.push({ devices });
    });

    const api = {
      vm_snapshot_restore_from_opfs: restore,
      vm_snapshot_save_to_opfs: save,
    } as unknown as WasmApi;

    const restored = await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbUhciRuntime: null,
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        usbXhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
    });

    // Restores should accept the legacy `usb.uhci` kind and normalize it to canonical `usb` so
    // follow-up saves don't emit duplicate USB entries.
    expect(restored.restoredDevices.map((d) => d.kind)).toEqual(["usb", "device.123"]);

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/next.snap",
      cpu: new ArrayBuffer(4),
      mmu: new ArrayBuffer(8),
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbUhciRuntime: { save_state: () => usbFresh },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        usbXhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
      restoredDevices: restored.restoredDevices,
    });

    expect(saveCalls).toHaveLength(1);
    // Unknown blob should be preserved, and the USB blob should come from the fresh snapshot (not restored).
    expect(saveCalls[0]!.devices).toEqual([
      { kind: "device.123", bytes: unknownBytes },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_USB}`, bytes: usbFresh },
    ]);
  });

  it("preserves cached USB controller sub-blobs across restore → save (USB container merge semantics)", async () => {
    const uhciOld = new Uint8Array([0x01]);
    const xhciOld = new Uint8Array([0x02, 0x03]);
    const cachedContainer = encodeUsbSnapshotContainer([
      { tag: USB_SNAPSHOT_TAG_UHCI, bytes: uhciOld },
      { tag: USB_SNAPSHOT_TAG_XHCI, bytes: xhciOld },
    ]);

    const uhciFresh = new Uint8Array([0x04]);
    const saveCalls: Array<{ devices: unknown }> = [];

    const api = {
      vm_snapshot_save_to_opfs: (_path: string, _cpu: Uint8Array, _mmu: Uint8Array, devices: unknown) => {
        saveCalls.push({ devices });
      },
    } as unknown as WasmApi;

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path: "state/next.snap",
      cpu: new ArrayBuffer(4),
      mmu: new ArrayBuffer(8),
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbUhciRuntime: { save_state: () => uhciFresh },
        usbUhciControllerBridge: null,
        usbEhciControllerBridge: null,
        usbXhciControllerBridge: null,
        netE1000: null,
        netStack: null,
      },
      restoredDevices: [{ kind: "usb.uhci", bytes: cachedContainer }],
    });

    expect(saveCalls).toHaveLength(1);
    const payload = saveCalls[0]!.devices as Array<{ kind: string; bytes: Uint8Array }>;
    expect(payload).toHaveLength(1);
    expect(payload[0]!.kind).toBe(`device.${VM_SNAPSHOT_DEVICE_ID_USB}`);

    const decoded = decodeUsbSnapshotContainer(payload[0]!.bytes);
    expect(decoded).not.toBeNull();
    const uhci = decoded!.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_UHCI)?.bytes ?? null;
    const xhci = decoded!.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_XHCI)?.bytes ?? null;
    expect(uhci).toEqual(uhciFresh);
    expect(xhci).toEqual(xhciOld);
  });

  it("includes RuntimeDiskWorker snapshot state as an extra device.<id> blob on save and applies it on restore", async () => {
    const diskState = serializeRuntimeDiskSnapshot({
      version: 1,
      nextHandle: 3,
      disks: [
        {
          handle: 1,
          readOnly: false,
          sectorSize: 512,
          capacityBytes: 1024,
          backend: {
            kind: "local",
            backend: "opfs",
            key: "test.img",
            format: "raw",
            diskKind: "hdd",
            sizeBytes: 1024,
          },
        },
      ],
    });
    const diskStateCopy = diskState.slice();
    let restoredPayload: Uint8Array | null = null;
    const diskClient = {
      prepareSnapshot: vi.fn(async () => diskState),
      restoreFromSnapshot: vi.fn(async (state: Uint8Array) => {
        // Simulate `RuntimeDiskClient` transfer semantics (buffer detaches after postMessage).
        restoredPayload = state.slice();
        structuredClone(state.buffer as ArrayBuffer, { transfer: [state.buffer as ArrayBuffer] });
      }),
    };

    const devices: Array<{ kind: string; bytes: Uint8Array }> = [];
    await appendRuntimeDiskWorkerSnapshotDeviceBlob(devices, diskClient);
    expect(diskClient.prepareSnapshot).toHaveBeenCalledTimes(1);
    expect(devices).toEqual([{ kind: IO_WORKER_RUNTIME_DISK_SNAPSHOT_KIND, bytes: diskState }]);

    const restored = await restoreRuntimeDiskWorkerSnapshotFromDeviceBlobs({ devices, diskClient });
    expect(diskClient.restoreFromSnapshot).toHaveBeenCalledTimes(1);
    expect(restoredPayload).toEqual(diskStateCopy);
    // The device blob stored in `devices` should not be detached by restore.
    expect(devices[0]!.bytes).toEqual(diskStateCopy);
    expect(restored).toMatchObject({ state: diskState });
    expect(restored?.activeDisk).toEqual({ handle: 1, sectorSize: 512, capacityBytes: 1024, readOnly: false });
    expect(restored?.cdDisk).toBeNull();
  });

  it("vm.snapshot.pause waits for in-flight disk I/O (diskIoChain) before ACKing paused", async () => {
    let resolveChain!: () => void;
    let diskIoChain: Promise<void> = new Promise<void>((resolve) => {
      resolveChain = () => resolve();
    });

    const setSnapshotPaused = vi.fn();
    const setUsbPaused = vi.fn();
    const onPaused = vi.fn();

    let finished = false;
    const task = pauseIoWorkerSnapshotAndDrainDiskIo({
      setSnapshotPaused,
      setUsbProxyCompletionRingDispatchPaused: setUsbPaused,
      getDiskIoChain: () => diskIoChain,
      onPaused: () => {
        onPaused();
        finished = true;
      },
    });

    // Should synchronously enter snapshot-paused mode, but not ACK paused until disk I/O drains.
    expect(setSnapshotPaused).toHaveBeenCalledWith(true);
    expect(setUsbPaused).toHaveBeenCalledWith(true);
    expect(onPaused).not.toHaveBeenCalled();

    // Allow microtasks to run; still blocked on diskIoChain.
    await Promise.resolve();
    expect(finished).toBe(false);

    resolveChain();
    await task;
    expect(onPaused).toHaveBeenCalledTimes(1);
    expect(finished).toBe(true);
  });

  it("vm.snapshot.pause waits for diskIoChain to stabilize when new I/O is queued during pause", async () => {
    let resolveFirst!: () => void;
    let resolveSecond!: () => void;
    let diskIoChain: Promise<void> = new Promise<void>((resolve) => {
      resolveFirst = () => resolve();
    });

    const setSnapshotPaused = vi.fn();
    const setUsbPaused = vi.fn();
    const onPaused = vi.fn();

    const task = pauseIoWorkerSnapshotAndDrainDiskIo({
      setSnapshotPaused,
      setUsbProxyCompletionRingDispatchPaused: setUsbPaused,
      getDiskIoChain: () => diskIoChain,
      onPaused,
    });

    // Simulate a new disk I/O op being chained after pause begins.
    diskIoChain = new Promise<void>((resolve) => {
      resolveSecond = () => resolve();
    });

    resolveFirst();
    // Allow the pause helper to observe the first chain resolved.
    await Promise.resolve();
    expect(onPaused).not.toHaveBeenCalled();

    resolveSecond();
    await task;
    expect(onPaused).toHaveBeenCalledTimes(1);
  });
});
