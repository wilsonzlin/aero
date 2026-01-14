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

export type AerogpuSubmission = {
    cmdStream: Uint8Array;
    signalFence: bigint;
    contextId: number;
    engineId: number;
    flags: number;
    allocTable: Uint8Array | null;
};

/**
 * Canonical full-system VM handle (`aero_machine::Machine`).
 *
 * This is the browser-facing, wasm-bindgen-exported machine wrapper from `crates/aero-wasm`.
 */
export type MachineHandle = {
    /**
     * Number of vCPUs configured for this machine.
     *
     * Optional for older WASM builds.
     */
    cpu_count?(): number;
    /**
     * Set the SMBIOS System UUID seed used by firmware.
     *
     * This only takes effect after the next {@link reset}.
     *
     * Optional for older WASM builds.
     */
    set_smbios_uuid_seed?(seed: bigint): void;
    reset(): void;
    /**
     * Set the BIOS boot drive number (`DL`) used when transferring control to the boot sector.
     *
     * Recommended values:
     * - `0x80`: primary HDD (normal boot)
     * - `0xE0`: ATAPI CD-ROM (El Torito install media)
     *
     * Note: this selection is consumed during BIOS POST/boot. Call {@link reset} after changing it
     * to re-run POST with the new value.
     *
     * Optional for older WASM builds.
     */
    set_boot_drive?(drive: number): void;
    /**
     * Set the preferred BIOS boot device for the next boot attempt.
     *
     * This is a convenience wrapper over {@link set_boot_drive}:
     * - `MachineBootDevice.Hdd` -> `DL=0x80`
     * - `MachineBootDevice.Cdrom` -> `DL=0xE0`
     *
     * Optional for older WASM builds.
     */
    set_boot_device?(device: number): void;
    /**
     * Returns the configured boot device preference.
     *
     * Optional for older WASM builds.
     */
    boot_device?(): number;
    /**
     * Returns the effective boot device used for the current boot session.
     *
     * When the firmware "CD-first when present" policy is enabled, this reflects what firmware
     * actually booted from (CD vs HDD), rather than just the configured preference.
     *
     * Optional for older WASM builds.
     */
    active_boot_device?(): number;
    /**
     * Returns the configured BIOS boot drive number (`DL`) used for firmware POST/boot.
     *
     * Recommended values:
     * - `0x80`: primary HDD (normal boot)
     * - `0xE0`: ATAPI CD-ROM (El Torito install media)
     *
     * Optional for older WASM builds.
     */
    boot_drive?(): number;
    /**
     * Enable/disable the firmware "CD-first when present" boot policy.
     *
     * When enabled and install media is attached, BIOS POST attempts to boot from CD-ROM first
     * and falls back to the configured `boot_drive` on failure.
     *
     * This only takes effect after the next {@link reset}.
     *
     * Optional for older WASM builds.
     */
    set_boot_from_cd_if_present?(enabled: boolean): void;
    /**
     * Returns whether the firmware "CD-first when present" boot policy is enabled.
     *
     * Optional for older WASM builds.
     */
    boot_from_cd_if_present?(): boolean;
    /**
     * Set the BIOS CD-ROM drive number used when booting under the "CD-first when present" policy.
     *
     * Valid El Torito CD-ROM drive numbers are `0xE0..=0xEF` (recommended `0xE0` for first CD-ROM).
     *
     * This only takes effect after the next {@link reset}.
     *
     * Optional for older WASM builds.
     */
    set_cd_boot_drive?(drive: number): void;
    /**
     * Returns the BIOS CD-ROM drive number used when booting under the "CD-first when present"
     * policy.
     *
     * Optional for older WASM builds.
     */
    cd_boot_drive?(): number;
    set_disk_image(bytes: Uint8Array): void;
    /**
     * Open (or create) an OPFS-backed disk image and attach it as the machine's canonical disk.
     *
     * This enables large disk images without loading the entire contents into RAM.
     *
     * Optional for older WASM builds.
     */
    set_disk_opfs?(path: string, create: boolean, sizeBytes: bigint, baseFormat?: string): Promise<void>;
    /**
     * Open (or create) an OPFS-backed disk image, attach it as the machine's canonical disk, and set
     * the snapshot overlay reference (`DISKS` entry) for `disk_id=0`.
     *
     * Optional for older WASM builds.
     */
    set_disk_opfs_and_set_overlay_ref?(path: string, create: boolean, sizeBytes: bigint, baseFormat?: string): Promise<void>;
    /**
     * Open (or create) an OPFS-backed disk image and attach it as the machine's canonical disk,
     * reporting create/resize progress via a callback.
     *
     * The callback is invoked with a numeric progress value in `[0.0, 1.0]`.
     *
     * Optional for older WASM builds.
     */
    set_disk_opfs_with_progress?(
        path: string,
        create: boolean,
        sizeBytes: bigint,
        progress: (progress: number) => void,
    ): Promise<void>;
    /**
     * Like {@link set_disk_opfs_with_progress}, but also records `base_image=path, overlay_image=\"\"`
     * in the snapshot `DISKS` overlay refs (`disk_id=0`).
     *
     * Optional for older WASM builds.
     */
    set_disk_opfs_with_progress_and_set_overlay_ref?(
        path: string,
        create: boolean,
        sizeBytes: bigint,
        progress: (progress: number) => void,
    ): Promise<void>;
    /**
     * Open an existing OPFS-backed disk image (using the file's current size) and attach it as
     * the machine's canonical disk.
     *
     * Optional for older WASM builds.
     */
    set_disk_opfs_existing?(path: string, baseFormat?: string, expectedSizeBytes?: bigint): Promise<void>;
    /**
     * Create an OPFS-backed copy-on-write disk: `base_path` (read-only, any supported format) +
     * `overlay_path` (writable Aero sparse overlay).
     *
     * Optional for older WASM builds.
     */
    set_disk_cow_opfs_create?(
        basePath: string,
        overlayPath: string,
        overlayBlockSizeBytes: number,
    ): Promise<void>;
    /**
     * Like {@link set_disk_cow_opfs_create}, but also records `base_image`/`overlay_image` into the
     * snapshot `DISKS` overlay refs (`disk_id=0`).
     *
     * Optional for older WASM builds.
     */
    set_disk_cow_opfs_create_and_set_overlay_ref?(
        basePath: string,
        overlayPath: string,
        overlayBlockSizeBytes: number,
    ): Promise<void>;
    /**
     * Open an existing OPFS-backed copy-on-write disk: `base_path` (read-only) + `overlay_path`
     * (existing writable Aero sparse overlay).
     *
     * Optional for older WASM builds.
     */
    set_disk_cow_opfs_open?(basePath: string, overlayPath: string): Promise<void>;
    /**
     * Like {@link set_disk_cow_opfs_open}, but also records `base_image`/`overlay_image` into the
     * snapshot `DISKS` overlay refs (`disk_id=0`).
     *
     * Optional for older WASM builds.
     */
    set_disk_cow_opfs_open_and_set_overlay_ref?(basePath: string, overlayPath: string): Promise<void>;
    /**
     * Attach the canonical primary HDD (`disk_id=0`, AHCI port 0) as an OPFS base disk plus an
     * aerosparse copy-on-write overlay (both in OPFS).
     *
     * The overlay image is created if missing and must match the base disk size (and
     * `overlayBlockSizeBytes`) when it already exists.
     *
     * Optional for older WASM builds.
     */
    set_primary_hdd_opfs_cow?(
        basePath: string,
        overlayPath: string,
        overlayBlockSizeBytes: number,
    ): Promise<void>;
    /**
     * Open an existing OPFS-backed disk image (using the file's current size) and attach it as
     * the canonical primary HDD (AHCI port 0), *without* creating a copy-on-write overlay.
     *
     * This also sets the snapshot disk overlay ref for `disk_id=0` to
     * `{ base_image: path, overlay_image: "" }` so snapshot restore flows can reattach the same
     * disk backend.
     *
     * Optional for older WASM builds.
     */
    set_primary_hdd_opfs_existing?(path: string): Promise<void>;
    /**
     * Open an existing OPFS-backed disk image and attach it as the machine's canonical disk,
     * recording snapshot overlay refs (DISKS entry) for `disk_id=0`.
     *
     * Optional for older WASM builds.
     */
    set_disk_opfs_existing_and_set_overlay_ref?(
        path: string,
        baseFormat?: string,
        expectedSizeBytes?: bigint,
    ): Promise<void>;
    /**
     * Open an existing OPFS-backed Aero sparse disk (`.aerospar`) and attach it as the machine's
     * canonical disk.
     *
     * Optional for older WASM builds.
     */
    set_disk_aerospar_opfs_open?(path: string): Promise<void>;
    /**
     * Create a new OPFS-backed Aero sparse disk (`.aerospar`) and attach it as the machine's
     * canonical disk.
     *
     * Notes:
     * - `diskSizeBytes` is the guest-visible disk capacity.
     * - `blockSizeBytes` must be a power-of-two multiple of 512.
     *
     * Optional for older WASM builds.
     */
    set_disk_aerospar_opfs_create?(path: string, diskSizeBytes: bigint, blockSizeBytes: number): Promise<void>;
    /**
     * Like {@link set_disk_aerospar_opfs_create}, but also records `base_image=path, overlay_image=\"\"`
     * in the snapshot `DISKS` overlay refs (`disk_id=0`).
     *
     * Optional for older WASM builds.
     */
    set_disk_aerospar_opfs_create_and_set_overlay_ref?(
        path: string,
        diskSizeBytes: bigint,
        blockSizeBytes: number,
    ): Promise<void>;
    /**
     * Open an existing OPFS-backed Aero sparse disk (`.aerospar`) and attach it as the machine's
     * canonical disk, recording snapshot overlay refs (DISKS entry) for `disk_id=0`.
     *
     * Optional for older WASM builds.
     */
    set_disk_aerospar_opfs_open_and_set_overlay_ref?(path: string): Promise<void>;
    /**
     * Open (or create) an OPFS-backed disk image and attach it as the canonical Windows 7 IDE
     * primary channel master ATA disk (`disk_id=2`).
     *
     * Optional for older WASM builds.
     */
    attach_ide_primary_master_disk_opfs?(
        path: string,
        create: boolean,
        sizeBytes: bigint,
    ): Promise<void>;
    /**
     * Open (or create) an OPFS-backed disk image and attach it as the canonical Windows 7 IDE
     * primary channel master ATA disk (`disk_id=2`), reporting create/resize progress via a
     * callback.
     *
     * The callback is invoked with a numeric progress value in `[0.0, 1.0]`.
     *
     * Optional for older WASM builds.
     */
    attach_ide_primary_master_disk_opfs_with_progress?(
        path: string,
        create: boolean,
        sizeBytes: bigint,
        progress: (progress: number) => void,
    ): Promise<void>;
    /**
     * Like {@link attach_ide_primary_master_disk_opfs}, but also records `base_image=path, overlay_image=\"\"`
     * in the snapshot `DISKS` overlay refs (`disk_id=2`).
     *
     * Optional for older WASM builds.
     */
    attach_ide_primary_master_disk_opfs_and_set_overlay_ref?(
        path: string,
        create: boolean,
        sizeBytes: bigint,
    ): Promise<void>;
    /**
     * Like {@link attach_ide_primary_master_disk_opfs_with_progress}, but also records the snapshot
     * overlay ref (`disk_id=2`).
     *
     * Optional for older WASM builds.
     */
    attach_ide_primary_master_disk_opfs_with_progress_and_set_overlay_ref?(
        path: string,
        create: boolean,
        sizeBytes: bigint,
        progress: (progress: number) => void,
    ): Promise<void>;
    /**
     * Open an existing OPFS-backed disk image (using the file's current size) and attach it as the
     * canonical Windows 7 IDE primary channel master ATA disk (`disk_id=2`).
     *
     * Optional for older WASM builds.
     */
    attach_ide_primary_master_disk_opfs_existing?(path: string): Promise<void>;
    /**
     * Like {@link attach_ide_primary_master_disk_opfs_existing}, but also records `base_image=path, overlay_image=\"\"`
     * in the snapshot `DISKS` overlay refs (`disk_id=2`).
     *
     * Optional for older WASM builds.
     */
    attach_ide_primary_master_disk_opfs_existing_and_set_overlay_ref?(path: string): Promise<void>;
    /**
     * Open an existing OPFS-backed ISO image (using the file's current size) and attach it as the
     * canonical Windows 7 IDE secondary channel master ATAPI CD-ROM (`disk_id=1`).
     *
     * Optional for older WASM builds.
     */
    attach_ide_secondary_master_iso_opfs_existing?(path: string): Promise<void>;
    /**
     * Open an existing OPFS-backed ISO image, attach it as the canonical install media CD-ROM, and
     * record snapshot overlay refs (DISKS entry) for `disk_id=1`.
     *
     * Optional for older WASM builds.
     */
    attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref?(path: string): Promise<void>;
    run_slice(maxInsts: number): { kind: number; executed: number; detail: string; free(): void };
    serial_output(): Uint8Array;
    /**
     * Returns the number of bytes currently buffered in the serial output log.
     *
     * Optional for older WASM builds.
     */
    serial_output_len?(): number;

    /**
     * BIOS-reported VBE linear framebuffer (LFB) base address.
     *
     * Optional for older WASM builds.
     */
    vbe_lfb_base?(): number;
    /**
     * Returns whether the canonical AeroGPU PCI function (`00:07.0`, `A3A0:0001`) is present.
     *
     * Optional for older WASM builds.
     */
    aerogpu_present?(): boolean;
    /**
     * Return the base address assigned to an AeroGPU PCI BAR.
     *
     * Returns 0 when AeroGPU is not present or when the BAR is missing/unassigned.
     *
     * Optional for older WASM builds.
     */
    aerogpu_bar_base?(bar: number): number;
    /**
     * Drain newly-decoded AeroGPU submissions from the in-process device model.
     *
     * Calling this enables the "submission bridge" in the AeroGPU device model: subsequent
     * submissions will no longer complete fences automatically, and callers must invoke
     * {@link aerogpu_complete_fence} for forward progress.
     *
     * Optional for older WASM builds.
     */
    aerogpu_drain_submissions?(): AerogpuSubmission[];
    /**
     * Mark a previously-drained submission's fence as complete.
     *
     * Optional for older WASM builds.
     */
    aerogpu_complete_fence?(fence: bigint): void;

    /**
     * Unified display scanout (boot display + WDDM / modern scanout).
     *
     * Prefer these APIs over the legacy {@link vga_present} exports when available.
     *
     * Call {@link display_present} before reading the framebuffer pointer/length or before sampling
     * {@link display_width}/{@link display_height} after the guest changes display modes.
     *
     * Optional for older WASM builds.
     */
    display_present?(): void;
    /** Current display output width in pixels (0 when the display is not present). */
    display_width?(): number;
    /** Current display output height in pixels (0 when the display is not present). */
    display_height?(): number;
    /** Byte stride of a single scanline in the returned framebuffer (RGBA8888). */
    display_stride_bytes?(): number;
    /**
     * Pointer (byte offset) into WASM linear memory for the current display front buffer.
     *
     * The buffer is RGBA8888 (`width * height * 4` bytes). Pair with {@link display_framebuffer_len_bytes}.
     *
     * Safety/validity: callers must re-query both the pointer and length after each
     * {@link display_present} call because the underlying Rust framebuffer cache may be resized or
     * reallocated, invalidating previously returned pointers.
     */
    display_framebuffer_ptr?(): number;
    /** Length in bytes of the current display front buffer (RGBA8888). */
    display_framebuffer_len_bytes?(): number;
    /**
     * Convenience helper: copy the framebuffer bytes into JS (RGBA8888).
     *
     * Slower than using {@link display_framebuffer_ptr} + {@link display_framebuffer_len_bytes}.
     *
     * Note: this calls {@link display_present} internally, so it can invalidate any previously
     * returned {@link display_framebuffer_ptr} pointer.
     */
    display_framebuffer_copy_rgba8888?(): Uint8Array;

    /**
     * VGA/SVGA scanout (BIOS text mode + VBE graphics).
     *
     * Call {@link vga_present} before reading the framebuffer pointer/length or before sampling
     * {@link vga_width}/{@link vga_height} after the guest changes video modes.
     *
     * Optional for older WASM builds.
     */
    vga_present?(): void;
    /** Current VGA output width in pixels (0 when VGA is not present). */
    vga_width?(): number;
    /** Current VGA output height in pixels (0 when VGA is not present). */
    vga_height?(): number;
    /** Byte stride of a single scanline in the returned framebuffer (RGBA8888, tightly packed). */
    vga_stride_bytes?(): number;
    /**
     * Pointer (byte offset) into WASM linear memory for the current VGA front buffer.
     *
     * The buffer is RGBA8888 (`width * height * 4` bytes). Pair with {@link vga_framebuffer_len_bytes}.
     *
     * Safety/validity: callers must re-query both the pointer and length after each
     * {@link vga_present} call because the underlying Rust framebuffer may be swapped/resized,
     * invalidating previously returned pointers.
     */
    vga_framebuffer_ptr?(): number;
    /** Length in bytes of the current VGA front buffer (RGBA8888). */
    vga_framebuffer_len_bytes?(): number;
    /**
     * Convenience helper: copy the framebuffer bytes into JS (RGBA8888).
     *
     * Slower than using {@link vga_framebuffer_ptr} + {@link vga_framebuffer_len_bytes}.
     *
     * Note: this calls {@link vga_present} internally, so it can invalidate any previously
     * returned {@link vga_framebuffer_ptr} pointer.
     */
    vga_framebuffer_copy_rgba8888?(): Uint8Array;
    /**
     * Legacy helper: copy the framebuffer bytes into JS (RGBA8888).
     *
     * Optional for older WASM builds.
     */
    vga_framebuffer_rgba8888_copy?(): Uint8Array | null;

    /**
     * Shared scanout state descriptor (e.g. legacy VGA/VBE vs AeroGPU WDDM scanout selection).
     *
     * When present, these return a pointer/length pair describing an `Int32Array`-backed scanout
     * state structure stored inside the module's linear memory (typically a `SharedArrayBuffer`
     * in the threaded build). See `web/src/ipc/scanout_state.ts` for the layout contract.
     *
     * Optional for older WASM builds and for builds that do not expose scanout state via linear
     * memory (callers should feature-detect).
     */
    scanout_state_ptr?(): number;
    scanout_state_len_bytes?(): number;

    /**
     * Shared hardware cursor state descriptor (AeroGPU cursor registers + cursor surface pointer).
     *
     * When present, these return a pointer/length pair describing an `Int32Array`-backed cursor
     * state structure stored inside the module's linear memory (typically a `SharedArrayBuffer`
     * in the threaded build). See `web/src/ipc/cursor_state.ts` for the layout contract.
     *
     * Optional for older WASM builds and for builds that do not expose cursor state via linear
     * memory (callers should feature-detect).
     */
    cursor_state_ptr?(): number;
    cursor_state_len_bytes?(): number;
    inject_browser_key(code: string, pressed: boolean): void;
    /**
     * Inject up to 4 raw PS/2 Set-2 scancode bytes.
     *
     * Format matches `InputEventType.KeyScancode` (`web/src/input/event_queue.ts`):
     * - `packed`: little-endian packed bytes (b0 in bits 0..7)
     * - `len`: number of valid bytes (1..=4)
     *
     * Optional for older WASM builds.
     */
    inject_key_scancode_bytes?(packed: number, len: number): void;
    /**
     * Inject an arbitrary-length raw PS/2 Set-2 scancode byte sequence.
     *
     * Optional for older WASM builds.
     */
    inject_keyboard_bytes?(bytes: Uint8Array): void;
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
     * - `3`: back
     * - `4`: forward
     *
     * Tip: when available, prefer `api.MouseButton.Left/Middle/Right/Back/Forward` for a stable
     * mapping reference.
     */
    inject_mouse_button?(button: number, pressed: boolean): void;
    /**
     * Set all mouse buttons at once using a bitmask matching DOM `MouseEvent.buttons`:
     * - bit0 (`0x01`): left
     * - bit1 (`0x02`): right
     * - bit2 (`0x04`): middle
     * - bit3 (`0x08`): back
     * - bit4 (`0x10`): forward
     *
     * Tip: when available, prefer building masks by OR'ing `api.MouseButtons.Left/Right/Middle/Back/Forward`.
     */
    inject_mouse_buttons_mask?(mask: number): void;
    /**
     * Set mouse button state using PS/2 packet bit conventions (also matches DOM `MouseEvent.buttons`):
     * - bit0 (`0x01`): left
     * - bit1 (`0x02`): right
     * - bit2 (`0x04`): middle
     * - bit3 (`0x08`): back / side (only emitted if the guest enabled IntelliMouse Explorer, device ID `0x04`)
     * - bit4 (`0x10`): forward / extra (same note as bit3)
     *
     * Optional for older WASM builds; prefer {@link inject_mouse_buttons_mask} (same mapping).
     */
    inject_ps2_mouse_buttons?(buttons: number): void;
    inject_mouse_left?(pressed: boolean): void;
    inject_mouse_right?(pressed: boolean): void;
    inject_mouse_middle?(pressed: boolean): void;
    inject_mouse_back?(pressed: boolean): void;
    inject_mouse_forward?(pressed: boolean): void;
    /**
     * Virtio-input injection helpers.
     *
     * Requires constructing the machine with virtio-input enabled (e.g.
     * `Machine.new_with_input_backends(..., enableVirtioInput=true, ...)` or
     * `Machine.new_with_options(..., { enable_virtio_input: true })`).
     *
     * These are safe to call even when virtio-input is disabled; they will no-op.
     *
     * Optional for older WASM builds.
     */
    inject_virtio_key?(linux_key: number, pressed: boolean): void;
    inject_virtio_rel?(dx: number, dy: number): void;
    inject_virtio_button?(btn: number, pressed: boolean): void;
    inject_virtio_wheel?(delta: number): void;
    inject_virtio_hwheel?(delta: number): void;
    inject_virtio_wheel2?(wheel: number, hwheel: number): void;
    // Newer, more explicit aliases (preferred for new code).
    inject_virtio_mouse_rel?(dx: number, dy: number): void;
    inject_virtio_mouse_button?(btn: number, pressed: boolean): void;
    inject_virtio_mouse_wheel?(delta: number): void;
    /** Whether the guest virtio-input keyboard driver has reached `DRIVER_OK`. */
    virtio_input_keyboard_driver_ok?(): boolean;
    /** Whether the guest virtio-input mouse driver has reached `DRIVER_OK`. */
    virtio_input_mouse_driver_ok?(): boolean;
    /**
     * Synthetic USB HID injection helpers (devices behind the external hub).
     *
     * Synthetic USB HID devices are enabled by default for `new Machine(ramSizeBytes)`.
     * To explicitly enable/disable them, use:
     * - `Machine.new_with_input_backends(..., enableSyntheticUsbHid=...)`, or
     * - `Machine.new_with_options(..., { enable_synthetic_usb_hid: ... })`.
     *
     * Optional for older WASM builds.
     */
    /** Whether the guest has configured the synthetic USB HID keyboard device (`SET_CONFIGURATION != 0`). */
    usb_hid_keyboard_configured?(): boolean;
    /** Whether the guest has configured the synthetic USB HID mouse device (`SET_CONFIGURATION != 0`). */
    usb_hid_mouse_configured?(): boolean;
    /** Whether the guest has configured the synthetic USB HID gamepad device (`SET_CONFIGURATION != 0`). */
    usb_hid_gamepad_configured?(): boolean;
    /** Whether the guest has configured the synthetic USB HID consumer-control device (`SET_CONFIGURATION != 0`). */
    usb_hid_consumer_control_configured?(): boolean;

    /**
     * Guest keyboard LED state helpers.
     *
     * All return the same HID-style LED bitmask layout:
     * - bit0: Num Lock
     * - bit1: Caps Lock
     * - bit2: Scroll Lock
     * - bit3: Compose
     * - bit4: Kana
     *
     * Optional for older WASM builds.
     */
    usb_hid_keyboard_leds?(): number;
    virtio_input_keyboard_leds?(): number;
    ps2_keyboard_leds?(): number;

    inject_usb_hid_keyboard_usage?(usage: number, pressed: boolean): void;
    /**
     * Inject a Consumer Control (HID Usage Page 0x0C) usage transition (media keys).
     *
     * Optional for older WASM builds.
     */
    inject_usb_hid_consumer_usage?(usage: number, pressed: boolean): void;
    /** Inject relative mouse motion (`dy > 0` = down, matches browser coordinates). */
    inject_usb_hid_mouse_move?(dx: number, dy: number): void;
    /** Set mouse button state (low bits match DOM `MouseEvent.buttons`). */
    inject_usb_hid_mouse_buttons?(mask: number): void;
    /** Set vertical mouse wheel delta (`delta > 0` = wheel up). */
    inject_usb_hid_mouse_wheel?(delta: number): void;
    /** Set horizontal mouse wheel delta (`delta > 0` = wheel right / AC Pan). */
    inject_usb_hid_mouse_hwheel?(delta: number): void;
    /**
     * Inject both vertical and horizontal wheel deltas in a single USB HID report.
     *
     * Optional for older WASM builds.
     */
    inject_usb_hid_mouse_wheel2?(wheel: number, hwheel: number): void;
    /**
     * Inject a packed gamepad report.
     *
     * Packing matches `web/src/input/gamepad.ts`.
     */
    inject_usb_hid_gamepad_report?(packedLo: number, packedHi: number): void;
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
     * Poll network devices and bridge frames to/from any attached network backend.
     *
     * Optional for older WASM builds.
     */
    poll_network?(): void;
    /**
     * Network backend statistics (if exposed by the WASM build).
     *
     * Optional for older WASM builds.
     */
    net_stats?():
        | {
              tx_pushed_frames: bigint;
              tx_pushed_bytes?: bigint;
              tx_dropped_oversize: bigint;
              tx_dropped_oversize_bytes?: bigint;
              tx_dropped_full: bigint;
              tx_dropped_full_bytes?: bigint;
              rx_popped_frames: bigint;
              rx_popped_bytes?: bigint;
              rx_dropped_oversize: bigint;
              rx_dropped_oversize_bytes?: bigint;
              rx_corrupt: bigint;
              rx_broken?: boolean;
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
    /**
     * Snapshot `DISKS` section support (disk overlay refs).
     *
     * Optional for older WASM builds.
     */
    reattach_restored_disks_from_opfs?(): Promise<void>;
    set_ahci_port0_disk_overlay_ref?(base_image: string, overlay_image: string): void;
    clear_ahci_port0_disk_overlay_ref?(): void;
    set_ide_secondary_master_atapi_overlay_ref?(base_image: string, overlay_image: string): void;
    clear_ide_secondary_master_atapi_overlay_ref?(): void;
    set_ide_primary_master_ata_overlay_ref?(base_image: string, overlay_image: string): void;
    clear_ide_primary_master_ata_overlay_ref?(): void;
    /**
     * Re-open OPFS-backed disk images referenced by snapshot `DISKS` overlay refs.
     *
     * Newer builds expose higher-level attachment helpers that understand the canonical Win7
     * storage topology (primary HDD + install media) and accept OPFS *path strings* (relative to
     * `navigator.storage.getDirectory()`).
     *
     * Optional for older WASM builds.
     */
    /**
     * Attach an ISO image (raw bytes) as the canonical install media / ATAPI CD-ROM (`disk_id=1`).
     *
     * This copies the ISO into WASM memory; for large ISOs, prefer OPFS-backed attachment via
     * {@link attach_install_media_iso_opfs}.
     *
     * Optional for older WASM builds.
     */
    attach_install_media_iso_bytes?(bytes: Uint8Array): void;
    /**
     * Back-compat alias: attach an in-memory ISO as the canonical install media CD-ROM.
     *
     * Prefer {@link attach_install_media_iso_bytes}.
     *
     * Optional for older WASM builds.
     */
    set_cd_image?(bytes: Uint8Array): void;
    /**
     * Attach an existing OPFS-backed ISO image as the canonical install media CD-ROM (`disk_id=1`).
     *
     * Newer wasm builds export this as `attach_install_media_iso_opfs`; older builds used
     * `attach_install_media_opfs_iso`, and some builds expose an `_existing` suffix variant.
     * Accept all for back-compat.
     */
    attach_install_media_iso_opfs?(path: string): Promise<void>;
    /**
     * Legacy/alternate naming for attaching an existing OPFS-backed ISO image as canonical install
     * media (`disk_id=1`).
     *
     * Some wasm builds expose this as an `_existing` suffix variant.
     *
     * Optional for older WASM builds.
     */
    attach_install_media_iso_opfs_existing?(path: string): Promise<void>;
    attach_install_media_opfs_iso?(path: string): Promise<void>;
    /**
     * Back-compat alias: attach an existing OPFS-backed ISO as the canonical install media CD-ROM.
     *
     * Prefer {@link attach_install_media_opfs_iso} / {@link attach_install_media_iso_opfs}.
     *
     * Optional for older WASM builds.
     */
    set_cd_opfs_existing?(path: string): Promise<void>;
    /**
     * Attach an existing OPFS-backed ISO image as install media and set the snapshot overlay ref
     * (`DISKS` entry) for `disk_id=1` in one call.
     *
     * Optional for older WASM builds.
     */
    attach_install_media_iso_opfs_and_set_overlay_ref?(path: string): Promise<void>;
    /**
     * Legacy/alternate naming for attaching an existing OPFS-backed ISO image as install media and
     * setting the snapshot overlay ref (`DISKS` entry) for `disk_id=1` in one call.
     *
     * Some wasm builds expose this as an `_existing` suffix variant.
     *
     * Optional for older WASM builds.
     */
    attach_install_media_iso_opfs_existing_and_set_overlay_ref?(path: string): Promise<void>;
    /**
     * Attach an existing OPFS-backed ISO image as install media, preserving guest-visible ATAPI
     * media state (intended for snapshot restore flows).
     *
     * Optional for older WASM builds.
     */
    attach_install_media_iso_opfs_for_restore?(path: string): Promise<void>;
    /**
     * Attach an existing OPFS-backed ISO image as install media for restore flows and set the
     * snapshot overlay ref (`DISKS` entry) for `disk_id=1`.
     *
     * Optional for older WASM builds.
     */
    attach_install_media_iso_opfs_for_restore_and_set_overlay_ref?(path: string): Promise<void>;
    /**
     * Eject/detach the canonical install media (IDE secondary master ATAPI, `disk_id=1`) and clear its snapshot overlay ref.
     *
     * Optional for older WASM builds.
     */
    eject_install_media?(): void;
    take_restored_disk_overlays?():
        | {
              disk_id: number;
              base_image: string;
              overlay_image: string;
          }[]
        | null;
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

/**
 * Guest-visible xHCI controller bridge handle.
 *
 * Stepping contract:
 * - The JS PCI wrapper treats one "frame" as 1ms of guest USB time.
 * - Prefer {@link step_frames} for deterministic, batched stepping.
 * - {@link step_frame} steps exactly one 1ms frame.
 * - Legacy aliases may be present for older call sites/builds:
 *   - {@link tick} steps `frames` 1ms frames.
 *   - {@link tick_1ms} steps exactly one 1ms frame.
 * - `poll()` (if present) must not advance time; it should only process already-due work and can
 *   be treated as `step_frames(0)` by legacy wrappers.
 */
export type XhciControllerBridgeHandle = {
    mmio_read(offset: number, size: number): number;
    mmio_write(offset: number, size: number, value: number): void;
    step_frames?(frames: number): void;
    step_frame?(): void;
    tick_1ms?(): void;
    tick?(frames: number): void;
    poll?(): void;
    irq_asserted(): boolean;
    /**
     * Update the device model's PCI command register (offset 0x04, low 16 bits).
     *
     * Optional for older WASM builds.
     */
    set_pci_command?(command: number): void;
    save_state?(): Uint8Array;
    load_state?(bytes: Uint8Array): void;
    /**
     * Optional guest USB topology management helpers.
     *
     * These are used to hotplug hubs and passthrough HID devices into the guest USB tree.
     *
     * Optional for older WASM builds.
     */
    attach_hub?: (rootPort: number, portCount: number) => void;
    detach_at_path?: (path: number[]) => void;
    attach_webhid_device?: (path: number[], device: unknown) => void;
    attach_usb_hid_passthrough_device?: (path: number[], device: unknown) => void;
    /**
     * Optional WebUSB passthrough device helpers.
     *
     * The passthrough device emits `UsbHostAction`s that must be executed by the browser, and the
     * results pushed back to the device via {@link push_completion}.
     *
     * Optional for older WASM builds. When present, these match the UHCI passthrough contract
     * (`UsbPassthroughBridgeLike`).
     */
    set_connected?: (connected: boolean) => void;
    drain_actions?: () => UsbHostAction[] | null;
    push_completion?: (completion: UsbHostCompletion) => void;
    reset?: () => void;
    pending_summary?: () => {
        queued_actions: number;
        queued_completions: number;
        inflight_control?: number | null;
        inflight_endpoints: number;
    } | null;
    /**
     * Deterministic USB device/controller snapshot bytes.
     *
     * Optional for older WASM builds.
     */
    snapshot_state?: () => Uint8Array;
    restore_state?: (bytes: Uint8Array) => void;
    free(): void;
};

/**
 * Guest-visible EHCI controller bridge handle.
 *
 * EHCI uses an MMIO BAR (unlike UHCI's I/O port space). Time is advanced in deterministic 1ms USB
 * frames via {@link step_frames}.
 */
export type EhciControllerBridgeHandle = {
    /**
     * Guest RAM mapping base offset inside the module's linear memory.
     */
    readonly guest_base: number;
    /**
     * Guest RAM mapping size in bytes.
     */
    readonly guest_size: number;

    mmio_read(offset: number, size: number): number;
    mmio_write(offset: number, size: number, value: number): void;

    /**
     * Advance the controller by `frames` USB frames (1ms each).
     */
    step_frames(frames: number): void;

    /**
     * Convenience wrapper for stepping a single USB frame (1ms).
     *
     * Optional for older WASM builds.
     */
    step_frame?(): void;

    /**
     * Alias for {@link step_frame}.
     *
     * Optional for older WASM builds.
     */
    tick_1ms?(): void;

    irq_asserted(): boolean;

    /**
     * Update the device model's PCI command register (offset 0x04, low 16 bits).
     *
     * Optional for older WASM builds.
     */
    set_pci_command?(command: number): void;

    /**
     * WebUSB passthrough device helpers.
     *
     * The passthrough device is connected to a reserved EHCI root port and emits `UsbHostAction`s
     * that must be executed by the browser `UsbBroker`.
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

    /**
     * Deterministic controller snapshot bytes.
     *
     * Optional for older WASM builds.
     */
    save_state?(): Uint8Array;
    load_state?(bytes: Uint8Array): void;
    snapshot_state?: () => Uint8Array;
    restore_state?: (bytes: Uint8Array) => void;

    /**
     * USB topology helpers (host-side device attachment).
     */
    attach_hub(rootPort: number, portCount: number): void;
    detach_at_path(path: number[]): void;
    attach_webhid_device(path: number[], device: InstanceType<WasmApi["WebHidPassthroughBridge"]>): void;
    attach_usb_hid_passthrough_device(
        path: number[],
        device: InstanceType<NonNullable<WasmApi["UsbHidPassthroughBridge"]>>,
    ): void;

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
    /**
     * Legacy virtio-pci (0.9) I/O port register accessors (BAR2).
     *
     * Optional for older WASM builds and for modern-only devices. Some builds expose these as
     * `io_read`/`io_write` (retained for back-compat); newer builds also expose the preferred
     * `legacy_io_read`/`legacy_io_write` names.
     */
    legacy_io_read?(offset: number, size: number): number;
    legacy_io_write?(offset: number, size: number, value: number): void;
    io_read?(offset: number, size: number): number;
    io_write?(offset: number, size: number, value: number): void;
    poll?(): void;
    tick?(nowMs?: number): void;
    /**
     * Update the device model's PCI command register (offset 0x04, low 16 bits).
     *
     * Optional for older WASM builds.
     */
    set_pci_command?(command: number): void;
    irq_level?(): boolean;
    irq_asserted?(): boolean;
    /**
     * Best-effort stats for the underlying `NET_TX`/`NET_RX` ring backend (if supported by the WASM build).
     *
     * Optional for older WASM builds.
     */
    virtio_net_stats?():
        | {
              tx_pushed_frames: bigint;
              tx_pushed_bytes?: bigint;
              tx_dropped_oversize: bigint;
              tx_dropped_oversize_bytes?: bigint;
              tx_dropped_full: bigint;
              tx_dropped_full_bytes?: bigint;
              rx_popped_frames: bigint;
              rx_popped_bytes?: bigint;
              rx_dropped_oversize: bigint;
              rx_dropped_oversize_bytes?: bigint;
              rx_corrupt: bigint;
              rx_broken?: boolean;
          }
        | null;
    free(): void;
};

export type VirtioSndPciBridgeHandle = {
    mmio_read(offset: number, size: number): number;
    mmio_write(offset: number, size: number, value: number): void;
    /**
     * Legacy virtio-pci (0.9) I/O port register accessors (BAR2).
     *
     * Optional for older WASM builds and for modern-only devices.
     */
    legacy_io_read?(offset: number, size: number): number;
    legacy_io_write?(offset: number, size: number, value: number): void;
    io_read?(offset: number, size: number): number;
    io_write?(offset: number, size: number, value: number): void;
    poll(): void;
    /**
     * Update the device model's PCI command register (offset 0x04, low 16 bits).
     *
     * Optional for older WASM builds.
     */
    set_pci_command?(command: number): void;
    driver_ok(): boolean;
    irq_asserted(): boolean;
    /**
     * Attach/detach the shared AudioWorklet output ring buffer (producer side).
     *
     * `ringSab` is treated as an `Option` on the Rust side; pass `undefined` (or
     * `null` in older bindings) to detach.
     */
    set_audio_ring_buffer(ringSab: SharedArrayBuffer | null | undefined, capacityFrames: number, channelCount: number): void;
    set_host_sample_rate_hz(rate: number): void;
    /**
     * Attach/detach the shared microphone capture ring buffer (consumer side).
     *
     * `ringSab` is treated as an `Option` on the Rust side; pass `undefined` (or
     * `null` in older bindings) to detach.
     */
    set_mic_ring_buffer(ringSab?: SharedArrayBuffer | null): void;
    set_capture_sample_rate_hz(rate: number): void;
    free(): void;
};

/**
 * Options object accepted by `api.Machine.new_with_options(...)`.
 *
 * Keys use the wasm-bindgen/Rust snake_case naming (matching `MachineConfig` field names).
 *
 * Note: this type includes an index signature so callers can pass forward-compatible keys without
 * TypeScript rejecting them. Known keys are typed precisely.
 */
export interface MachineOptions {
    [key: string]: unknown;
    enable_pc_platform?: boolean;
    enable_acpi?: boolean;
    enable_e1000?: boolean;
    enable_virtio_net?: boolean;
    enable_virtio_blk?: boolean;
    enable_virtio_input?: boolean;
    enable_ahci?: boolean;
    enable_nvme?: boolean;
    enable_ide?: boolean;
    enable_uhci?: boolean;
    enable_ehci?: boolean;
    enable_xhci?: boolean;
    enable_synthetic_usb_hid?: boolean;
    enable_vga?: boolean;
    enable_aerogpu?: boolean;
    enable_serial?: boolean;
    enable_i8042?: boolean;
    enable_a20_gate?: boolean;
    enable_reset_ctrl?: boolean;
}

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
         readonly page_shift?: number;
         readonly page_size?: number;
         readonly page_offset_mask?: number;
         readonly jit_ctx_ram_base_offset?: number;
         readonly jit_ctx_tlb_salt_offset?: number;
         readonly jit_ctx_tlb_offset?: number;
         readonly jit_ctx_header_bytes?: number;
         readonly jit_ctx_total_bytes?: number;
         readonly jit_tlb_entries?: number;
         readonly jit_tlb_entry_bytes?: number;
         readonly jit_tlb_flag_read?: number;
         readonly jit_tlb_flag_write?: number;
         readonly jit_tlb_flag_exec?: number;
         readonly jit_tlb_flag_is_ram?: number;
         readonly tier2_ctx_offset?: number;
         readonly tier2_ctx_size?: number;
         readonly trace_exit_reason_offset?: number;
         readonly code_version_table_ptr_offset?: number;
         readonly code_version_table_len_offset?: number;
         readonly commit_flag_offset?: number;
         readonly commit_flag_bytes?: number;
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
     * Synchronous browser storage capability probes (OPFS + environment checks).
     *
     * This is intended to be called before attempting OPFS-backed disk/ISO attachment so the UI
     * can surface clearer diagnostics (e.g. "OPFS unsupported" vs "sync access handles require a worker").
     *
     * Optional for older WASM builds.
     */
    storage_capabilities?: () => {
        readonly opfsSupported: boolean;
        readonly opfsSyncAccessSupported: boolean;
        readonly isWorkerScope: boolean;
        readonly crossOriginIsolated: boolean;
        readonly sharedArrayBufferSupported: boolean;
        readonly isSecureContext: boolean;
    };

    /**
     * Legacy shared-guest-memory Machine factories (free-function exports).
     *
     * Newer WASM builds prefer static constructors on {@link Machine} (e.g. `Machine.new_shared` /
     * `Machine.new_win7_storage_shared`). Keep these optional for compatibility with older bundles
     * and intermediate refactors.
     */
    create_win7_machine_shared_guest_memory?: (guestBase: number, guestSize: number) => MachineHandle | Promise<MachineHandle>;
    create_machine_win7_shared_guest_memory?: (guestBase: number, guestSize: number) => MachineHandle | Promise<MachineHandle>;
    create_machine_shared_guest_memory_win7?: (guestBase: number, guestSize: number) => MachineHandle | Promise<MachineHandle>;

    /**
     * Tiered VM Tier-1 JIT ABI layout constants.
     *
     * Exposed so JS code can locate the per-call commit flag and inline TLB region without
     * duplicating Rust-side constants that may change over time.
     *
     * Optional while older WASM builds are still in circulation.
     */
    tiered_vm_jit_abi_layout?: () => {
        readonly jit_ctx_header_bytes: number;
        readonly jit_tlb_entries: number;
        readonly jit_tlb_entry_bytes: number;
        readonly tier2_ctx_bytes: number;
        readonly commit_flag_offset: number;
        readonly jit_ctx_ptr_offset?: number;
        readonly tier2_ctx_offset?: number;
        readonly trace_exit_reason_offset?: number;
        readonly code_version_table_ptr_offset?: number;
        readonly code_version_table_len_offset?: number;
    };

    /**
     * DOM-style mouse button IDs (mirrors `MouseEvent.button`).
     *
     * Optional for older WASM builds.
     */
    MouseButton?: WasmEnum<"Left" | "Middle" | "Right" | "Back" | "Forward">;
    /**
     * Mouse button bit values matching `MouseEvent.buttons` (bitmask).
     *
     * Optional for older WASM builds.
     */
    MouseButtons?: WasmEnum<"Left" | "Right" | "Middle" | "Back" | "Forward">;

    /**
     * Canonical machine BIOS boot device selection.
     *
     * Optional for older WASM builds.
     */
    MachineBootDevice?: WasmEnum<"Hdd" | "Cdrom">;

    /**
     * Guest-visible virtio-input device exposed via virtio-pci (BAR0 MMIO).
     */
    VirtioInputPciDevice?: new (
        guestBase: number,
        guestSize: number,
        kind: "keyboard" | "mouse",
        transportMode?: unknown,
    ) => {
        mmio_read(offset: number, size: number): number;
        mmio_write(offset: number, size: number, value: number): void;
        legacy_io_read?(offset: number, size: number): number;
        legacy_io_write?(offset: number, size: number, value: number): void;
        io_read?(offset: number, size: number): number;
        io_write?(offset: number, size: number, value: number): void;
        poll(): void;
        /**
         * Update the device model's PCI command register (offset 0x04, low 16 bits).
         *
         * Optional for older WASM builds.
         */
        set_pci_command?(command: number): void;
        driver_ok(): boolean;
        irq_asserted(): boolean;
        inject_key(linux_key: number, pressed: boolean): void;
        inject_rel(dx: number, dy: number): void;
        inject_button(btn: number, pressed: boolean): void;
        inject_wheel(delta: number): void;
        inject_hwheel?(delta: number): void;
        inject_wheel2?(wheel: number, hwheel: number): void;
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
            /**
             * Update the device model's PCI command register (offset 0x04, low 16 bits).
             *
             * Optional for older WASM builds.
             */
            set_pci_command?(command: number): void;
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
            /**
             * Update the device model's PCI command register (offset 0x04, low 16 bits).
             *
             * Optional for older WASM builds.
             */
            set_pci_command?(command: number): void;

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
     * Guest-visible EHCI controller bridge.
     *
     * Optional and has multiple constructor signatures depending on the deployed WASM build:
     * - `new (guestBase, guestSize)` for canonical builds (guestSize=0 means "use remainder of linear memory").
     * - older/experimental bindings may accept fewer args; callers should treat this as optional and feature-detect.
     */
    EhciControllerBridge?: {
        new (): EhciControllerBridgeHandle;
        new (guestBase: number): EhciControllerBridgeHandle;
        new (guestBase: number, guestSize: number): EhciControllerBridgeHandle;
    };

    /**
     * Guest-visible xHCI controller bridge.
     *
     * Stepping is deterministic and expressed in 1ms "frames" (USB frames). Prefer the UHCI-style
     * {@link XhciControllerBridgeHandle.step_frames} / {@link XhciControllerBridgeHandle.step_frame}
     * exports when available.
     *
     * Optional and has multiple constructor signatures depending on the deployed WASM build:
     * - `new (guestBase)` for legacy builds.
     * - `new (guestBase, guestSize)` for newer builds (guestSize=0 means "use remainder of linear memory").
     * - some wasm-bindgen glue versions can enforce constructor arity; callers may need to fall back to `new ()`.
     */
    XhciControllerBridge?: {
        new (): XhciControllerBridgeHandle;
        new (guestBase: number): XhciControllerBridgeHandle;
        new (guestBase: number, guestSize: number): XhciControllerBridgeHandle;
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
    VirtioNetPciBridge?: new (
        guestBase: number,
        guestSize: number,
        ioIpcSab: SharedArrayBuffer,
        transportMode?: unknown,
    ) => VirtioNetPciBridgeHandle;

    /**
     * Guest-visible virtio-snd PCI bridge (virtio-pci modern, BAR0 MMIO).
     *
     * Optional until all deployed WASM builds include virtio-snd support.
     */
    VirtioSndPciBridge?: new (
        guestBase: number,
        guestSize: number,
        transportMode?: unknown,
    ) => VirtioSndPciBridgeHandle;

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
        /**
         * Inject raw Set-2 scancode bytes into the keyboard output queue.
         *
         * Optional for older WASM builds.
         */
        inject_keyboard_bytes?(bytes: Uint8Array): void;
        inject_mouse_move(dx: number, dy: number): void;
        inject_mouse_buttons(buttons: number): void;
        inject_mouse_wheel(delta: number): void;
        /**
         * Inject PS/2 mouse motion + wheel.
         *
         * Optional for older WASM builds.
         */
        inject_ps2_mouse_motion?(dx: number, dy: number, wheel: number): void;
        /**
         * Set PS/2 mouse button state as a bitmask matching DOM `MouseEvent.buttons` (low 5 bits):
         * - bit0 (`0x01`): left
         * - bit1 (`0x02`): right
         * - bit2 (`0x04`): middle
         * - bit3 (`0x08`): back / side (only emitted if the guest enabled IntelliMouse Explorer, device ID `0x04`)
         * - bit4 (`0x10`): forward / extra (same note as bit3)
         *
         * This is an alias for {@link inject_mouse_buttons} (same mapping).
         *
         * Optional for older WASM builds.
         */
        inject_ps2_mouse_buttons?(buttons: number): void;

        /**
         * Drain pending IRQ pulses since the last call.
         *
         * Bits:
         * - bit0 (0x01): IRQ1 pulse (keyboard byte became available)
         * - bit1 (0x02): IRQ12 pulse (mouse byte became available)
         *
         * Newer builds expose this so edge-triggered i8042 IRQ behavior is represented
         * faithfully even when the output buffer refills immediately after a port 0x60 read.
         */
        drain_irqs?(): number;

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
        /**
         * Convenience: open `NET_TX`/`NET_RX` rings from an `ioIpcSab` and attach them as an L2 tunnel.
         *
         * Optional for older WASM builds.
         */
        attach_l2_tunnel_from_io_ipc_sab?(ioIpc: SharedArrayBuffer): void;
        /**
         * Legacy/alternate naming for attaching NET_TX/NET_RX rings.
         *
         * Optional for older WASM builds; prefer {@link attach_l2_tunnel_rings} when available.
         */
        attach_net_rings?(netTx: SharedRingBufferHandle, netRx: SharedRingBufferHandle): void;
        detach_network(): void;
        /**
         * Legacy/alternate naming for detaching the network backend.
         *
         * Optional for older WASM builds; prefer {@link detach_network} when available.
         */
        detach_net_rings?(): void;
        poll_network(): void;
        /**
         * Network backend statistics (if exposed by the WASM build).
         *
         * Optional for older WASM builds.
         */
        net_stats?():
            | {
                   tx_pushed_frames: bigint;
                   tx_pushed_bytes?: bigint;
                   tx_dropped_oversize: bigint;
                   tx_dropped_oversize_bytes?: bigint;
                   tx_dropped_full: bigint;
                   tx_dropped_full_bytes?: bigint;
                   rx_popped_frames: bigint;
                   rx_popped_bytes?: bigint;
                   rx_dropped_oversize: bigint;
                   rx_dropped_oversize_bytes?: bigint;
                   rx_corrupt: bigint;
                   rx_broken?: boolean;
               }
            | null;
        run_slice(max_insts: number): { kind: number; executed: number; detail: string; free(): void };
        free(): void;
    };

    UsbHidBridge: new () => {
        keyboard_event(usage: number, pressed: boolean): void;
        /**
         * Inject a Consumer Control (HID Usage Page 0x0C) usage transition.
         *
         * Optional for older WASM builds.
         */
        consumer_event?(usage: number, pressed: boolean): void;
        mouse_move(dx: number, dy: number): void;
        mouse_buttons(buttons: number): void;
        mouse_wheel(delta: number): void;
        /**
         * Inject both vertical and horizontal wheel deltas in a single report frame.
         *
         * Optional for older WASM builds.
         */
        mouse_wheel2?(wheel: number, hwheel: number): void;
        /**
         * Inject a horizontal wheel delta (AC Pan).
         *
         * Optional for older WASM builds.
         */
        mouse_hwheel?(delta: number): void;
        gamepad_report(packedLo: number, packedHi: number): void;
        drain_next_keyboard_report(): Uint8Array | null;
        /**
         * Drain the next Consumer Control report (2 bytes, little-endian usage ID) or return `null`.
         *
         * Optional for older WASM builds.
         */
        drain_next_consumer_report?(): Uint8Array | null;
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
        /**
         * Drain the next pending guest feature report read request (or return `null` if none).
         *
         * Optional while older WASM builds are still in circulation.
         */
        drain_next_feature_report_request?: () => { requestId: number; reportId: number } | null;
        /**
         * Complete a feature report read request with host-provided data.
         *
         * Optional while older WASM builds are still in circulation.
         */
        complete_feature_report_request?: (requestId: number, reportId: number, data: Uint8Array) => boolean;
        /**
         * Fail a feature report read request.
         *
         * Optional while older WASM builds are still in circulation.
         */
        fail_feature_report_request?: (requestId: number, reportId: number, error?: string) => boolean;
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
        /**
         * Drain the next pending guest feature report read request (or return `null` if none).
         *
         * Optional while older WASM builds are still in circulation.
         */
        drain_next_feature_report_request?: () => { requestId: number; reportId: number } | null;
        /**
         * Complete a feature report read request with host-provided data.
         *
         * Optional while older WASM builds are still in circulation.
         */
        complete_feature_report_request?: (requestId: number, reportId: number, data: Uint8Array) => boolean;
        /**
         * Fail a feature report read request.
         *
         * Optional while older WASM builds are still in circulation.
         */
        fail_feature_report_request?: (requestId: number, reportId: number, error?: string) => boolean;
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
         * topology (e.g. `guestPath` like `[0, 5]`).
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
        webhid_drain_output_reports(): Array<{ deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }> | null;
        /**
         * Drain pending `GET_REPORT (Feature)` requests issued by the guest.
         *
         * Optional for older WASM builds.
         */
        webhid_drain_feature_report_requests?(): Array<{ deviceId: number; requestId: number; reportId: number }> | null;
        /**
         * Complete (or fail) a pending `GET_REPORT (Feature)` request.
         *
         * - When `ok=true`, `data` is returned to the guest (empty payload when omitted).
         * - When `ok=false`, the request is failed and the guest sees a timeout-style error.
         *
         * Returns whether the completion was accepted by the guest-visible UHCI model.
         *
         * Optional for older WASM builds.
         */
        webhid_complete_feature_report_request?(
            deviceId: number,
            requestId: number,
            reportId: number,
            ok: boolean,
            data?: Uint8Array,
        ): boolean;
        /**
         * Legacy completion API used by older WASM builds (pre `webhid_complete_feature_report_request`).
         *
         * Optional.
         */
        webhid_push_feature_report_result?(
            deviceId: number,
            requestId: number,
            reportId: number,
            ok: boolean,
            data?: Uint8Array,
        ): void;

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
        /**
         * Drains queued WebUSB host actions.
         *
         * Returns `null` when there are no pending actions (to keep worker-side polling
         * allocation-free when idle).
         */
        webusb_drain_actions(): UsbHostAction[] | null;
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
     * WebUSB EHCI passthrough harness (dev-only): drives a tiny EHCI-style control-transfer state
     * machine and emits `UsbHostAction`s.
     *
     * Optional until all deployed WASM builds include it.
     */
    WebUsbEhciPassthroughHarness?: new () => {
        attach_controller(): void;
        detach_controller(): void;
        attach_device(): void;
        detach_device(): void;
        cmd_get_device_descriptor(): void;
        cmd_get_config_descriptor(): void;
        tick(): void;

        controller_attached(): boolean;
        device_attached(): boolean;
        usbsts(): number;
        irq_level(): boolean;
        last_error(): string | null;
        clear_usbsts(bits: number): void;

        drain_actions(): UsbHostAction[] | null;
        push_completion(completion: UsbHostCompletion): void;

        device_descriptor(): Uint8Array | null;
        config_descriptor(): Uint8Array | null;
        config_total_len(): number;

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
        /**
         * Update the device model's PCI command register (offset 0x04, low 16 bits).
         *
         * Optional for older WASM builds.
         */
        set_pci_command?(command: number): void;
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
    Machine: {
        /**
         * Construct a canonical machine using the browser defaults (see `aero_machine::MachineConfig::browser_defaults`).
         *
         * Today this means AeroGPU is enabled by default (and the standalone VGA/VBE device model is disabled).
         *
         * To force the legacy VGA/VBE device model instead, use {@link new_with_config} or {@link new_with_options}.
         */
        new (ramSizeBytes: number): MachineHandle;
        /**
         * Create a canonical machine with a custom SMBIOS System UUID seed.
         *
         * wasm-bindgen exports this using a camelCase name (`newWithSmbiosUuidSeed`).
         *
         * Optional for older WASM builds.
         */
        newWithSmbiosUuidSeed?: (ramSizeBytes: number, smbiosUuidSeed: bigint) => MachineHandle;
        /**
         * Create a machine with explicit graphics configuration.
         *
         * When `enableAerogpu=true`, VGA is disabled by default.
         *
         * Note: `enableAerogpu` and `enableVga` are mutually exclusive in the native machine
         * configuration; passing `enableAerogpu=true` and `enableVga=true` will fail construction.
         *
         * Optional for older WASM builds.
         */
        new_with_config?: (
            ramSizeBytes: number,
            enableAerogpu: boolean,
            enableVga?: boolean,
            cpuCount?: number,
        ) => MachineHandle;
        /**
         * Construct a canonical machine with an explicit vCPU count (SMP).
         *
         * Optional for older WASM builds.
         */
        new_with_cpu_count?: (ramSizeBytes: number, cpuCount: number) => MachineHandle;
        /**
         * Construct a canonical machine and optionally enable additional input backends.
         *
         * Optional for older WASM builds.
         */
        new_with_input_backends?: (
            ramSizeBytes: number,
            enableVirtioInput: boolean,
            enableSyntheticUsbHid: boolean,
        ) => MachineHandle;
        /**
         * Construct a machine with an options object that can override the default device set.
         *
         * This preserves the existing `new(ramSizeBytes)` behavior when `options` is omitted.
         *
         * Note: the mutually-exclusive VGA/AeroGPU device selection mirrors `new_with_config`:
         * - If callers explicitly set `enable_aerogpu` without specifying `enable_vga`, VGA defaults to
         *   `!enable_aerogpu`.
         * - If callers enable `enable_vga=true` without explicitly specifying `enable_aerogpu`,
         *   AeroGPU defaults to `false`.
         *
         * Optional for older WASM builds.
         */
        new_with_options?: (ramSizeBytes: number, options?: MachineOptions | null) => MachineHandle;
        /**
         * Construct a machine backed by shared guest RAM (see {@link guest_ram_layout}).
         *
         * Optional for older WASM builds.
         */
        new_shared?: (guestBase: number, guestSize: number) => MachineHandle;
        /**
         * Construct a shared-guest-memory machine with explicit graphics configuration.
         *
         * This is the shared-memory equivalent of {@link new_with_config} and allows browser
         * runtimes to force-disable AeroGPU (enable VGA) while still using shared guest RAM.
         *
         * Optional for older WASM builds.
         */
        new_shared_with_config?: (
            guestBase: number,
            guestSize: number,
            enableAerogpu: boolean,
            enableVga?: boolean,
            cpuCount?: number,
        ) => MachineHandle;
        /**
         * Construct a machine preset for the canonical Win7 storage topology backed by shared guest RAM.
         *
         * Optional for older WASM builds.
         */
        new_win7_storage_shared?: (guestBase: number, guestSize: number) => MachineHandle;
        /**
         * Back-compat shared-guest-memory factory naming variants.
         *
         * These were exported by intermediate wasm-bindgen surfaces before the canonical
         * `Machine.new_shared` / `Machine.new_win7_storage_shared` naming stabilized.
         *
         * Optional for older WASM builds.
         */
        new_win7_storage_shared_guest_memory?: (guestBase: number, guestSize: number) => MachineHandle;
        new_shared_guest_memory_win7_storage?: (guestBase: number, guestSize: number) => MachineHandle;
        new_shared_guest_memory?: (guestBase: number, guestSize: number) => MachineHandle;
        from_shared_guest_memory_win7_storage?: (guestBase: number, guestSize: number) => MachineHandle;
        from_shared_guest_memory?: (guestBase: number, guestSize: number) => MachineHandle;
        /**
         * Construct a machine preset for the canonical Win7 storage topology backed by internal guest RAM.
         *
         * Optional for older WASM builds.
         */
        new_win7_storage?: (ramSizeBytes: number) => MachineHandle;
        /**
         * Stable snapshot `disk_id` helpers for the canonical Win7 storage topology.
         *
         * Optional for older WASM builds.
         */
        disk_id_primary_hdd?(): number;
        disk_id_install_media?(): number;
        disk_id_ide_primary_master?(): number;
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
         * Return the linear-memory pointer to the `CpuState.a20_enabled` flag.
         *
         * Newer builds expose this so the JS host can update the A20 gate state without
         * re-entering WASM while the VM is executing (re-entrant `&mut self` would be UB).
         */
        a20_enabled_ptr?(): number;
        /**
         * Deterministic CPU/MMU snapshot helpers for the minimal Tier-0 VM loop.
         *
         * Used by the multi-worker VM snapshot orchestrator (CPU + IO workers).
         *
      * Optional while older WASM builds are still in circulation.
      */
        save_state_v2?: () => { cpu: Uint8Array; mmu: Uint8Array; cpu_internal?: Uint8Array };
        load_state_v2?: (cpu: Uint8Array, mmu: Uint8Array) => void;
        /**
         * Restore non-architectural CPU bookkeeping (`DeviceId::CPU_INTERNAL`, v2).
         *
         * Optional while older WASM builds are still in circulation.
         */
        load_cpu_internal_state_v2?: (bytes: Uint8Array) => void;
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
         * Return the linear-memory pointer to the `CpuState.a20_enabled` flag.
         *
         * Newer builds expose this so the JS host can update the A20 gate state without
         * re-entering WASM while the VM is executing (re-entrant `&mut self` would be UB).
         */
        a20_enabled_ptr?(): number;

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
         * Snapshot page-version metadata for a code range.
         */
        snapshot_meta(codePaddr: bigint, byteLen: number): unknown;
        /**
         * Install a compiled handle into the runtime using a pre-snapshotted meta object (from
         * {@link snapshot_meta}).
         *
         * Returns an array of evicted entry RIPs so the JS runtime can free/reuse table slots.
         */
        install_handle(entryRip: bigint, tableIndex: number, meta: unknown): bigint[];
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
        is_compiled(entryRip: bigint): boolean;
        cache_len(): number;
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
    /**
     * Worker-side VM snapshot helpers (CPU/MMU/device state + full RAM streaming to OPFS) exposed
     * as free functions.
     *
     * These are optional for older WASM builds; newer builds prefer {@link WorkerVmSnapshot}.
     */
    vm_snapshot_save_to_opfs?: (path: string, cpu: Uint8Array, mmu: Uint8Array, devices: unknown) => unknown;
    vm_snapshot_restore_from_opfs?: (path: string) => unknown;
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
    VirtioSndPlaybackDemo?: new (
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
        /**
         * Configure the demo's sine-wave generator.
         *
         * Optional for older WASM builds.
         */
        set_sine_wave?(freqHz: number, gain: number): void;
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
    HdaControllerBridge?: new (guestBase: number, guestSize: number, outputSampleRateHz?: number) => {
        /**
         * Host/output sample rate used by the controller when emitting audio.
         *
         * Added in newer WASM builds; older builds may not expose this.
         */
        readonly output_sample_rate_hz?: number;
        mmio_read(offset: number, size: number): number;
        mmio_write(offset: number, size: number, value: number): void;
        /**
         * Update the device model's PCI command register (offset 0x04, low 16 bits).
         *
         * Optional for older WASM builds.
         */
        set_pci_command?(command: number): void;

        attach_audio_ring(ringSab: SharedArrayBuffer, capacityFrames: number, channelCount: number): void;
        detach_audio_ring(): void;
        /**
         * Alias for {@link attach_audio_ring} retained for older call sites/spec drafts.
         */
        attach_output_ring?: (ringSab: SharedArrayBuffer, capacityFrames: number, channelCount: number) => void;
        /**
         * Alias for {@link detach_audio_ring} retained for older call sites/spec drafts.
         */
        detach_output_ring?: () => void;

        attach_mic_ring(ringSab: SharedArrayBuffer, sampleRate: number): void;
        detach_mic_ring(): void;

        set_output_rate_hz(rate: number): void;
        /**
         * Alias for {@link set_output_rate_hz} retained for older call sites/spec drafts.
         */
        set_output_sample_rate_hz?: (rate: number) => void;
        process(frames: number): void;

        /**
         * Alias for {@link process} retained for older call sites.
         */
        step_frames(frames: number): void;
        /**
         * Older stepping API retained by some WASM builds.
         */
        tick?(frames: number): void;

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
         * Deterministic snapshot/restore helpers (aero-io-snapshot TLV blob, inner `HDA0`).
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
    /**
     * The instantiated module's linear memory (when it can be extracted).
     *
     * This is primarily useful for reading raw pixel buffers produced by WASM-side scanout
     * helpers (e.g. VGA/VBE present-to-RGBA) without needing to depend on wasm-bindgen's
     * internal `wasm.memory` export plumbing.
     *
     * Optional for older/custom init shims and test importer overrides.
     */
    wasmMemory?: WebAssembly.Memory;
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
    const crossOriginIsolated = (globalThis as unknown as { crossOriginIsolated?: unknown }).crossOriginIsolated;

    // `crossOriginIsolated` is required for SharedArrayBuffer on the web. In non-web
    // contexts (e.g. Node/Vitest), this flag does not exist, but SharedArrayBuffer +
    // shared WebAssembly.Memory may still be available.
    if (hasCrossOriginIsolated && crossOriginIsolated !== true) {
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
        mem_store_u32: mod.mem_store_u32 ?? mod.memStoreU32,
        guest_ram_layout: mod.guest_ram_layout ?? mod.guestRamLayout,
        storage_capabilities: mod.storage_capabilities ?? mod.storageCapabilities,
        create_win7_machine_shared_guest_memory:
          mod.create_win7_machine_shared_guest_memory ?? mod.createWin7MachineSharedGuestMemory,
        create_machine_win7_shared_guest_memory:
          mod.create_machine_win7_shared_guest_memory ?? mod.createMachineWin7SharedGuestMemory,
        create_machine_shared_guest_memory_win7:
          mod.create_machine_shared_guest_memory_win7 ?? mod.createMachineSharedGuestMemoryWin7,
        tiered_vm_jit_abi_layout: mod.tiered_vm_jit_abi_layout ?? mod.tieredVmJitAbiLayout,
        mem_load_u32: mod.mem_load_u32 ?? mod.memLoadU32,
        jit_abi_constants: mod.jit_abi_constants ?? mod.jitAbiConstants,
        MouseButton: mod.MouseButton,
        MouseButtons: mod.MouseButtons,
        MachineBootDevice: mod.MachineBootDevice,
        VirtioInputPciDevice: mod.VirtioInputPciDevice,
        SharedRingBuffer: mod.SharedRingBuffer,
        open_ring_by_kind: mod.open_ring_by_kind ?? mod.openRingByKind,
        demo_render_rgba8888: mod.demo_render_rgba8888,
        UsbHidBridge: mod.UsbHidBridge,
        WebHidPassthroughBridge: mod.WebHidPassthroughBridge,
        UsbHidPassthroughBridge: mod.UsbHidPassthroughBridge,
        UsbPassthroughBridge: mod.UsbPassthroughBridge,
        UhciRuntime: mod.UhciRuntime,
        VirtioNetPciBridge: mod.VirtioNetPciBridge,
        VirtioSndPciBridge: mod.VirtioSndPciBridge,
        WebUsbUhciPassthroughHarness: mod.WebUsbUhciPassthroughHarness,
        WebUsbEhciPassthroughHarness: mod.WebUsbEhciPassthroughHarness,
        UhciControllerBridge: mod.UhciControllerBridge,
        EhciControllerBridge: mod.EhciControllerBridge,
        XhciControllerBridge: mod.XhciControllerBridge,
        E1000Bridge: mod.E1000Bridge,
        PcMachine: mod.PcMachine,
        WebUsbUhciBridge: mod.WebUsbUhciBridge,
        I8042Bridge: mod.I8042Bridge,
        synthesize_webhid_report_descriptor:
          mod.synthesize_webhid_report_descriptor ?? mod.synthesizeWebhidReportDescriptor ?? mod.synthesizeWebHidReportDescriptor,
        GuestCpuBenchHarness: mod.GuestCpuBenchHarness,
        UsbPassthroughDemo: mod.UsbPassthroughDemo,
        CpuWorkerDemo: mod.CpuWorkerDemo,
        AeroApi: mod.AeroApi,
        DemoVm: mod.DemoVm,
        Machine: mod.Machine,
        WasmVm: mod.WasmVm,
        WasmTieredVm: mod.WasmTieredVm,
        vm_snapshot_save_to_opfs:
          mod.vm_snapshot_save_to_opfs ??
          mod.vmSnapshotSaveToOpfs ??
          mod.save_vm_snapshot_to_opfs ??
          mod.saveVmSnapshotToOpfs ??
          mod.snapshot_vm_to_opfs ??
          mod.snapshotVmToOpfs ??
          mod.snapshot_worker_vm_to_opfs ??
          mod.snapshotWorkerVmToOpfs ??
          mod.worker_vm_snapshot_to_opfs ??
          mod.workerVmSnapshotToOpfs,
        vm_snapshot_restore_from_opfs:
          mod.vm_snapshot_restore_from_opfs ??
          mod.vmSnapshotRestoreFromOpfs ??
          mod.restore_vm_snapshot_from_opfs ??
          mod.restoreVmSnapshotFromOpfs ??
          mod.restore_snapshot_vm_from_opfs ??
          mod.restoreSnapshotVmFromOpfs ??
          mod.restore_worker_vm_snapshot_from_opfs ??
          mod.restoreWorkerVmSnapshotFromOpfs ??
          mod.snapshot_restore_vm_from_opfs ??
          mod.snapshotRestoreVmFromOpfs,
        WorkerVmSnapshot: mod.WorkerVmSnapshot,
        WorkletBridge: mod.WorkletBridge,
        create_worklet_bridge: mod.create_worklet_bridge ?? mod.createWorkletBridge,
        attach_worklet_bridge: mod.attach_worklet_bridge ?? mod.attachWorkletBridge,
        MicBridge: mod.MicBridge,
        attach_mic_bridge: mod.attach_mic_bridge ?? mod.attachMicBridge,
        SineTone: mod.SineTone,
        HdaControllerBridge: mod.HdaControllerBridge,
        HdaPcmWriter: mod.HdaPcmWriter,
        HdaPlaybackDemo: mod.HdaPlaybackDemo,
        VirtioSndPlaybackDemo: mod.VirtioSndPlaybackDemo,
    };
}

// `wasm-pack` outputs into `web/src/wasm/pkg-single` and `web/src/wasm/pkg-threaded`.
//
// These directories are generated (see `web/scripts/build_wasm.mjs`) and are not
// necessarily present in a fresh checkout. Use `import.meta.glob` so:
//  - Vite builds don't fail when the generated output is missing.
//  - When the output *is* present, it is bundled as usual.
//
// Note: some Node-based unit tests execute the TypeScript sources directly under
// Node (via `--experimental-strip-types`) without Vite transforms. In that
// environment `import.meta.glob` is undefined, so fall back to an empty importer
// table.
let wasmImporters: Record<string, () => Promise<unknown>> = {};
try {
    // This call is transformed by Vite during dev/build.
    wasmImporters = import.meta.glob("../wasm/pkg-*/aero_wasm.js") as Record<string, () => Promise<unknown>>;
} catch {
    wasmImporters = {};
}

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

type ResolvedWasmImporter = {
    importer: () => Promise<RawWasmModule>;
    wasmUrl: URL;
    jsUrl: URL;
    source: "override" | "release" | "dev";
};

let wasmJsImportNonce = 0;
const wasmJsModulesByMemory: Record<WasmVariant, WeakMap<WebAssembly.Memory, RawWasmModule>> = {
    single: new WeakMap(),
    threaded: new WeakMap(),
};

async function importWasmJsModule(
    variant: WasmVariant,
    resolved: ResolvedWasmImporter,
    options: { memory?: WebAssembly.Memory },
): Promise<RawWasmModule> {
    const memory = options.memory;

    // Default path: use the Vite-provided importer (bundler-safe, cached).
    if (!memory || resolved.source === "override") {
        return (await resolved.importer()) as RawWasmModule;
    }

    // When a caller injects a WebAssembly.Memory, they usually expect a *fresh* wasm instance that
    // uses that specific memory. However, wasm-bindgen's generated JS glue caches a singleton wasm
    // instance in a module-scoped `let wasm;` and silently ignores subsequent init calls with a
    // different memory.
    //
    // This is especially problematic in Node/Vitest where unit tests create distinct
    // WebAssembly.Memory instances per test. To keep tests deterministic, bypass the ESM module
    // cache by importing the JS glue via a cache-busted `file://...aero_wasm.js?nonce=...` URL.
    //
    // IMPORTANT: Only do this for `file:` URLs. In browser builds Vite bundles the wasm-pack output
    // and `import()` with a runtime-generated URL would not be included in the bundle.
    if (resolved.jsUrl.protocol !== "file:") {
        return (await resolved.importer()) as RawWasmModule;
    }

    const cached = wasmJsModulesByMemory[variant].get(memory);
    if (cached) return cached;

    const bustUrl = new URL(resolved.jsUrl.href);
    bustUrl.searchParams.set("aero_wasm_nonce", String((wasmJsImportNonce += 1)));

    // Keep the specifier opaque to Vite so browser builds don't try to bundle it.
    const mod = (await import(/* @vite-ignore */ bustUrl.href)) as RawWasmModule;
    wasmJsModulesByMemory[variant].set(memory, mod);
    return mod;
}

function resolveWasmImporter(
    variant: WasmVariant,
): ResolvedWasmImporter | undefined {
    const override = globalThis.__aeroWasmJsImporterOverride?.[variant];
    if (override) {
        // The override is primarily for tests; avoid hard-coding a file:// URL which would
        // trigger Node-specific file reads in `resolveWasmInputForInit`.
        return {
            importer: override,
            wasmUrl: new URL("about:blank"),
            jsUrl: new URL("about:blank"),
            source: "override",
        };
    }

    const releaseJsPath = variant === "single" ? "../wasm/pkg-single/aero_wasm.js" : "../wasm/pkg-threaded/aero_wasm.js";
    const devJsPath = variant === "single" ? "../wasm/pkg-single-dev/aero_wasm.js" : "../wasm/pkg-threaded-dev/aero_wasm.js";
    const importer = wasmImporters[releaseJsPath] ?? wasmImporters[devJsPath];
    if (!importer) return undefined;

    const isRelease = importer === wasmImporters[releaseJsPath];
    const jsPath = isRelease ? releaseJsPath : devJsPath;
    const wasmPath =
        isRelease
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
    const jsUrl = new URL(/* @vite-ignore */ jsPath, import.meta.url);

    return {
        importer: importer as () => Promise<RawWasmModule>,
        wasmUrl,
        jsUrl,
        source: isRelease ? "release" : "dev",
    };
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
    const originalInstantiateUntyped = originalInstantiate as unknown as (module: unknown, imports?: unknown) => unknown;
    const instantiatePatched = (module: unknown, imports?: unknown) => {
        const patchedImports = imports ?? {};
        patchWasmImportsWithMemory(patchedImports, memory);
        return originalInstantiateUntyped(module, patchedImports);
    };

    const wasm = WebAssembly as unknown as { instantiate: unknown; instantiateStreaming?: unknown };
    wasm.instantiate = instantiatePatched;
    if (hasInstantiateStreaming) {
        const originalInstantiateStreamingUntyped = originalInstantiateStreaming as unknown as (
            source: unknown,
            imports?: unknown,
        ) => unknown;
        const instantiateStreamingPatched = (source: unknown, imports?: unknown) => {
            const patchedImports = imports ?? {};
            patchWasmImportsWithMemory(patchedImports, memory);
            return originalInstantiateStreamingUntyped(source, patchedImports);
        };
        wasm.instantiateStreaming = instantiateStreamingPatched;
    }
    try {
        return await fn();
    } finally {
        wasm.instantiate = originalInstantiate;
        if (hasInstantiateStreaming) {
            wasm.instantiateStreaming = originalInstantiateStreaming;
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
            // Modern wasm-bindgen builds prefer a single options object, e.g.
            // `default({ module_or_path, memory })`. Passing positional args triggers a warning.
            //
            // Keep compatibility with older build outputs by trying the object form first, then
            // falling back to legacy positional overloads.
            try {
                await initFn({ module_or_path: input, module: input, memory });
                return;
            } catch {
                // ignore (fall back to legacy call shapes)
            }

            try {
                // Legacy call shape (wasm-bindgen import-memory builds often used
                // `default(input?, memory?)`).
                await initFn(input, memory);
                return;
            } catch (err) {
                // Some wasm-bindgen outputs ignore/validate the extra argument.
                // Retrying with `default(input)` still uses the patched import object
                // (so the provided memory is wired up) but avoids "wrong number of
                // arguments" style failures.
                try {
                    await initFn(input);
                    return;
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
    const mod = await importWasmJsModule("single", resolved, { memory: options.memory });
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
    const mod = await importWasmJsModule("threaded", resolved, { memory: options.memory });
    await initWasmBindgenModule(mod, resolved.wasmUrl, {
        variant: "threaded",
        module: options.module,
        memory: options.memory,
    });
    return { api: toApi(mod), memory: options.memory ?? tryExtractWasmMemory(mod) };
}

export async function initWasm(options: WasmInitOptions = {}): Promise<WasmInitResult> {
    let requested: WasmVariant | "auto" = options.variant ?? "auto";
    const threadSupport = detectThreadSupport();
    const moduleVariantHint = options.module ? lookupPrecompiledWasmModuleVariant(options.module) : undefined;
    const memoryIsShared =
        typeof SharedArrayBuffer !== "undefined" &&
        options.memory != null &&
        ((options.memory.buffer as unknown as ArrayBufferLike) instanceof SharedArrayBuffer);

    // Shared guest RAM (`WebAssembly.Memory({ shared: true })`) requires a shared-memory / threads-enabled wasm module.
    // The single-threaded wasm-pack output cannot import a shared memory, so falling back from
    // threaded->single is impossible in this configuration.
    if (memoryIsShared) {
        if (requested === "single") {
            throw new Error(
                [
                    "Single-threaded WASM build requested but a shared WebAssembly.Memory was provided.",
                    "",
                    "The single-threaded WASM module cannot import shared linear memory. Use the threaded build instead.",
                    "",
                    "Build it with:",
                    "  cd web",
                    "  npm run wasm:build:threaded",
                ].join("\n"),
            );
        }
        if (requested === "auto") {
            requested = "threaded";
        }
    }

    if (requested === "auto" && moduleVariantHint) {
        if (moduleVariantHint === "single") {
            const loaded = await loadSingle(options);
            return {
                api: loaded.api,
                variant: "single",
                reason: "Using single-threaded WASM because the provided WebAssembly.Module was precompiled for it.",
                memory: describeMemory(loaded.memory),
                wasmMemory: loaded.memory,
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
                    wasmMemory: loaded.memory,
                };
            } catch (err) {
                const message = err instanceof Error ? err.message : String(err);
                const loaded = await loadSingle({ ...options, module: undefined });
                return {
                    api: loaded.api,
                    variant: "single",
                    reason: `Threaded WASM init failed (precompiled module provided); falling back to single. Error: ${message}`,
                    memory: describeMemory(loaded.memory),
                    wasmMemory: loaded.memory,
                };
            }
        }

        const loaded = await loadSingle({ ...options, module: undefined });
        return {
            api: loaded.api,
            variant: "single",
            reason: `Threaded precompiled module provided but the current runtime cannot use shared-memory WebAssembly; falling back to single. Reason: ${threadSupport.reason}`,
            memory: describeMemory(loaded.memory),
            wasmMemory: loaded.memory,
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
            wasmMemory: loaded.memory,
        };
    }

    if (requested === "single") {
        const loaded = await loadSingle(options);
        return {
            api: loaded.api,
            variant: "single",
            reason: "Forced via initWasm({ variant: 'single' })",
            memory: describeMemory(loaded.memory),
            wasmMemory: loaded.memory,
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
                wasmMemory: loaded.memory,
            };
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            const loaded = await loadSingle(options);
            return {
                api: loaded.api,
                variant: "single",
                reason: `Threaded WASM init failed; falling back to single. Error: ${message}`,
                memory: describeMemory(loaded.memory),
                wasmMemory: loaded.memory,
            };
        }
    }

    const loaded = await loadSingle(options);
    return {
        api: loaded.api,
        variant: "single",
        reason: threadSupport.reason,
        memory: describeMemory(loaded.memory),
        wasmMemory: loaded.memory,
    };
}
