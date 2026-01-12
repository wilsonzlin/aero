import { describe, expect, it } from "vitest";

import type { WasmApi } from "../runtime/wasm_loader";
import { saveIoWorkerVmSnapshotToOpfs } from "./io_worker_vm_snapshot";
import { VM_SNAPSHOT_DEVICE_ID_E1000, VM_SNAPSHOT_DEVICE_ID_NET_STACK, VM_SNAPSHOT_DEVICE_ID_USB } from "./vm_snapshot_wasm";

describe("workers/io_worker_vm_snapshot", () => {
  it("forwards net.e1000 and net.stack blobs to vm_snapshot_save_to_opfs when save_state hooks exist", async () => {
    const calls: Array<{ path: string; cpu: Uint8Array; mmu: Uint8Array; devices: unknown }> = [];
    const api = {
      vm_snapshot_save_to_opfs: (path: string, cpu: Uint8Array, mmu: Uint8Array, devices: unknown) => {
        calls.push({ path, cpu, mmu, devices });
      },
    } as unknown as WasmApi;

    const usbState = new Uint8Array([0x01, 0x02]);
    const e1000State = new Uint8Array([0x03, 0x04, 0x05]);
    const stackState = new Uint8Array([0x06]);

    const usbUhciRuntime = { save_state: () => usbState };
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
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_E1000}`, bytes: e1000State },
      { kind: `device.${VM_SNAPSHOT_DEVICE_ID_NET_STACK}`, bytes: stackState },
    ]);
  });
});
