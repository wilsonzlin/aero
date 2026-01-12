import { describe, expect, it } from "vitest";

import type { SharedRingBufferHandle, WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine network typings)", () => {
  it("requires feature detection for optional network attachment methods", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const machine = {
      attach_l2_tunnel_rings: (_tx: SharedRingBufferHandle, _rx: SharedRingBufferHandle) => {},
      attach_l2_tunnel_from_io_ipc_sab: (_sab: SharedArrayBuffer) => {},
      detach_network: () => {},
    } as unknown as Machine;

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

    // Optional methods should require feature detection under `strictNullChecks`.
    function assertStrictNullChecksEnforced() {
      // @ts-expect-error attach_l2_tunnel_rings may be undefined
      machine.attach_l2_tunnel_rings(ring, ring);
      // @ts-expect-error attach_l2_tunnel_from_io_ipc_sab may be undefined
      machine.attach_l2_tunnel_from_io_ipc_sab(ioIpcSab);
      // @ts-expect-error detach_network may be undefined
      machine.detach_network();
      // @ts-expect-error attach_net_rings may be undefined
      machine.attach_net_rings(ring, ring);
      // @ts-expect-error detach_net_rings may be undefined
      machine.detach_net_rings();
      // @ts-expect-error net_stats may be undefined
      machine.net_stats();
    }
    void assertStrictNullChecksEnforced;

    if (machine.attach_l2_tunnel_rings) {
      machine.attach_l2_tunnel_rings(ring, ring);
    }
    if (machine.attach_l2_tunnel_from_io_ipc_sab) {
      machine.attach_l2_tunnel_from_io_ipc_sab(ioIpcSab);
    }
    if (machine.detach_network) {
      machine.detach_network();
    }
    if (machine.attach_net_rings) {
      machine.attach_net_rings(ring, ring);
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
