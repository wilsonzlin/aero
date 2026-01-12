import { describe, expect, it } from "vitest";
import { vi } from "vitest";

import type { WasmApi } from "../runtime/wasm_loader";
import { restoreIoWorkerVmSnapshotFromOpfs, saveIoWorkerVmSnapshotToOpfs } from "./io_worker_vm_snapshot";
import {
  VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA,
  VM_SNAPSHOT_DEVICE_ID_E1000,
  VM_SNAPSHOT_DEVICE_ID_I8042,
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
    const i8042State = new Uint8Array([0x02]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const usbUhciRuntime = { save_state: () => usbState };
    const i8042 = { save_state: () => i8042State };
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
        i8042,
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
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_I8042}`, bytes: i8042State },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA}`, bytes: hdaState },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_E1000}`, bytes: e1000State },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`, bytes: stackState },
    ]);
  });

  it("normalizes device.<id> kinds on restore and applies net.stack TCP restore policy=drop", async () => {
    const usbState = new Uint8Array([0x01, 0x02]);
    const i8042State = new Uint8Array([0x02]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const restore = vi.fn(() => ({
      cpu: new Uint8Array([0xaa]),
      mmu: new Uint8Array([0xbb]),
      devices: [
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_USB}`, bytes: usbState },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_I8042}`, bytes: i8042State },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA}`, bytes: hdaState },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_E1000}`, bytes: e1000State },
        { kind: `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`, bytes: stackState },
      ],
    }));

    const api = { vm_snapshot_restore_from_opfs: restore } as unknown as WasmApi;

    const usbLoad = vi.fn();
    const i8042Load = vi.fn();
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
        i8042: { load_state: i8042Load },
        audioHda: { load_state: hdaLoad },
        netE1000: { load_state: e1000Load },
        netStack: { load_state: stackLoad, apply_tcp_restore_policy: stackPolicy },
      },
    });

    expect(restore).toHaveBeenCalledWith("state/test.snap");
    expect(usbLoad).toHaveBeenCalledWith(usbState);
    expect(i8042Load).toHaveBeenCalledWith(i8042State);
    expect(hdaLoad).toHaveBeenCalledWith(hdaState);
    expect(e1000Load).toHaveBeenCalledWith(e1000State);
    expect(stackLoad).toHaveBeenCalledWith(stackState);
    expect(stackPolicy).toHaveBeenCalledWith("drop");

    // Returned blob kinds should be canonical (not device.<id>).
    expect(res.devices?.map((d) => d.kind)).toEqual(["usb.uhci", "input.i8042", "audio.hda", "net.e1000", "net.stack"]);
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
        usbUhciRuntime: { save_state: () => usbState },
        usbUhciControllerBridge: null,
        i8042: { save_state: () => i8042State },
        audioHda: { save_state: () => hdaState },
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
      VM_SNAPSHOT_DEVICE_ID_E1000,
      VM_SNAPSHOT_DEVICE_ID_NET_STACK,
    ]);
  });

  it("applies device blobs from WorkerVmSnapshot builder restore", async () => {
    const usbState = new Uint8Array([0x01, 0x02]);
    const i8042State = new Uint8Array([0x02]);
    const hdaState = new Uint8Array([0x02, 0x03]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const usbLoad = vi.fn();
    const i8042Load = vi.fn();
    const hdaLoad = vi.fn();
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
        usbUhciRuntime: { load_state: usbLoad },
        usbUhciControllerBridge: null,
        i8042: { load_state: i8042Load },
        audioHda: { load_state: hdaLoad },
        netE1000: { load_state: e1000Load },
        netStack: { load_state: stackLoad, apply_tcp_restore_policy: stackPolicy },
      },
    });

    expect(usbLoad).toHaveBeenCalledWith(usbState);
    expect(i8042Load).toHaveBeenCalledWith(i8042State);
    expect(hdaLoad).toHaveBeenCalledWith(hdaState);
    expect(e1000Load).toHaveBeenCalledWith(e1000State);
    expect(stackLoad).toHaveBeenCalledWith(stackState);
    expect(stackPolicy).toHaveBeenCalledWith("drop");

    expect(res.devices?.map((d) => d.kind)).toEqual(["usb.uhci", "input.i8042", "audio.hda", "net.e1000", "net.stack"]);
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
