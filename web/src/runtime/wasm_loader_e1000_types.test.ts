import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (E1000 typings)", () => {
  it("requires feature detection for the optional E1000Bridge export", () => {
    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete values to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const api = {} as WasmApi;

    type E1000 = InstanceType<NonNullable<WasmApi["E1000Bridge"]>>;
    const dev = { pop_tx_frame: () => null } as unknown as E1000;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error E1000Bridge may be undefined
      // eslint-disable-next-line @typescript-eslint/no-unused-expressions
      new api.E1000Bridge(0, 0);

      // `mac_addr` is optional for forwards/backwards compatibility.
      // @ts-expect-error mac_addr may be undefined
      dev.mac_addr();
    }
    void assertStrictNullChecksEnforced;

    if (api.E1000Bridge) {
      const nic = new api.E1000Bridge(0, 0);
      nic.mmio_read(0, 4);
      nic.mmio_write(0, 4, 0);
      nic.io_read(0, 4);
      nic.io_write(0, 4, 0);
      nic.poll();
      nic.irq_level();
      nic.receive_frame(new Uint8Array());
      void nic.pop_tx_frame();
      nic.free();
    }

    if (dev.mac_addr) {
      void dev.mac_addr();
    }

    expect(true).toBe(true);
  });
});

