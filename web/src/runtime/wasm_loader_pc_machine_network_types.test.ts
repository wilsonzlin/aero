import { describe, expect, it } from "vitest";

import type { SharedRingBufferHandle, WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (PcMachine network typings)", () => {
  it("requires feature detection for optional PcMachine exports/methods", () => {
    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete values to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const api = {} as WasmApi;

    type PcMachine = InstanceType<NonNullable<WasmApi["PcMachine"]>>;

    const ring: SharedRingBufferHandle = {
      capacity_bytes: () => 0,
      try_push: (_payload: Uint8Array) => true,
      try_pop: () => null,
      wait_for_data: () => {},
      push_blocking: (_payload: Uint8Array) => {},
      pop_blocking: () => new Uint8Array(),
      free: () => {},
    };

    const ioIpcSab =
      typeof SharedArrayBuffer !== "undefined"
        ? new SharedArrayBuffer(0)
        : (new ArrayBuffer(0) as unknown as SharedArrayBuffer);

    const machine = {
      reset: () => {},
      set_disk_image: (_bytes: Uint8Array) => {},
      attach_l2_tunnel_rings: (_tx: SharedRingBufferHandle, _rx: SharedRingBufferHandle) => {},
      detach_network: () => {},
      poll_network: () => {},
      run_slice: (_maxInsts: number) => ({
        kind: 0,
        executed: 0,
        detail: "",
        free: () => {},
      }),
      attach_net_rings: (_tx: SharedRingBufferHandle, _rx: SharedRingBufferHandle) => {},
      detach_net_rings: () => {},
      net_stats: () => null,
      free: () => {},
    } as unknown as PcMachine;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error PcMachine may be undefined
      // eslint-disable-next-line @typescript-eslint/no-unused-expressions
      new api.PcMachine(0);

      // @ts-expect-error attach_net_rings may be undefined
      machine.attach_net_rings(ring, ring);
      // @ts-expect-error attach_l2_tunnel_from_io_ipc_sab may be undefined
      machine.attach_l2_tunnel_from_io_ipc_sab(ioIpcSab);
      // @ts-expect-error detach_net_rings may be undefined
      machine.detach_net_rings();
      // @ts-expect-error net_stats may be undefined
      machine.net_stats();
    }
    void assertStrictNullChecksEnforced;

    if (api.PcMachine) {
      const m = new api.PcMachine(2 * 1024 * 1024);
      m.reset();
      m.set_disk_image(new Uint8Array());
      m.attach_l2_tunnel_rings(ring, ring);
      if (m.attach_l2_tunnel_from_io_ipc_sab) {
        m.attach_l2_tunnel_from_io_ipc_sab(ioIpcSab);
      }
      if (m.attach_net_rings) {
        m.attach_net_rings(ring, ring);
      }
      if (m.net_stats) {
        m.net_stats();
      }
      if (m.detach_net_rings) {
        m.detach_net_rings();
      }
      m.detach_network();
      m.poll_network();
      m.run_slice(1_000_000).free();
      m.free();
    }

    if (machine.attach_net_rings) {
      machine.attach_net_rings(ring, ring);
    }
    if (machine.attach_l2_tunnel_from_io_ipc_sab) {
      machine.attach_l2_tunnel_from_io_ipc_sab(ioIpcSab);
    }
    if (machine.detach_net_rings) {
      machine.detach_net_rings();
    }
    if (machine.net_stats) {
      machine.net_stats();
    }

    expect(true).toBe(true);
  });
});
