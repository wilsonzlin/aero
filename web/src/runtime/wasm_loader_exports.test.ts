import { afterEach, describe, expect, it } from "vitest";

import { initWasm } from "./wasm_loader";

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

const originalJsOverride = globalThis.__aeroWasmJsImporterOverride;

afterEach(() => {
  globalThis.__aeroWasmJsImporterOverride = originalJsOverride;
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
  it("surfaces MouseButton/MouseButtons when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const MouseButton = { Left: 0, Middle: 1, Right: 2, Back: 3, Forward: 4 } as const;
    const MouseButtons = { Left: 1, Right: 2, Middle: 4, Back: 8, Forward: 16 } as const;

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        MouseButton,
        MouseButtons,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.MouseButton).toBe(MouseButton);
    expect(api.MouseButtons).toBe(MouseButtons);
    expect(api.MouseButton?.Left).toBe(0);
    expect(api.MouseButtons?.Middle).toBe(4);
  });

  it("surfaces storage_capabilities when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const caps = {
      opfsSupported: true,
      opfsSyncAccessSupported: false,
      isWorkerScope: true,
      crossOriginIsolated: false,
      sharedArrayBufferSupported: true,
      isSecureContext: true,
    } as const;

    const storage_capabilities = () => caps;

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        storage_capabilities,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.storage_capabilities).toBe(storage_capabilities);
    expect(api.storage_capabilities?.()).toEqual(caps);
  });

  it("surfaces legacy shared-guest-memory Machine free-function factories when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const create_win7_machine_shared_guest_memory = (_guestBase: number, _guestSize: number) => ({});
    const create_machine_win7_shared_guest_memory = (_guestBase: number, _guestSize: number) => ({});
    const create_machine_shared_guest_memory_win7 = (_guestBase: number, _guestSize: number) => ({});

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        create_win7_machine_shared_guest_memory,
        create_machine_win7_shared_guest_memory,
        create_machine_shared_guest_memory_win7,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.create_win7_machine_shared_guest_memory).toBe(create_win7_machine_shared_guest_memory);
    expect(api.create_machine_win7_shared_guest_memory).toBe(create_machine_win7_shared_guest_memory);
    expect(api.create_machine_shared_guest_memory_win7).toBe(create_machine_shared_guest_memory_win7);
  });

  it("supports camelCase create*Machine*SharedGuestMemory naming (surfaced via create_* keys)", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const createWin7MachineSharedGuestMemory = (_guestBase: number, _guestSize: number) => ({});
    const createMachineWin7SharedGuestMemory = (_guestBase: number, _guestSize: number) => ({});
    const createMachineSharedGuestMemoryWin7 = (_guestBase: number, _guestSize: number) => ({});

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        createWin7MachineSharedGuestMemory,
        createMachineWin7SharedGuestMemory,
        createMachineSharedGuestMemoryWin7,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.create_win7_machine_shared_guest_memory).toBe(createWin7MachineSharedGuestMemory);
    expect(api.create_machine_win7_shared_guest_memory).toBe(createMachineWin7SharedGuestMemory);
    expect(api.create_machine_shared_guest_memory_win7).toBe(createMachineSharedGuestMemoryWin7);
  });

  it("supports camelCase guestRamLayout/memStoreU32/memLoadU32 naming (surfaced via canonical keys)", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const caps = {
      opfsSupported: true,
      opfsSyncAccessSupported: false,
      isWorkerScope: true,
      crossOriginIsolated: false,
      sharedArrayBufferSupported: true,
      isSecureContext: true,
    } as const;

    const memStoreU32 = (_offset: number, _value: number) => {};
    const memLoadU32 = (_offset: number) => 0;
    const guestRamLayout = (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 });
    const storageCapabilities = () => caps;

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        memStoreU32,
        memLoadU32,
        guestRamLayout,
        storageCapabilities,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.mem_store_u32).toBe(memStoreU32);
    expect(api.mem_load_u32).toBe(memLoadU32);
    expect(api.guest_ram_layout).toBe(guestRamLayout);
    expect(api.storage_capabilities).toBe(storageCapabilities);
    expect(api.storage_capabilities?.()).toEqual(caps);
  });

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

    globalThis.__aeroWasmJsImporterOverride = {
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

  it("supports camelCase openRingByKind export naming (surfaces as open_ring_by_kind)", async () => {
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

    const openRingByKind = (_buffer: SharedArrayBuffer, _kind: number, _nth: number) => new FakeSharedRingBuffer(_buffer, 0);

    globalThis.__aeroWasmJsImporterOverride = {
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
        openRingByKind,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.SharedRingBuffer).toBe(FakeSharedRingBuffer);
    expect(api.open_ring_by_kind).toBe(openRingByKind);
  });

  it("surfaces vm_snapshot_save_to_opfs/vm_snapshot_restore_from_opfs when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const vm_snapshot_save_to_opfs = () => {};
    const vm_snapshot_restore_from_opfs = () => ({ cpu: new Uint8Array(), mmu: new Uint8Array(), devices: [] });

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        vm_snapshot_save_to_opfs,
        vm_snapshot_restore_from_opfs,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.vm_snapshot_save_to_opfs).toBe(vm_snapshot_save_to_opfs);
    expect(api.vm_snapshot_restore_from_opfs).toBe(vm_snapshot_restore_from_opfs);
  });

  it("supports camelCase vmSnapshot* VM snapshot free-function exports (surfaced via vm_snapshot_* keys)", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const vmSnapshotSaveToOpfs = () => {};
    const vmSnapshotRestoreFromOpfs = () => ({ cpu: new Uint8Array(), mmu: new Uint8Array(), devices: [] });

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        vmSnapshotSaveToOpfs,
        vmSnapshotRestoreFromOpfs,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.vm_snapshot_save_to_opfs).toBe(vmSnapshotSaveToOpfs);
    expect(api.vm_snapshot_restore_from_opfs).toBe(vmSnapshotRestoreFromOpfs);
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

    globalThis.__aeroWasmJsImporterOverride = {
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

    globalThis.__aeroWasmJsImporterOverride = {
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

  it("surfaces EhciControllerBridge when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeEhciControllerBridge {
      constructor(_guestBase: number, _guestSize?: number) {}

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      step_frames(_frames: number): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        EhciControllerBridge: FakeEhciControllerBridge,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.EhciControllerBridge).toBe(FakeEhciControllerBridge);
  });

  it("surfaces XhciControllerBridge when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeXhciControllerBridge {
      constructor(_guestBase: number, _guestSize: number) {}

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      step_frames(_frames: number): void {}
      step_frame(): void {}
      tick(_frames: number): void {}
      poll(): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        XhciControllerBridge: FakeXhciControllerBridge,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.XhciControllerBridge).toBe(FakeXhciControllerBridge);
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

    globalThis.__aeroWasmJsImporterOverride = {
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

  it("surfaces PcMachine when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakePcMachine {
      constructor(_ramSizeBytes: number) {}

      reset(): void {}
      set_disk_image(_bytes: Uint8Array): void {}
      attach_l2_tunnel_rings(_tx: unknown, _rx: unknown): void {}
      detach_network(): void {}
      poll_network(): void {}
      run_slice(_maxInsts: number): { kind: number; executed: number; detail: string; free(): void } {
        return { kind: 0, executed: 0, detail: "", free: () => {} };
      }
      free(): void {}
    }

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        PcMachine: FakePcMachine,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.PcMachine).toBe(FakePcMachine);
  });

  it("surfaces VirtioInputPciDevice when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeVirtioInputPciDevice {
      constructor(_guestBase: number, _guestSize: number, _kind: "keyboard" | "mouse") {}

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      poll(): void {}
      driver_ok(): boolean {
        return false;
      }
      irq_asserted(): boolean {
        return false;
      }
      inject_key(_linuxKey: number, _pressed: boolean): void {}
      inject_rel(_dx: number, _dy: number): void {}
      inject_button(_btn: number, _pressed: boolean): void {}
      inject_wheel(_delta: number): void {}
      inject_hwheel(_delta: number): void {}
      inject_wheel2(_wheel: number, _hwheel: number): void {}
      free(): void {}
    }

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        VirtioInputPciDevice: FakeVirtioInputPciDevice,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.VirtioInputPciDevice).toBe(FakeVirtioInputPciDevice);
  });

  it("surfaces VirtioNetPciBridge when present", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    class FakeVirtioNetPciBridge {
      constructor(_guestBase: number, _guestSize: number, _ioIpcSab: SharedArrayBuffer) {}

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      poll(): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    globalThis.__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (_input?: unknown) => {},
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
        mem_store_u32: (_offset: number, _value: number) => {},
        mem_load_u32: (_offset: number) => 0,
        guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
        VirtioNetPciBridge: FakeVirtioNetPciBridge,
      }),
    };

    const { api } = await initWasm({ variant: "single", module });
    expect(api.VirtioNetPciBridge).toBe(FakeVirtioNetPciBridge);

    // Type-level regression coverage: ensure the IO worker can instantiate and call methods without casts.
    const Ctor = api.VirtioNetPciBridge;
    expect(Ctor).toBeDefined();
    if (!Ctor) throw new Error("VirtioNetPciBridge export unexpectedly missing in fake WASM module");
    const bridge = new Ctor(0, 0, new SharedArrayBuffer(4));
    expect(bridge.mmio_read(0, 4)).toBe(0);
    bridge.mmio_write(0, 4, 0);
    bridge.poll?.();
    bridge.virtio_net_stats?.();
    expect(bridge.irq_asserted?.() ?? false).toBe(false);
    bridge.free();
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

    globalThis.__aeroWasmJsImporterOverride = {
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

    globalThis.__aeroWasmJsImporterOverride = {
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
    const originalCrossOriginIsolatedDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
    if (originalCrossOriginIsolatedDescriptor) Reflect.deleteProperty(globalThis, "crossOriginIsolated");

    try {
      if ("crossOriginIsolated" in globalThis) return;

      const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

      globalThis.__aeroWasmJsImporterOverride = {
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
      if (originalCrossOriginIsolatedDescriptor) {
        Object.defineProperty(globalThis, "crossOriginIsolated", originalCrossOriginIsolatedDescriptor);
      } else {
        Reflect.deleteProperty(globalThis, "crossOriginIsolated");
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
    const originalCrossOriginIsolatedDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    try {
      globalThis.__aeroWasmJsImporterOverride = {
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
      if (originalCrossOriginIsolatedDescriptor) {
        Object.defineProperty(globalThis, "crossOriginIsolated", originalCrossOriginIsolatedDescriptor);
      } else {
        Reflect.deleteProperty(globalThis, "crossOriginIsolated");
      }
    }
  });
});
