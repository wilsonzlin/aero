import type { UsbHostAction, UsbHostCompletion } from "../usb/webusb_backend";
import { lookupPrecompiledWasmModuleVariant } from "./wasm_precompiled_registry";

export type WasmVariant = "threaded" | "single";

export type MicBridgeHandle = {
    buffered_samples(): number;
    dropped_samples(): number;
    read_f32_into(out: Float32Array): number;
    free(): void;
};

export type SharedRingBufferHandle = {
    capacity_bytes(): number;
    try_push(payload: Uint8Array): boolean;
    // wasm-bindgen represents `Option<Uint8Array>` as `undefined` in most builds,
    // but older bindings or manual shims may use `null`. Accept both.
    try_pop(): Uint8Array | null | undefined;
    wait_for_data(): void;
    push_blocking(payload: Uint8Array): void;
    pop_blocking(): Uint8Array;
    free(): void;
};

export type WasmEnum<K extends string> = Readonly<Record<K, number>>;

// wasm-bindgen represents Rust enums as numeric discriminants in JS/TS type defs.
export type RunExitKind = number;

export type UhciControllerBridgeHandle = {
    io_read(offset: number, size: number): number;
    io_write(offset: number, size: number, value: number): void;
    tick_1ms(): void;
    irq_asserted(): boolean;
    save_state?(): Uint8Array;
    load_state?(bytes: Uint8Array): void;
    /**
     * Deterministic USB device/controller snapshot bytes.
     *
     * Optional for older WASM builds.
     */
    snapshot_state?: () => Uint8Array;
    restore_state?: (bytes: Uint8Array) => void;
    free(): void;
};

export type GuestCpuBenchHarnessHandle = {
    payload_info(variant: string): unknown;
    run_payload_once(variant: string, itersPerRun: number): unknown;
    free(): void;
};

export type VirtioNetPciBridgeHandle = {
    mmio_read(offset: number, size: number): number;
    mmio_write(offset: number, size: number, value: number): void;
    poll?(): void;
    tick?(nowMs?: number): void;
    irq_level?(): boolean;
    irq_asserted?(): boolean;
    free(): void;
};

export interface WasmApi {
    greet(name: string): string;
    add(a: number, b: number): number;
    version: () => number;
    sum: (a: number, b: number) => number;
    mem_store_u32: (offset: number, value: number) => void;
    mem_load_u32: (offset: number) => number;
    /**
     * Tier-1 JIT ABI constants (mirrors `aero_cpu_core::state`).
     *
     * This is used by browser workers that need to snapshot/restore CpuState when
     * rolling back a JIT block that exits to the interpreter.
     */
    jit_abi_constants?: () => {
        readonly cpu_state_size: number;
        readonly cpu_state_align: number;
        readonly cpu_rip_off: number;
        readonly cpu_rflags_off: number;
        readonly cpu_gpr_off: Uint32Array;
    };
    /**
     * Aero IPC ring helpers for working directly with an AIPC `SharedArrayBuffer`.
     *
     * Optional for older WASM builds.
     */
    SharedRingBuffer?: new (buffer: SharedArrayBuffer, offsetBytes: number) => SharedRingBufferHandle;
    open_ring_by_kind?: (buffer: SharedArrayBuffer, kind: number, nth: number) => SharedRingBufferHandle;
    /**
     * Demo renderer: writes an RGBA8888 test pattern into WASM linear memory at `dstOffset`.
     *
     * Optional for dev builds where the generated wasm-bindgen package hasn't been rebuilt yet.
     */
    demo_render_rgba8888?: (
        dstOffset: number,
        width: number,
        height: number,
        strideBytes: number,
        nowMs: number,
    ) => number;
    /**
     * Guest RAM layout contract (see `docs/adr/0003-shared-memory-layout.md`).
     *
     * Returns a small object describing where guest physical address 0 maps
     * inside the wasm linear memory.
     */
    guest_ram_layout: (desiredBytes: number) => {
        readonly guest_base: number;
        readonly guest_size: number;
        readonly runtime_reserved: number;
    };

    /**
     * DOM-style mouse button IDs (mirrors `MouseEvent.button`).
     *
     * Optional for older WASM builds.
     */
    MouseButton?: WasmEnum<"Left" | "Middle" | "Right">;
    /**
     * Mouse button bit values matching `MouseEvent.buttons` (bitmask).
     *
     * Optional for older WASM builds.
     */
    MouseButtons?: WasmEnum<"Left" | "Right" | "Middle">;

    /**
     * Guest-visible virtio-input device exposed via virtio-pci (BAR0 MMIO).
     */
    VirtioInputPciDevice?: new (guestBase: number, guestSize: number, kind: "keyboard" | "mouse") => {
        mmio_read(offset: number, size: number): number;
        mmio_write(offset: number, size: number, value: number): void;
        poll(): void;
        driver_ok(): boolean;
        irq_asserted(): boolean;
        inject_key(linux_key: number, pressed: boolean): void;
        inject_rel(dx: number, dy: number): void;
        inject_button(btn: number, pressed: boolean): void;
        inject_wheel(delta: number): void;
        free(): void;
    };

    /**
     * Guest-visible UHCI controller bridge.
     *
     * Optional and has multiple constructor signatures depending on the deployed WASM build:
     * - `new (guestBase)` for legacy builds (PIO + 1ms tick).
     * - `new (guestBase, guestSize)` for newer builds (PIO + frame stepping + USB topology management).
     */
    UhciControllerBridge?: {
        new (guestBase: number): {
            io_read(offset: number, size: number): number;
            io_write(offset: number, size: number, value: number): void;
            tick_1ms(): void;
            irq_asserted(): boolean;
            save_state?(): Uint8Array;
            load_state?(bytes: Uint8Array): void;
            snapshot_state?: () => Uint8Array;
            restore_state?: (bytes: Uint8Array) => void;
            free(): void;
        };
        new (guestBase: number, guestSize: number): {
            readonly guest_base: number;
            readonly guest_size: number;

            io_read(offset: number, size: number): number;
            io_write(offset: number, size: number, value: number): void;

            step_frames(frames: number): void;
            step_frame(): void;
            /**
             * Alias for {@link step_frame} retained for older call sites.
             */
            tick_1ms(): void;

            irq_asserted(): boolean;

            save_state?(): Uint8Array;
            load_state?(bytes: Uint8Array): void;

            attach_hub(rootPort: number, portCount: number): void;
            detach_at_path(path: number[]): void;

            attach_webhid_device(path: number[], device: InstanceType<WasmApi["WebHidPassthroughBridge"]>): void;
            attach_usb_hid_passthrough_device(
                path: number[],
                device: InstanceType<NonNullable<WasmApi["UsbHidPassthroughBridge"]>>,
            ): void;

            /**
             * WebUSB passthrough device helpers.
             *
             * The passthrough device is connected to a reserved UHCI root port and emits
             * `UsbHostAction`s that must be executed by the browser `UsbBroker`.
             */
            set_connected(connected: boolean): void;
            drain_actions(): UsbHostAction[] | null;
            push_completion(completion: UsbHostCompletion): void;
            reset(): void;
            pending_summary(): {
                queued_actions: number;
                queued_completions: number;
                inflight_control?: number | null;
                inflight_endpoints: number;
            } | null;

            snapshot_state?: () => Uint8Array;
            restore_state?: (bytes: Uint8Array) => void;
            free(): void;
        };
    };

    /**
     * Guest-visible Intel E1000 NIC bridge.
     *
     * The I/O worker exposes this as a PCI function with:
     * - BAR0: MMIO (0x20000 bytes)
     * - BAR1: I/O (0x40 bytes)
     *
     * TX/RX frames are moved over the IO_IPC_NET_{TX,RX} rings by the JS wrapper.
     */
    E1000Bridge?: new (guestBase: number, guestSize: number, mac?: Uint8Array) => {
        /**
         * Forward PCI configuration space writes into the Rust device model.
         *
         * This is required for keeping the device's internal PCI command register
         * (including Bus Master Enable) in sync with the JS PCI bus.
         *
         * Optional while older WASM builds are still in circulation.
         */
        pci_config_write?: (offset: number, size: number, value: number) => void;
        mmio_read(offset: number, size: number): number;
        mmio_write(offset: number, size: number, value: number): void;
        io_read(offset: number, size: number): number;
        io_write(offset: number, size: number, value: number): void;
        /**
         * Update the device model's PCI command register (offset 0x04, low 16 bits).
         *
         * Optional for older WASM builds.
         */
        set_pci_command?(command: number): void;
        poll(): void;
        receive_frame(frame: Uint8Array): void;
        // wasm-bindgen represents `Option<Uint8Array>` as `undefined` in most builds,
        // but older bindings or manual shims may use `null`. Accept both.
        pop_tx_frame(): Uint8Array | null | undefined;
        irq_level(): boolean;
        mac_addr?(): Uint8Array;
        /**
         * Snapshot/restore the guest-visible NIC model state as an `aero-io-snapshot` blob.
         *
         * Optional for older WASM builds.
         */
        save_state?(): Uint8Array;
        load_state?(bytes: Uint8Array): void;
        snapshot_state?(): Uint8Array;
        restore_state?(bytes: Uint8Array): void;
        free(): void;
    };

    /**
     * Guest-visible virtio-net PCI bridge.
     *
     * In the browser runtime this is backed by the `NET_TX`/`NET_RX` AIPC rings inside `ioIpcSab`
     * (see `web/src/runtime/shared_layout.ts`).
     *
     * Optional until all deployed WASM builds include virtio networking support.
     */
    VirtioNetPciBridge?: new (guestBase: number, guestSize: number, ioIpcSab: SharedArrayBuffer) => VirtioNetPciBridgeHandle;

    /**
     * Legacy i8042 (PS/2 keyboard + mouse) controller bridge.
     *
     * Optional for older WASM builds; when present, the browser I/O worker should
     * prefer this over the JS i8042 implementation so behavior + snapshots stay
     * consistent with the canonical Rust model.
     */
    I8042Bridge?: new () => {
        port_read(port: number): number;
        port_write(port: number, value: number): void;

        inject_key_scancode_bytes(packed: number, len: number): void;
        inject_mouse_move(dx: number, dy: number): void;
        inject_mouse_buttons(buttons: number): void;
        inject_mouse_wheel(delta: number): void;

        /**
         * IRQ "level mask" for IRQ1/IRQ12.
         *
         * Contract (matches `crates/aero-wasm/src/i8042_bridge.rs`):
         * - bit0 (0x01): IRQ1 asserted
         * - bit1 (0x02): IRQ12 asserted
         */
        irq_mask(): number;

        readonly a20_enabled: boolean;
        take_reset_requests(): number;

        save_state(): Uint8Array;
        load_state(bytes: Uint8Array): void;
        free(): void;
    };

    /**
     * Full PCI-capable PC machine wrapper (includes E1000 + ring backend integration).
     *
     * Optional for older WASM builds.
     */
    PcMachine?: new (ram_size_bytes: number) => {
        reset(): void;
        set_disk_image(bytes: Uint8Array): void;
        attach_l2_tunnel_rings(tx: SharedRingBufferHandle, rx: SharedRingBufferHandle): void;
        detach_network(): void;
        poll_network(): void;
        run_slice(max_insts: number): { kind: RunExitKind; executed: number; detail: string };
        free(): void;
    };

    UsbHidBridge: new () => {
        keyboard_event(usage: number, pressed: boolean): void;
        mouse_move(dx: number, dy: number): void;
        mouse_buttons(buttons: number): void;
        mouse_wheel(delta: number): void;
        gamepad_report(packedLo: number, packedHi: number): void;
        drain_next_keyboard_report(): Uint8Array | null;
        drain_next_mouse_report(): Uint8Array | null;
        drain_next_gamepad_report(): Uint8Array | null;
        free(): void;
    };
    WebHidPassthroughBridge: new (
        vendorId: number,
        productId: number,
        manufacturer: string | undefined,
        product: string | undefined,
        serial: string | undefined,
        collections: unknown,
    ) => {
        push_input_report(reportId: number, data: Uint8Array): void;
        drain_next_output_report(): { reportType: "output" | "feature"; reportId: number; data: Uint8Array<ArrayBuffer> } | null;
        configured(): boolean;
        free(): void;
    };

    /**
     * Generic USB HID passthrough bridge (accepts a pre-synthesized HID report descriptor).
     *
     * Optional while older wasm builds are still in circulation.
     */
    UsbHidPassthroughBridge?: new (
        vendorId: number,
        productId: number,
        manufacturer: string | undefined,
        product: string | undefined,
        serial: string | undefined,
        reportDescriptorBytes: Uint8Array,
        hasInterruptOut: boolean,
        interfaceSubclass?: number,
        interfaceProtocol?: number,
    ) => {
        push_input_report(reportId: number, data: Uint8Array): void;
        drain_next_output_report(): { reportType: "output" | "feature"; reportId: number; data: Uint8Array } | null;
        configured(): boolean;
        free(): void;
    };

    /**
     * WebUSB passthrough bridge (Rust `UsbPassthroughDevice` host action queue).
     *
     * Note: This is optional for older WASM builds.
     */
    UsbPassthroughBridge?: new () => {
        /**
         * Drains queued host actions. Returns `null` when there are no pending
         * actions (to keep worker-side polling allocation-free when idle).
         */
        drain_actions(): UsbHostAction[] | null;
        push_completion(completion: UsbHostCompletion): void;
        reset(): void;
        pending_summary(): {
            queued_actions: number;
            queued_completions: number;
            inflight_control?: number | null;
            inflight_endpoints: number;
        };
        free(): void;
    };

    /**
     * Guest-visible UHCI controller runtime (shared guest RAM + hotplug for WebHID/WebUSB passthrough).
     *
     * Note: optional until all deployed WASM builds include it.
     */
    UhciRuntime?: new (guestBase: number, guestSize: number) => {
        io_base(): number;
        irq_line(): number;
        irq_level(): boolean;

        port_read(offset: number, size: number): number;
        port_write(offset: number, size: number, value: number): void;

        tick_1ms(): void;
        step_frame(): void;

        webhid_attach(
            deviceId: number,
            vendorId: number,
            productId: number,
            productName: string | undefined,
            collectionsJson: unknown,
            preferredPort?: number,
        ): number;
        /**
         * Newer UHCI runtime builds support attaching WebHID devices behind the external hub
         * topology (e.g. `guestPath` like `[0, 4]`).
         *
         * Optional to allow older deployed wasm builds.
         */
        webhid_attach_at_path?(
            deviceId: number,
            vendorId: number,
            productId: number,
            productName: string | undefined,
            collectionsJson: unknown,
            guestPath: number[],
        ): void;
        /**
         * Optional external hub port-count hint for the UHCI runtime path. Older builds may ignore
         * this and rely on root-port-only WebHID attachment.
         */
        webhid_attach_hub?(guestPath: number[], portCount?: number): void;
        webhid_detach(deviceId: number): void;
        webhid_push_input_report(deviceId: number, reportId: number, data: Uint8Array): void;
        webhid_drain_output_reports(): Array<{ deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }>;

        /**
         * Attach a pre-built USB HID passthrough device at the given topology path.
         *
         * Optional for older WASM builds.
         */
        attach_usb_hid_passthrough_device?(
            guestPath: number[],
            device: InstanceType<NonNullable<WasmApi["UsbHidPassthroughBridge"]>>,
        ): void;

        webusb_attach(preferredPort?: number): number;
        webusb_detach(): void;
        webusb_drain_actions(): UsbHostAction[];
        webusb_push_completion(completion: UsbHostCompletion): void;

        save_state?(): Uint8Array;
        load_state?(bytes: Uint8Array): void;

        snapshot_state?: () => Uint8Array;
        restore_state?: (bytes: Uint8Array) => void;
        free(): void;
    };

    /**
     * WebUSB UHCI passthrough enumeration harness (drives UHCI TDs and emits `UsbHostAction`s).
     *
     * Note: optional until all deployed WASM builds include it.
     */
    WebUsbUhciPassthroughHarness?: new () => {
        /**
         * Human-readable state string for debugging.
         */
        state(): string;
        /**
         * Advance the harness by one UHCI frame and return the latest status snapshot.
         */
        tick(): unknown;
        /**
         * Return the current status snapshot without stepping.
         */
        status(): unknown;
        reset(): void;
        drain_actions(): UsbHostAction[] | null;
        push_completion(completion: UsbHostCompletion): void;
        free(): void;
    };

    /**
      * Worker-side UHCI controller + WebUSB passthrough bridge.
      *
      * This exports a guest-visible UHCI controller (PIO registers + TD/QH traversal)
      * with:
      * - root port 0 hosting an emulated external USB hub (for WebHID passthrough devices)
      * - root port 1 reserved for the WebUSB passthrough device (`set_connected`)
      */
    WebUsbUhciBridge?: new (guestBase: number) => {
        io_read(offset: number, size: number): number;
        io_write(offset: number, size: number, value: number): void;
        step_frames(frames: number): void;
        irq_level(): boolean;
        set_connected(connected: boolean): void;

        detach_at_path(path: number[]): void;
        attach_webhid_device(path: number[], device: unknown): void;
        attach_usb_hid_passthrough_device(path: number[], device: unknown): void;
        save_state?(): Uint8Array;
        load_state?(bytes: Uint8Array): void;

        drain_actions(): UsbHostAction[] | null;
        push_completion(completion: UsbHostCompletion): void;
        reset(): void;
        pending_summary(): {
            queued_actions: number;
            queued_completions: number;
            inflight_control?: number | null;
            inflight_endpoints: number;
        } | null;
        snapshot_state?: () => Uint8Array;
        restore_state?: (bytes: Uint8Array) => void;
        free(): void;
    };

    /**
     * WebUSB passthrough demo driver (queues GET_DESCRIPTOR to validate action↔completion wiring).
     *
     * Optional for older WASM builds.
     */
    UsbPassthroughDemo?: new () => {
        reset(): void;
        queue_get_device_descriptor(len: number): void;
        queue_get_config_descriptor(len: number): void;
        drain_actions(): unknown;
        push_completion(completion: unknown): void;
        poll_last_result(): unknown;
        free(): void;
    };

    /**
      * Synthesize a HID report descriptor from WebHID-normalized collections metadata.
      *
      * Optional while older wasm builds are still in circulation.
      */
    synthesize_webhid_report_descriptor?: (collectionsJson: unknown) => Uint8Array;

    /**
     * Guest CPU benchmark harness (PF-008).
     *
     * Optional while older wasm builds are still in circulation.
     */
    GuestCpuBenchHarness?: new () => GuestCpuBenchHarnessHandle;
    CpuWorkerDemo?: new (
        ramSizeBytes: number,
        framebufferOffsetBytes: number,
        width: number,
        height: number,
        tileSize: number,
        guestCounterOffsetBytes: number,
    ) => {
        tick(nowMs: number): number;
        render_frame(frameSeq: number, nowMs: number): number;
        free(): void;
    };
    AeroApi: new () => { version(): string; free(): void };
    /**
     * Legacy deterministic stub VM used for snapshotting demos.
     *
     * Deprecated in favor of `Machine` (the canonical full-system VM). This is now implemented as
     * a thin wrapper around the canonical `aero_machine::Machine` so that snapshot demos exercise
     * the real VM core.
     */
    DemoVm: new (ramSizeBytes: number) => {
        run_steps(steps: number): void;
        serial_output(): Uint8Array;
        /**
         * Returns the number of bytes currently in the demo VM's serial output buffer.
         *
         * Optional for older WASM builds; prefer this over calling `serial_output()`
         * when you only need the length (to avoid copying large buffers into JS).
         */
        serial_output_len?(): number;
        snapshot_full(): Uint8Array;
        snapshot_dirty(): Uint8Array;
        restore_snapshot(bytes: Uint8Array): void;
        /**
         * Stream a full snapshot directly into OPFS using a `FileSystemSyncAccessHandle`.
         *
         * Note: this requires running in a Dedicated Worker (sync access handles are worker-only).
         */
        snapshot_full_to_opfs?(path: string): Promise<void>;
        /**
         * Stream a dirty-page snapshot directly into OPFS using a `FileSystemSyncAccessHandle`.
         *
         * Note: this requires running in a Dedicated Worker (sync access handles are worker-only).
         */
        snapshot_dirty_to_opfs?(path: string): Promise<void>;
        /**
         * Restore a snapshot by streaming it from OPFS using a `FileSystemSyncAccessHandle`.
         *
         * Note: this requires running in a Dedicated Worker (sync access handles are worker-only).
         */
        restore_snapshot_from_opfs?(path: string): Promise<void>;
        free(): void;
    };

    /**
     * Canonical full-system VM (`aero_machine::Machine`).
     */
    Machine: new (ramSizeBytes: number) => {
        reset(): void;
        set_disk_image(bytes: Uint8Array): void;
        run_slice(maxInsts: number): { kind: number; executed: number; detail: string; free(): void };
        serial_output(): Uint8Array;
        /**
         * Returns the number of bytes currently buffered in the serial output log.
         *
         * Optional for older WASM builds.
         */
        serial_output_len?(): number;
        inject_browser_key(code: string, pressed: boolean): void;
        /**
         * PS/2 mouse injection helpers (optional for older WASM builds).
         */
        inject_mouse_motion?(dx: number, dy: number, wheel: number): void;
        /**
         * Inject mouse motion using PS/2 coordinate conventions:
         * - `dx`: positive is right
         * - `dy`: positive is up (PS/2 convention)
         * - `wheel`: positive is wheel up
         *
         * Optional for older WASM builds; prefer {@link inject_mouse_motion} when your input source
         * uses browser-style +Y down (e.g. `MouseEvent.movementY`).
         */
        inject_ps2_mouse_motion?(dx: number, dy: number, wheel: number): void;
        /**
         * Inject a mouse button transition using DOM `MouseEvent.button` mapping:
         * - `0`: left
         * - `1`: middle
         * - `2`: right
         *
         * Tip: when available, prefer `api.MouseButton.Left/Middle/Right` for a stable mapping
         * reference.
         */
        inject_mouse_button?(button: number, pressed: boolean): void;
        /**
         * Set all mouse buttons at once using a bitmask matching DOM `MouseEvent.buttons`:
         * - bit0 (`0x01`): left
         * - bit1 (`0x02`): right
         * - bit2 (`0x04`): middle
         *
         * Tip: when available, prefer building masks by OR'ing `api.MouseButtons.Left/Right/Middle`.
         */
        inject_mouse_buttons_mask?(mask: number): void;
        /**
         * Set mouse button state using PS/2 packet bit conventions (also matches DOM `MouseEvent.buttons`):
         * - bit0 (`0x01`): left
         * - bit1 (`0x02`): right
         * - bit2 (`0x04`): middle
         *
         * Optional for older WASM builds; prefer {@link inject_mouse_buttons_mask} (same mapping).
         */
        inject_ps2_mouse_buttons?(buttons: number): void;
        inject_mouse_left?(pressed: boolean): void;
        inject_mouse_right?(pressed: boolean): void;
        inject_mouse_middle?(pressed: boolean): void;
        /**
         * Attach/detach the canonical machine network backend via Aero IPC rings.
         *
         * Optional for older WASM builds.
         */
        attach_l2_tunnel_rings?(tx: SharedRingBufferHandle, rx: SharedRingBufferHandle): void;
        attach_l2_tunnel_from_io_ipc_sab?(ioIpc: SharedArrayBuffer): void;
        detach_network?(): void;
        /**
         * Legacy/alternate naming for attaching NET_TX/NET_RX rings.
         *
         * Optional for older WASM builds; prefer {@link attach_l2_tunnel_rings} when available.
         */
        attach_net_rings?(netTx: SharedRingBufferHandle, netRx: SharedRingBufferHandle): void;
        /**
         * Legacy/alternate naming for detaching the network backend.
         *
         * Optional for older WASM builds; prefer {@link detach_network} when available.
         */
        detach_net_rings?(): void;
        /**
         * Network backend statistics (if exposed by the WASM build).
         *
         * Optional for older WASM builds.
         */
        net_stats?():
            | {
                  tx_pushed_frames: bigint;
                  tx_dropped_oversize: bigint;
                  tx_dropped_full: bigint;
                  rx_popped_frames: bigint;
                  rx_dropped_oversize: bigint;
                  rx_corrupt: bigint;
              }
            | null;
        /**
         * Optional for older WASM builds; canonical machine snapshot support.
         */
        snapshot_full?(): Uint8Array;
        snapshot_dirty?(): Uint8Array;
        restore_snapshot?(bytes: Uint8Array): void;
        snapshot_full_to_opfs?(path: string): Promise<void>;
        snapshot_dirty_to_opfs?(path: string): Promise<void>;
        restore_snapshot_from_opfs?(path: string): Promise<void>;
        free(): void;
    };

    /**
     * Minimal browser VM loop (Tier-0 interpreter) wired to injected shared guest RAM.
     *
     * Intended for the CPU worker runtime (`web/src/workers/cpu.worker.ts`).
     */
    WasmVm?: new (guestBase: number, guestSize: number) => {
        reset_real_mode(entryIp: number): void;
        run_slice(maxInsts: number): { kind: number; executed: number; detail: string; free(): void };
        /**
         * Deterministic CPU/MMU snapshot helpers for the minimal Tier-0 VM loop.
         *
         * Used by the multi-worker VM snapshot orchestrator (CPU + IO workers).
         *
         * Optional while older WASM builds are still in circulation.
         */
        save_state_v2?: () => { cpu: Uint8Array; mmu: Uint8Array };
        load_state_v2?: (cpu: Uint8Array, mmu: Uint8Array) => void;
        free(): void;
    };

    /**
     * Tiered VM loop (Tier-0 interpreter + Tier-1 JIT dispatch via `globalThis.__aero_jit_call`).
     *
     * Used by the browser CPU worker runtime and the root Vite harness JIT smoke test
     * (`src/workers/cpu-worker.ts`).
     *
    * Optional while older WASM builds are still in circulation.
    */
    WasmTieredVm?: new (guestBase: number, guestSize: number) => {
        reset_real_mode(entryIp: number): void;

        /**
         * Execute up to N basic blocks. Each block is executed either via Tier-0 or a cached Tier-1 entry.
         *
         * Newer WASM builds return a structured result object; older builds return `void`.
         */
        run_blocks(
            blocks: number,
        ):
            | {
                  kind: number;
                  detail: string;
                  executed_blocks: number;
                  interp_blocks: number;
                  jit_blocks: number;
              }
            | void;
        /**
         * Drain de-duplicated compile requests (entry RIPs).
         */
        drain_compile_requests(): bigint[] | Uint32Array | BigInt64Array | BigUint64Array;
        /**
         * Notify the embedded JIT runtime that the guest wrote to memory.
         *
         * This is used by Tier-1 store helpers (when inline-TLB store fast-path is disabled) to
         * invalidate cached blocks/traces for self-modifying code correctness.
         *
         * Optional for older WASM builds.
         */
        on_guest_write?(paddr: bigint | number, len: number): void;
        /**
         * Install a compiled Tier-1 block into the runtime cache.
         *
         * Returns an array of evicted entry RIPs so the JS runtime can free/reuse table slots.
         */
        install_tier1_block(
            entryRip: bigint | number,
            tableIndex: number,
            codePaddr: bigint | number,
            byteLen: number,
        ): bigint[] | Uint32Array | BigInt64Array | BigUint64Array | void;
        /**
         * Notify the JIT runtime that guest code bytes were modified (e.g. DMA writes).
         */
        jit_on_guest_write?(paddr: bigint | number, len: number): void;
        readonly interp_executions?: number;
        readonly jit_executions?: number;
        /**
         * Total executed blocks since last reset (newer builds).
         */
        readonly interp_blocks_total?: bigint;
        readonly jit_blocks_total?: bigint;
        readonly guest_base?: number;
        readonly guest_size?: number;
        free(): void;
    };

    /**
     * Worker-side VM snapshot builder (CPU/MMU/device state + full RAM streaming to OPFS).
     *
     * Optional while older WASM builds are still in circulation.
     */
    WorkerVmSnapshot?: new (guestBase: number, guestSize: number) => {
        set_cpu_state_v2(cpuBytes: Uint8Array, mmuBytes: Uint8Array): void;
        add_device_state(id: number, version: number, flags: number, data: Uint8Array): void;
        snapshot_full_to_opfs(path: string): Promise<void>;
        restore_snapshot_from_opfs(path: string): Promise<unknown>;
        free(): void;
    };

    // Optional audio exports (present when the WASM build includes the audio worklet bridge).
    WorkletBridge?: new (capacityFrames: number, channelCount: number) => {
        readonly shared_buffer: SharedArrayBuffer;
        readonly capacity_frames: number;
        readonly channel_count: number;
        write_f32_interleaved(samples: Float32Array): number;
        buffer_level_frames(): number;
        underrun_count(): number;
        overrun_count(): number;
        free(): void;
    };
    create_worklet_bridge?: (capacityFrames: number, channelCount: number) => unknown;
    attach_worklet_bridge?: (sab: SharedArrayBuffer, capacityFrames: number, channelCount: number) => unknown;
    MicBridge?: {
        fromSharedBuffer(sab: SharedArrayBuffer): MicBridgeHandle;
    };
    attach_mic_bridge?: (sab: SharedArrayBuffer) => MicBridgeHandle;
    SineTone?: new () => {
        write(
            bridge: unknown,
            frames: number,
            freqHz: number,
            sampleRate: number,
            gain: number,
        ): number;
    };
    HdaPcmWriter?: new (dstSampleRateHz: number) => {
        readonly dst_sample_rate_hz: number;
        set_dst_sample_rate_hz(dstSampleRateHz: number): void;
        reset(): void;
        push_hda_pcm_bytes(bridge: unknown, hdaFormat: number, pcmBytes: Uint8Array): number;
        free(): void;
    };
    HdaPlaybackDemo?: new (
        ringSab: SharedArrayBuffer,
        capacityFrames: number,
        channelCount: number,
        hostSampleRate: number,
    ) => {
        readonly host_sample_rate_hz: number;
        readonly total_frames_produced: number;
        readonly total_frames_written: number;
        readonly total_frames_dropped: number;
        readonly last_tick_requested_frames: number;
        readonly last_tick_produced_frames: number;
        readonly last_tick_written_frames: number;
        readonly last_tick_dropped_frames: number;
        init_sine_dma(freqHz: number, gain: number): void;
        tick(frames: number): number;
        free(): void;
    };

    /**
     * Guest-visible Intel HD Audio (HDA) controller bridge (MMIO + DMA).
     *
     * Intended for the browser IO worker: JS provides the PCI/MMIO plumbing and drives
     * time progression; Rust provides the full HDA device model.
     *
     * Optional for older WASM builds.
     */
    HdaControllerBridge?: new (guestBase: number, guestSize: number) => {
        mmio_read(offset: number, size: number): number;
        mmio_write(offset: number, size: number, value: number): void;

        attach_audio_ring(ringSab: SharedArrayBuffer, capacityFrames: number, channelCount: number): void;
        detach_audio_ring(): void;

        attach_mic_ring(ringSab: SharedArrayBuffer, sampleRate: number): void;
        detach_mic_ring(): void;

        set_output_rate_hz(rate: number): void;
        process(frames: number): void;

        /**
         * Alias for {@link process} retained for older call sites.
         */
        step_frames(frames: number): void;

        irq_level(): boolean;

        /**
         * Legacy mic ring attachment helper retained for older call sites.
         *
         * Prefer {@link attach_mic_ring} + {@link detach_mic_ring} for new code.
         */
        set_mic_ring_buffer(sab?: SharedArrayBuffer): void;
        set_capture_sample_rate_hz(sampleRateHz: number): void;

        buffer_level_frames(): number;
        overrun_count(): number;

        /**
         * Optional AudioWorklet output ring attachment.
         *
         * Newer builds attach the SharedArrayBuffer ring directly to the WASM-side HDA controller so
         * the device can stream output samples without JS copies.
         */
        set_audio_ring_buffer?(sab: SharedArrayBuffer | null | undefined, capacityFrames: number, channelCount: number): void;

        /**
         * Deterministic device snapshot bytes (aero-io-snapshot TLV blob).
         *
         * Optional for older WASM builds.
         */
        save_state?(): Uint8Array;
        load_state?(bytes: Uint8Array): void;
        snapshot_state?(): Uint8Array;
        restore_state?(bytes: Uint8Array): void;
        free(): void;
    };
}

export interface WasmInitMemoryInfo {
    byteLength: number;
    shared: boolean;
}

export interface WasmInitResult {
    api: WasmApi;
    variant: WasmVariant;
    reason: string;
    memory?: WasmInitMemoryInfo;
}

export interface WasmInitOptions {
    /**
     * - `auto` (default): pick the best variant for this runtime.
     * - `threaded`: require the shared-memory build and throw if unavailable.
     * - `single`: force the non-shared-memory build.
     */
    variant?: WasmVariant | "auto";

    /**
     * Optional precompiled WebAssembly.Module to instantiate from.
     *
     * When available, callers can compile the WASM binary once (on the main thread)
     * and structured-clone the resulting `WebAssembly.Module` into workers so each
     * context can instantiate without redundant compilation.
     *
     * If instantiation from the provided module fails, we fall back to wasm-bindgen's
     * default loader (fetch/compile) and log a warning.
     */
    module?: WebAssembly.Module;

    /**
     * Optional `WebAssembly.Memory` to use as the module's imported linear memory.
     *
     * This is required for worker/shared-memory mode: the coordinator allocates a
     * shared `WebAssembly.Memory` and workers inject it so both JS and WASM code
     * observe the same guest RAM.
     */
    memory?: WebAssembly.Memory;
}

interface ThreadSupport {
    supported: boolean;
    reason: string;
}

function detectThreadSupport(): ThreadSupport {
    if (typeof globalThis === "undefined") {
        return { supported: false, reason: "Not running in a JS environment with globalThis" };
    }

    const hasCrossOriginIsolated = "crossOriginIsolated" in globalThis;

    // `crossOriginIsolated` is required for SharedArrayBuffer on the web. In non-web
    // contexts (e.g. Node/Vitest), this flag does not exist, but SharedArrayBuffer +
    // shared WebAssembly.Memory may still be available.
    if (hasCrossOriginIsolated && !(globalThis as any).crossOriginIsolated) {
        return {
            supported: false,
            reason: "crossOriginIsolated is false (missing COOP/COEP headers); SharedArrayBuffer is unavailable",
        };
    }

    if (typeof SharedArrayBuffer === "undefined") {
        return { supported: false, reason: "SharedArrayBuffer is undefined (not supported or not enabled)" };
    }

    if (typeof Atomics === "undefined") {
        return { supported: false, reason: "Atomics is undefined (WASM threads are not supported)" };
    }

    if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory === "undefined") {
        return { supported: false, reason: "WebAssembly.Memory is unavailable in this environment" };
    }

    try {
        // Even with SAB present, some environments may not support shared WebAssembly.Memory.
        // This is the most direct capability probe.
        // eslint-disable-next-line no-new
        new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        return { supported: false, reason: `Shared WebAssembly.Memory is not supported: ${message}` };
    }

    return {
        supported: true,
        reason: hasCrossOriginIsolated
            ? "crossOriginIsolated + SharedArrayBuffer + Atomics + shared WebAssembly.Memory"
            : "SharedArrayBuffer + Atomics + shared WebAssembly.Memory",
    };
}

type RawWasmModule = any;

function describeMemory(memory: WebAssembly.Memory | undefined): WasmInitMemoryInfo | undefined {
    if (!memory) return undefined;
    return {
        byteLength: memory.buffer.byteLength,
        shared: typeof SharedArrayBuffer !== "undefined" && memory.buffer instanceof SharedArrayBuffer,
    };
}

function toApi(mod: RawWasmModule): WasmApi {
    return {
        greet: mod.greet,
        add: mod.add,
        version: mod.version,
        sum: mod.sum,
        mem_store_u32: mod.mem_store_u32,
        guest_ram_layout: mod.guest_ram_layout,
        mem_load_u32: mod.mem_load_u32,
        jit_abi_constants: mod.jit_abi_constants,
        MouseButton: mod.MouseButton,
        MouseButtons: mod.MouseButtons,
        VirtioInputPciDevice: mod.VirtioInputPciDevice,
        SharedRingBuffer: mod.SharedRingBuffer,
        open_ring_by_kind: mod.open_ring_by_kind,
        demo_render_rgba8888: mod.demo_render_rgba8888,
        UsbHidBridge: mod.UsbHidBridge,
        WebHidPassthroughBridge: mod.WebHidPassthroughBridge,
        UsbHidPassthroughBridge: mod.UsbHidPassthroughBridge,
        UsbPassthroughBridge: mod.UsbPassthroughBridge,
        UhciRuntime: mod.UhciRuntime,
        VirtioNetPciBridge: mod.VirtioNetPciBridge,
        WebUsbUhciPassthroughHarness: mod.WebUsbUhciPassthroughHarness,
        UhciControllerBridge: mod.UhciControllerBridge,
        E1000Bridge: mod.E1000Bridge,
        PcMachine: mod.PcMachine,
        WebUsbUhciBridge: mod.WebUsbUhciBridge,
        I8042Bridge: mod.I8042Bridge,
        synthesize_webhid_report_descriptor: mod.synthesize_webhid_report_descriptor,
        GuestCpuBenchHarness: mod.GuestCpuBenchHarness,
        UsbPassthroughDemo: mod.UsbPassthroughDemo,
        CpuWorkerDemo: mod.CpuWorkerDemo,
        AeroApi: mod.AeroApi,
        DemoVm: mod.DemoVm,
        Machine: mod.Machine,
        WasmVm: mod.WasmVm,
        WasmTieredVm: mod.WasmTieredVm,
        WorkerVmSnapshot: mod.WorkerVmSnapshot,
        WorkletBridge: mod.WorkletBridge,
        create_worklet_bridge: mod.create_worklet_bridge,
        attach_worklet_bridge: mod.attach_worklet_bridge,
        MicBridge: mod.MicBridge,
        attach_mic_bridge: mod.attach_mic_bridge,
        SineTone: mod.SineTone,
        HdaPcmWriter: mod.HdaPcmWriter,
        HdaPlaybackDemo: mod.HdaPlaybackDemo,
        HdaControllerBridge: mod.HdaControllerBridge,
    };
}

// `wasm-pack` outputs into `web/src/wasm/pkg-single` and `web/src/wasm/pkg-threaded`.
//
// These directories are generated (see `web/scripts/build_wasm.mjs`) and are not
// necessarily present in a fresh checkout. Use `import.meta.glob` so:
//  - Vite builds don't fail when the generated output is missing.
//  - When the output *is* present, it is bundled as usual.
const wasmImporters = import.meta.glob("../wasm/pkg-*/aero_wasm.js");

const warnOnceKeys = new Set<string>();
function warnOnce(key: string, message: string): void {
    if (warnOnceKeys.has(key)) return;
    warnOnceKeys.add(key);
    console.warn(message);
}

declare global {
    // eslint-disable-next-line no-var
    var __aeroWasmJsImporterOverride:
        | Partial<Record<WasmVariant, () => Promise<RawWasmModule>>>
        | undefined;
}

function resolveWasmImporter(
    variant: WasmVariant,
): { importer: () => Promise<RawWasmModule>; wasmUrl: URL } | undefined {
    const override = globalThis.__aeroWasmJsImporterOverride?.[variant];
    if (override) {
        // The override is primarily for tests; avoid hard-coding a file:// URL which would
        // trigger Node-specific file reads in `resolveWasmInputForInit`.
        return { importer: override, wasmUrl: new URL("about:blank") };
    }

    const releasePath = variant === "single" ? "../wasm/pkg-single/aero_wasm.js" : "../wasm/pkg-threaded/aero_wasm.js";
    const devPath = variant === "single" ? "../wasm/pkg-single-dev/aero_wasm.js" : "../wasm/pkg-threaded-dev/aero_wasm.js";
    const importer = wasmImporters[releasePath] ?? wasmImporters[devPath];
    if (!importer) return undefined;

    const wasmPath =
        importer === wasmImporters[releasePath]
            ? variant === "single"
                ? "../wasm/pkg-single/aero_wasm_bg.wasm"
                : "../wasm/pkg-threaded/aero_wasm_bg.wasm"
            : variant === "single"
              ? "../wasm/pkg-single-dev/aero_wasm_bg.wasm"
              : "../wasm/pkg-threaded-dev/aero_wasm_bg.wasm";

    // The wasm binary is generated by `wasm-pack` and may not exist in a fresh checkout.
    // Suppress Vite's build-time warning about `new URL(..., import.meta.url)` paths that cannot
    // be resolved statically.
    const wasmUrl = new URL(/* @vite-ignore */ wasmPath, import.meta.url);

    return { importer: importer as () => Promise<RawWasmModule>, wasmUrl };
}

type WasmLoadResult = { api: WasmApi; memory?: WebAssembly.Memory };

function patchWasmImportsWithMemory(imports: unknown, memory: WebAssembly.Memory): void {
    if (!imports || typeof imports !== "object") return;
    const obj = imports as Record<string, any>;

    // `wasm-ld --import-memory` uses the canonical "env"."memory" import.
    obj.env ??= {};
    obj.env.memory = memory;

    // wasm-bindgen's JS glue also creates an `imports.wbg` (or
    // `imports.__wbindgen_placeholder__`) module for JS↔WASM shims; patch these
    // defensively in case the memory import gets routed differently by future
    // toolchains.
    if (obj.wbg && typeof obj.wbg === "object") obj.wbg.memory = memory;
    if (obj.__wbindgen_placeholder__ && typeof obj.__wbindgen_placeholder__ === "object") {
        obj.__wbindgen_placeholder__.memory = memory;
    }
}

async function withPatchedMemoryImport<T>(
    memory: WebAssembly.Memory,
    fn: () => Promise<T>,
): Promise<T> {
    const originalInstantiate = WebAssembly.instantiate;
    const hasInstantiateStreaming = typeof WebAssembly.instantiateStreaming === "function";
    const originalInstantiateStreaming = hasInstantiateStreaming ? WebAssembly.instantiateStreaming : undefined;

    // Avoid strict typing here: `WebAssembly.instantiate*` are overloaded and
    // Node/Vitest execute these `.ts` sources directly with stripped types.
    // Keep the runtime behavior correct while sidestepping TypeScript overload
    // assignability issues across TS versions.
    const instantiatePatched = (module: any, imports?: any) => {
        const patchedImports = imports ?? {};
        patchWasmImportsWithMemory(patchedImports, memory);
        return originalInstantiate(module as any, patchedImports as any);
    };

    (WebAssembly as any).instantiate = instantiatePatched;
    if (hasInstantiateStreaming) {
        const instantiateStreamingPatched = (source: any, imports?: any) => {
            const patchedImports = imports ?? {};
            patchWasmImportsWithMemory(patchedImports, memory);
            return originalInstantiateStreaming!(source as any, patchedImports as any);
        };
        (WebAssembly as any).instantiateStreaming = instantiateStreamingPatched;
    }
    try {
        return await fn();
    } finally {
        (WebAssembly as any).instantiate = originalInstantiate;
        if (hasInstantiateStreaming) {
            (WebAssembly as any).instantiateStreaming = originalInstantiateStreaming;
        }
    }
}

async function resolveWasmInputForInit(wasmUrl: URL): Promise<unknown> {
    // wasm-bindgen's `--target web` glue uses `fetch(new URL(..., import.meta.url))`.
    // In Node (Vitest) `fetch(file://...)` is not supported, so we pre-read the
    // `.wasm` bytes from disk and pass them to the init function directly.
    if (wasmUrl.protocol === "file:") {
        // Keep the dynamic imports opaque to Vite/Rollup so browser builds don't
        // try to resolve Node builtins.
        const fsPromises = "node:fs/promises";
        const nodeUrl = "node:url";
        const { readFile } = await import(/* @vite-ignore */ fsPromises);
        const { fileURLToPath } = await import(/* @vite-ignore */ nodeUrl);
        return await readFile(fileURLToPath(wasmUrl));
    }
    return undefined;
}

async function initWasmBindgenModule(
    mod: RawWasmModule,
    wasmUrl: URL,
    options: { variant: WasmVariant; module?: WebAssembly.Module; memory?: WebAssembly.Memory },
): Promise<void> {
    const initFn = mod.default;
    if (typeof initFn !== "function") {
        throw new Error("WASM package does not export a default wasm-bindgen init function.");
    }

    const { module, memory, variant } = options;
    let urlInputPromise: Promise<unknown> | null = null;
    const resolveUrlInput = async (): Promise<unknown> => {
        if (!urlInputPromise) {
            urlInputPromise = resolveWasmInputForInit(wasmUrl);
        }
        return await urlInputPromise;
    };

    const doInit = async (input: unknown): Promise<void> => {
        if (!memory) {
            await initFn(input);
            return;
        }

        // wasm-bindgen's `--import-memory` builds typically expose an init signature
        // like `default(input?, memory?)`. Unfortunately, the exact generated
        // signature has varied across wasm-bindgen versions.
        //
        // Robust strategy:
        // - Prefer calling `default(undefined, memory)` (or `default(input, memory)`
        //   in Node where we must pass bytes to avoid `fetch(file://...)`).
        // - Regardless of the signature, ensure the final instantiation sees the
        //   desired memory by patching `WebAssembly.instantiate*` to force
        //   `imports.env.memory = memory` before instantiation.
        await withPatchedMemoryImport(memory, async () => {
            try {
                // Preferred call shape (wasm-bindgen import-memory builds often use
                // `default(input?, memory?)`).
                await initFn(input, memory);
            } catch (err) {
                // Some wasm-bindgen outputs ignore/validate the extra argument.
                // Retrying with `default(input)` still uses the patched import object
                // (so the provided memory is wired up) but avoids "wrong number of
                // arguments" style failures.
                try {
                    await initFn(input);
                } catch {
                    throw err;
                }
            }
        });
    };

    if (module) {
        try {
            await doInit(module);
            return;
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            warnOnce(
                `wasm:init:${variant}:module`,
                `[wasm] ${variant} init with precompiled module failed; falling back to default loader. Error: ${message}`,
            );
            await doInit(await resolveUrlInput());
            return;
        }
    }

    await doInit(await resolveUrlInput());
}

function tryExtractWasmMemory(mod: RawWasmModule): WebAssembly.Memory | undefined {
    const candidates = [mod.memory, mod.__wbg_memory, mod.__wbindgen_memory, mod.wasm?.memory];
    for (const c of candidates) {
        if (c instanceof WebAssembly.Memory) return c;
    }
    return undefined;
}

async function loadSingle(options: WasmInitOptions): Promise<WasmLoadResult> {
    const resolved = resolveWasmImporter("single");
    if (!resolved) {
        throw new Error(
            [
                "Missing single-thread WASM package.",
                "",
                "Build it with (from the repo root):",
                "  npm run wasm:build:single",
                "Or a faster dev build:",
                "  npm run wasm:build:dev",
                "",
                "Or build both variants:",
                "  npm run wasm:build",
            ].join("\n"),
        );
    }
    const mod = (await resolved.importer()) as RawWasmModule;
    await initWasmBindgenModule(mod, resolved.wasmUrl, {
        variant: "single",
        module: options.module,
        memory: options.memory,
    });
    return { api: toApi(mod), memory: options.memory ?? tryExtractWasmMemory(mod) };
}

async function loadThreaded(options: WasmInitOptions): Promise<WasmLoadResult> {
    const resolved = resolveWasmImporter("threaded");
    if (!resolved) {
        throw new Error(
            [
                "Missing threaded WASM package.",
                "",
                "Build it with (from the repo root):",
                "  npm run wasm:build:threaded",
                "Or a faster dev build:",
                "  node web/scripts/build_wasm.mjs threaded dev",
                "",
                "Or build both variants:",
                "  npm run wasm:build",
            ].join("\n"),
        );
    }
    const mod = (await resolved.importer()) as RawWasmModule;
    await initWasmBindgenModule(mod, resolved.wasmUrl, {
        variant: "threaded",
        module: options.module,
        memory: options.memory,
    });
    return { api: toApi(mod), memory: options.memory ?? tryExtractWasmMemory(mod) };
}

export async function initWasm(options: WasmInitOptions = {}): Promise<WasmInitResult> {
    const requested = options.variant ?? "auto";
    const threadSupport = detectThreadSupport();
    const moduleVariantHint = options.module ? lookupPrecompiledWasmModuleVariant(options.module) : undefined;

    if (requested === "auto" && moduleVariantHint) {
        if (moduleVariantHint === "single") {
            const loaded = await loadSingle(options);
            return {
                api: loaded.api,
                variant: "single",
                reason: "Using single-threaded WASM because the provided WebAssembly.Module was precompiled for it.",
                memory: describeMemory(loaded.memory),
            };
        }

        // `precompileWasm("threaded")` can run in environments that cannot *instantiate* the
        // shared-memory module (e.g. browsers without COOP/COEP). Treat the hint as a
        // preference, not a hard requirement.
        if (threadSupport.supported) {
            try {
                const loaded = await loadThreaded(options);
                return {
                    api: loaded.api,
                    variant: "threaded",
                    reason: `Using threaded WASM because the provided WebAssembly.Module was precompiled for it. (${threadSupport.reason})`,
                    memory: describeMemory(loaded.memory),
                };
            } catch (err) {
                const message = err instanceof Error ? err.message : String(err);
                const loaded = await loadSingle({ ...options, module: undefined });
                return {
                    api: loaded.api,
                    variant: "single",
                    reason: `Threaded WASM init failed (precompiled module provided); falling back to single. Error: ${message}`,
                    memory: describeMemory(loaded.memory),
                };
            }
        }

        const loaded = await loadSingle({ ...options, module: undefined });
        return {
            api: loaded.api,
            variant: "single",
            reason: `Threaded precompiled module provided but the current runtime cannot use shared-memory WebAssembly; falling back to single. Reason: ${threadSupport.reason}`,
            memory: describeMemory(loaded.memory),
        };
    }

    if (requested === "threaded") {
        if (!threadSupport.supported) {
            throw new Error(
                [
                    "Threaded WASM build requested but the current runtime cannot use shared-memory WebAssembly.",
                    `Reason: ${threadSupport.reason}`,
                    "",
                    "To enable the threaded build in browsers you must serve the page with COOP/COEP headers so that",
                    "`crossOriginIsolated === true`, e.g.:",
                    "  Cross-Origin-Opener-Policy: same-origin",
                    "  Cross-Origin-Embedder-Policy: require-corp",
                ].join("\n"),
            );
        }

        const loaded = await loadThreaded(options);
        return {
            api: loaded.api,
            variant: "threaded",
            reason: threadSupport.reason,
            memory: describeMemory(loaded.memory),
        };
    }

    if (requested === "single") {
        const loaded = await loadSingle(options);
        return {
            api: loaded.api,
            variant: "single",
            reason: "Forced via initWasm({ variant: 'single' })",
            memory: describeMemory(loaded.memory),
        };
    }

    if (threadSupport.supported) {
        try {
            const loaded = await loadThreaded(options);
            return {
                api: loaded.api,
                variant: "threaded",
                reason: threadSupport.reason,
                memory: describeMemory(loaded.memory),
            };
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            const loaded = await loadSingle(options);
            return {
                api: loaded.api,
                variant: "single",
                reason: `Threaded WASM init failed; falling back to single. Error: ${message}`,
                memory: describeMemory(loaded.memory),
            };
        }
    }

    const loaded = await loadSingle(options);
    return { api: loaded.api, variant: "single", reason: threadSupport.reason, memory: describeMemory(loaded.memory) };
}
