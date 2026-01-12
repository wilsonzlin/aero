import { afterEach, describe, expect, it } from "vitest";

import { initWasm } from "./wasm_loader";

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

const originalJsOverride = (globalThis as any).__aeroWasmJsImporterOverride;

afterEach(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).__aeroWasmJsImporterOverride = originalJsOverride;
});

function sharedMemorySupported(): boolean {
  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory !== "function") return false;
  if (typeof SharedArrayBuffer === "undefined") return false;
  try {
    // eslint-disable-next-line no-new
    const mem = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    return mem.buffer instanceof SharedArrayBuffer;
  } catch {
    return false;
  }
}

describe("runtime/wasm_loader (optional exports)", () => {
  it("surfaces SharedRingBuffer/open_ring_by_kind when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeSharedRingBuffer {
      constructor(_buffer: SharedArrayBuffer, _offsetBytes: number) {}

      capacity_bytes(): number {
        return 0;
      }

      try_push(_payload: Uint8Array): boolean {
        return true;
      }

      try_pop(): Uint8Array | null {
        return null;
      }

      wait_for_data(): void {}

      push_blocking(_payload: Uint8Array): void {}

      pop_blocking(): Uint8Array {
        return new Uint8Array();
      }

      free(): void {}
    }

    const open_ring_by_kind = (_buffer: SharedArrayBuffer, _kind: number, _nth: number) =>
      new FakeSharedRingBuffer(_buffer, 0);

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        SharedRingBuffer: FakeSharedRingBuffer,
        open_ring_by_kind,
      }),
    };

    const { api, variant } = await initWasm({ variant: "single", module });
    expect(variant).toBe("single");
    expect(api.SharedRingBuffer).toBe(FakeSharedRingBuffer);
    expect(api.open_ring_by_kind).toBe(open_ring_by_kind);
  });

  it("surfaces WebUsbUhciBridge when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeWebUsbUhciBridge {
      constructor(_guestBase: number) {}

      io_read(_offset: number, _size: number): number {
        return 0;
      }
      io_write(_offset: number, _size: number, _value: number): void {}
      step_frames(_frames: number): void {}
      irq_level(): boolean {
        return false;
      }
      set_connected(_connected: boolean): void {}

      drain_actions(): unknown {
        return null;
      }
      push_completion(_completion: unknown): void {}
      reset(): void {}
      pending_summary(): unknown {
        return null;
      }
      free(): void {}
    }

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        WebUsbUhciBridge: FakeWebUsbUhciBridge,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.WebUsbUhciBridge).toBe(FakeWebUsbUhciBridge);
  });

  it("surfaces UhciControllerBridge when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeUhciControllerBridge {
      constructor(_guestBase: number, _guestSize?: number) {}

      io_read(_offset: number, _size: number): number {
        return 0;
      }
      io_write(_offset: number, _size: number, _value: number): void {}
      tick_1ms(): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        UhciControllerBridge: FakeUhciControllerBridge,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.UhciControllerBridge).toBe(FakeUhciControllerBridge);
  });

  it("surfaces E1000Bridge when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeE1000Bridge {
      constructor(_guestBase: number, _guestSize: number, _mac?: Uint8Array) {}

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      io_read(_offset: number, _size: number): number {
        return 0;
      }
      io_write(_offset: number, _size: number, _value: number): void {}
      poll(): void {}
      receive_frame(_frame: Uint8Array): void {}
      pop_tx_frame(): Uint8Array | null {
        return null;
      }
      irq_level(): boolean {
        return false;
      }
      free(): void {}
    }

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        E1000Bridge: FakeE1000Bridge,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.E1000Bridge).toBe(FakeE1000Bridge);
  });

  it("surfaces UsbPassthroughDemo when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeUsbPassthroughDemo {
      reset(): void {}
      queue_get_device_descriptor(_len: number): void {}
      queue_get_config_descriptor(_len: number): void {}
      drain_actions(): unknown {
        return null;
      }
      push_completion(_completion: unknown): void {}
      poll_last_result(): unknown {
        return null;
      }
      free(): void {}
    }

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        UsbPassthroughDemo: FakeUsbPassthroughDemo,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.UsbPassthroughDemo).toBe(FakeUsbPassthroughDemo);
  });

  it("surfaces GuestCpuBenchHarness when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeGuestCpuBenchHarness {
      payload_info(_variant: string): unknown {
        return {};
      }

      run_payload_once(_variant: string, _iters: number): unknown {
        return {};
      }

      free(): void {}
    }

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        GuestCpuBenchHarness: FakeGuestCpuBenchHarness,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.GuestCpuBenchHarness).toBeDefined();
    expect(api.GuestCpuBenchHarness).toBe(FakeGuestCpuBenchHarness);
  });

  it("allows threaded init without a crossOriginIsolated flag in Node-like runtimes", async () => {
    if (!sharedMemorySupported()) return;

    // This test exercises the "non-web" path where `crossOriginIsolated` is not
    // a defined global. In browsers, the property exists and is not writable, so
    // we skip when the runtime provides it.
    const hadCrossOriginIsolated = Object.prototype.hasOwnProperty.call(globalThis, "crossOriginIsolated");
    const originalCrossOriginIsolated = (globalThis as any).crossOriginIsolated;
    if (hadCrossOriginIsolated) delete (globalThis as any).crossOriginIsolated;

    try {
      if ("crossOriginIsolated" in globalThis) return;

      const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroWasmJsImporterOverride = {
        threaded: async () => ({
          default: async (_input?: unknown) => {},
          greet: (name: string) => `hello ${name}`,
          add: (a: number, b: number) => a + b,
          version: () => 1,
          sum: (a: number, b: number) => a + b,
          mem_store_u32: (_offset: number, _value: number) => {},
          mem_load_u32: (_offset: number) => 0,
          guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        }),
      };

      const { variant } = await initWasm({ variant: "threaded", module });
      expect(variant).toBe("threaded");
    } finally {
      if (hadCrossOriginIsolated) {
        (globalThis as any).crossOriginIsolated = originalCrossOriginIsolated;
      } else {
        delete (globalThis as any).crossOriginIsolated;
      }
    }
  });

  it("surfaces SharedRingBuffer/open_ring_by_kind for threaded init when present", async () => {
    if (!sharedMemorySupported()) return;
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeSharedRingBuffer {
      constructor(_buffer: SharedArrayBuffer, _offsetBytes: number) {}
      capacity_bytes(): number {
        return 0;
      }
      try_push(_payload: Uint8Array): boolean {
        return true;
      }
      try_pop(): Uint8Array | null {
        return null;
      }
      wait_for_data(): void {}
      push_blocking(_payload: Uint8Array): void {}
      pop_blocking(): Uint8Array {
        return new Uint8Array();
      }
      free(): void {}
    }

    const open_ring_by_kind = (_buffer: SharedArrayBuffer, _kind: number, _nth: number) =>
      new FakeSharedRingBuffer(_buffer, 0);

    // In browsers, `crossOriginIsolated` is present and must be true for
    // SharedArrayBuffer/WASM threads. Spoof it here so the test exercises the
    // same (web-like) path under Node/Vitest.
    const hadCrossOriginIsolated = Object.prototype.hasOwnProperty.call(globalThis, "crossOriginIsolated");
    const originalCrossOriginIsolated = (globalThis as any).crossOriginIsolated;
    (globalThis as any).crossOriginIsolated = true;

    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroWasmJsImporterOverride = {
        threaded: async () => ({
          default: async (_input?: unknown) => {},
          greet: (name: string) => `hello ${name}`,
          add: (a: number, b: number) => a + b,
          version: () => 1,
          sum: (a: number, b: number) => a + b,
          mem_store_u32: (_offset: number, _value: number) => {},
          mem_load_u32: (_offset: number) => 0,
          guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
          SharedRingBuffer: FakeSharedRingBuffer,
          open_ring_by_kind,
        }),
      };

      const { api, variant } = await initWasm({ variant: "threaded", module });
      expect(variant).toBe("threaded");
      expect(api.SharedRingBuffer).toBe(FakeSharedRingBuffer);
      expect(api.open_ring_by_kind).toBe(open_ring_by_kind);
    } finally {
      if (hadCrossOriginIsolated) {
        (globalThis as any).crossOriginIsolated = originalCrossOriginIsolated;
      } else {
        delete (globalThis as any).crossOriginIsolated;
      }
    }
  });
});
