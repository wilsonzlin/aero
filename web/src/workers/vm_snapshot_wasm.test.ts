import { describe, expect, it } from "vitest";

import type { WasmApi } from "../runtime/wasm_loader";
import {
  VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND,
  VM_SNAPSHOT_DEVICE_I8042_KIND,
  VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA,
  VM_SNAPSHOT_DEVICE_ID_I8042,
  VM_SNAPSHOT_DEVICE_E1000_KIND,
  VM_SNAPSHOT_DEVICE_ID_E1000,
  VM_SNAPSHOT_DEVICE_ID_NET_STACK,
  VM_SNAPSHOT_DEVICE_ID_USB,
  VM_SNAPSHOT_DEVICE_NET_STACK_KIND,
  VM_SNAPSHOT_DEVICE_USB_KIND,
  parseAeroIoSnapshotVersion,
  resolveVmSnapshotRestoreFromOpfsExport,
  resolveVmSnapshotSaveToOpfsExport,
  vmSnapshotDeviceIdToKind,
  vmSnapshotDeviceKindToId,
} from "./vm_snapshot_wasm";

describe("workers/vm_snapshot_wasm", () => {
  it("prefers free-function save exports when present", () => {
    const save = () => {};
    class FakeBuilder {}

    const api = { vm_snapshot_save_to_opfs: save, WorkerVmSnapshot: FakeBuilder } as unknown as WasmApi;
    const res = resolveVmSnapshotSaveToOpfsExport(api);
    expect(res).not.toBeNull();
    expect(res?.kind).toBe("free-function");
    if (res?.kind === "free-function") {
      expect(res.fn).toBe(save);
    }
  });

  it("falls back to WorkerVmSnapshot for save when free-functions are absent", () => {
    class FakeBuilder {}
    const api = { WorkerVmSnapshot: FakeBuilder } as unknown as WasmApi;
    const res = resolveVmSnapshotSaveToOpfsExport(api);
    expect(res).not.toBeNull();
    expect(res?.kind).toBe("builder");
    if (res?.kind === "builder") {
      expect(res.Ctor).toBe(FakeBuilder);
    }
  });

  it("returns null for save when no compatible exports exist", () => {
    const api = {} as unknown as WasmApi;
    expect(resolveVmSnapshotSaveToOpfsExport(api)).toBeNull();
  });

  it("prefers free-function restore exports when present", () => {
    const restore = () => {};
    class FakeBuilder {}

    const api = { vm_snapshot_restore_from_opfs: restore, WorkerVmSnapshot: FakeBuilder } as unknown as WasmApi;
    const res = resolveVmSnapshotRestoreFromOpfsExport(api);
    expect(res).not.toBeNull();
    expect(res?.kind).toBe("free-function");
    if (res?.kind === "free-function") {
      expect(res.fn).toBe(restore);
    }
  });

  it("falls back to WorkerVmSnapshot for restore when free-functions are absent", () => {
    class FakeBuilder {}
    const api = { WorkerVmSnapshot: FakeBuilder } as unknown as WasmApi;
    const res = resolveVmSnapshotRestoreFromOpfsExport(api);
    expect(res).not.toBeNull();
    expect(res?.kind).toBe("builder");
    if (res?.kind === "builder") {
      expect(res.Ctor).toBe(FakeBuilder);
    }
  });

  it("returns null for restore when no compatible exports exist", () => {
    const api = {} as unknown as WasmApi;
    expect(resolveVmSnapshotRestoreFromOpfsExport(api)).toBeNull();
  });

  it("maps snapshot device kinds/ids for WorkerVmSnapshot", () => {
    expect(vmSnapshotDeviceKindToId(VM_SNAPSHOT_DEVICE_USB_KIND)).toBe(VM_SNAPSHOT_DEVICE_ID_USB);
    expect(vmSnapshotDeviceIdToKind(VM_SNAPSHOT_DEVICE_ID_USB)).toBe(VM_SNAPSHOT_DEVICE_USB_KIND);

    expect(vmSnapshotDeviceKindToId(VM_SNAPSHOT_DEVICE_I8042_KIND)).toBe(VM_SNAPSHOT_DEVICE_ID_I8042);
    expect(vmSnapshotDeviceIdToKind(VM_SNAPSHOT_DEVICE_ID_I8042)).toBe(VM_SNAPSHOT_DEVICE_I8042_KIND);

    expect(vmSnapshotDeviceKindToId(VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND)).toBe(VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA);
    expect(vmSnapshotDeviceIdToKind(VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA)).toBe(VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND);

    expect(vmSnapshotDeviceKindToId(VM_SNAPSHOT_DEVICE_NET_STACK_KIND)).toBe(VM_SNAPSHOT_DEVICE_ID_NET_STACK);
    expect(vmSnapshotDeviceIdToKind(VM_SNAPSHOT_DEVICE_ID_NET_STACK)).toBe(VM_SNAPSHOT_DEVICE_NET_STACK_KIND);

    expect(vmSnapshotDeviceKindToId(VM_SNAPSHOT_DEVICE_E1000_KIND)).toBe(VM_SNAPSHOT_DEVICE_ID_E1000);
    expect(vmSnapshotDeviceIdToKind(VM_SNAPSHOT_DEVICE_ID_E1000)).toBe(VM_SNAPSHOT_DEVICE_E1000_KIND);

    // Forward compatibility: unknown device IDs should still roundtrip through WorkerVmSnapshot.
    expect(vmSnapshotDeviceKindToId("device.999")).toBe(999);
    expect(vmSnapshotDeviceIdToKind(999)).toBe("device.999");

    expect(vmSnapshotDeviceKindToId("unknown")).toBeNull();
  });

  it("parses aero-io-snapshot TLV version header when present", () => {
    const bytes = new Uint8Array(16);
    bytes[0] = 0x41;
    bytes[1] = 0x45;
    bytes[2] = 0x52;
    bytes[3] = 0x4f;
    // device_id = "UHRT"
    bytes[8] = 0x55;
    bytes[9] = 0x48;
    bytes[10] = 0x52;
    bytes[11] = 0x54;
    // major=0x0201, minor=0x0403
    bytes[12] = 0x01;
    bytes[13] = 0x02;
    bytes[14] = 0x03;
    bytes[15] = 0x04;

    expect(parseAeroIoSnapshotVersion(bytes)).toEqual({ version: 0x0201, flags: 0x0403 });
    expect(parseAeroIoSnapshotVersion(new Uint8Array())).toEqual({ version: 1, flags: 0 });
  });
  it("parses legacy AERO-prefixed device snapshot header when present", () => {
    const bytes = new Uint8Array(16);
    bytes[0] = 0x41;
    bytes[1] = 0x45;
    bytes[2] = 0x52;
    bytes[3] = 0x4f;
    // legacy header version=0x0201, flags=0x0403
    bytes[4] = 0x01;
    bytes[5] = 0x02;
    bytes[6] = 0x03;
    bytes[7] = 0x04;
    // Non-ASCII bytes in the "device id" slot should cause fallback to the legacy header parse.
    bytes[8] = 0x04;
    bytes[9] = 0x45;
    bytes[10] = 0x01;
    bytes[11] = 0xff;
    // Would be interpreted as device_version by the io-snapshot parser; ensure we don't use it.
    bytes[12] = 0xaa;
    bytes[13] = 0xbb;
    bytes[14] = 0xcc;
    bytes[15] = 0xdd;

    expect(parseAeroIoSnapshotVersion(bytes)).toEqual({ version: 0x0201, flags: 0x0403 });
  });
});
