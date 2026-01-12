import { describe, expect, it } from "vitest";
import { vi } from "vitest";

import type { WasmApi } from "../runtime/wasm_loader";
import { restoreIoWorkerVmSnapshotFromOpfs, saveIoWorkerVmSnapshotToOpfs } from "./io_worker_vm_snapshot";
import {
  VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA,
  VM_SNAPSHOT_DEVICE_ID_E1000,
  VM_SNAPSHOT_DEVICE_ID_NET_STACK,
  VM_SNAPSHOT_DEVICE_ID_USB,
} from "./vm_snapshot_wasm";

describe("workers/io_worker_vm_snapshot", () => {
  it("forwards device blobs to vm_snapshot_save_to_opfs when save_state hooks exist", async () => {
    const calls: Array<{ path: string; cpu: Uint8Array; mmu: Uint8Array; devices: unknown }> = [];
    const api = {
      vm_snapshot_save_to_opfs: (path: string, cpu: Uint8Array, mmu: Uint8Array, devices: unknown) => {
        calls.push({ path, cpu, mmu, devices });
      },
    } as unknown as WasmApi;

    const usbState = new Uint8Array([0x01, 0x02]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const usbUhciRuntime = { save_state: () => usbState };
    const audioHda = { save_state: () => hdaState };
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
        usbUhciRuntime,
        usbUhciControllerBridge: null,
        audioHda,
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
    expect(calls[0]!.devices).toEqual([
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_USB}`, bytes: usbState },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA}`, bytes: hdaState },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_E1000}`, bytes: e1000State },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`, bytes: stackState },
    ]);
  });

  it("normalizes device.<id> kinds on restore and applies net.stack TCP restore policy=drop", async () => {
    const usbState = new Uint8Array([0x01, 0x02]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      devices: [
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_USB}`, bytes: usbState },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA}`, bytes: hdaState },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_E1000}`, bytes: e1000State },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`, bytes: stackState },
      ],
    }));

    const api = { vm_snapshot_restore_from_opfs: restore } as unknown as WasmApi;

    const usbLoad = vi.fn();
    const hdaLoad = vi.fn();
    const e1000Load = vi.fn();
    const stackLoad = vi.fn();
    const stackPolicy = vi.fn();

    const res = await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path: "state/test.snap",
      guestBase: 0,
      guestSize: 0x1000,
      runtimes: {
        usbUhciRuntime: { load_state: usbLoad },
        usbUhciControllerBridge: null,
        audioHda: { load_state: hdaLoad },
        netE1000: { load_state: e1000Load },
        netStack: { load_state: stackLoad, apply_tcp_restore_policy: stackPolicy },
      },
    });

    expect(restore).toHaveBeenCalledWith("state/test.snap");
    expect(usbLoad).toHaveBeenCalledWith(usbState);
    expect(hdaLoad).toHaveBeenCalledWith(hdaState);
    expect(e1000Load).toHaveBeenCalledWith(e1000State);
    expect(stackLoad).toHaveBeenCalledWith(stackState);
    expect(stackPolicy).toHaveBeenCalledWith("drop");

    // Returned blob kinds should be canonical (not device.<id>).
    expect(res.devices?.map((d) => d.kind)).toEqual(["usb.uhci", "audio.hda", "net.e1000", "net.stack"]);
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
            usbUhciRuntime: null,
            usbUhciControllerBridge: null,
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
});
