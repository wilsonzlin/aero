# 12 - Testing Strategy & Validation

## Overview

Comprehensive testing is critical for an emulator. We must verify correctness at the instruction level, system level, and application level.

## Practical guide (running tests locally)

This document describes *what* we test and *why*. For the practical, developer-facing guide to running the full test stack locally (Rust, WASM, TypeScript, Playwright), plus common issues like COOP/COEP and WebGPU gating, see:

- [`TESTING.md`](./TESTING.md)
- For deterministic manual smoke-test procedures (when automation isn’t sufficient), see:
  - [`docs/testing/`](./testing/)
- For “single document” end-to-end subsystem plans (device model ↔ guest drivers ↔ web runtime), see:
  - [`docs/test-plans/`](./test-plans/) (for example: virtio-input)

---

## Testing Pyramid

```
┌─────────────────────────────────────────────────────────────────┐
│                    Testing Pyramid                               │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│                         ┌───────┐                                │
│                        /         \                               │
│                       / End-to-End\                              │
│                      /   (E2E)     \                             │
│                     /   ~50 tests   \                            │
│                    ─────────────────────                         │
│                   /                     \                        │
│                  /    Integration        \                       │
│                 /     ~500 tests          \                      │
│                ───────────────────────────────                   │
│               /                             \                    │
│              /         Unit Tests            \                   │
│             /         ~10,000 tests           \                  │
│            ─────────────────────────────────────                 │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Unit Tests

### CPU Instruction Tests

The canonical interpreter is Tier-0 (`aero_cpu_core::interp::tier0`). Instruction-level unit tests
live under `crates/aero-cpu-core/tests/` and generally follow the same shape:

- allocate a small `FlatTestBus` (linear-address bus)
- load a short instruction sequence into guest memory
- drive execution with `exec::Vcpu` + `exec::Tier0Interpreter`
- assert on architectural state (`CpuState` registers/flags) and any exits (`Exception`, assists)

```rust
use aero_cpu_core::exec::{Interpreter as _, Tier0Interpreter, Vcpu};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, RFLAGS_CF, RFLAGS_ZF};
use aero_x86::Register;

#[test]
fn mov_and_add_update_registers_and_flags() {
    let mut bus = FlatTestBus::new(0x2000);
    let code_base = 0x100u64;

    // MOV RAX, RBX; ADD RAX, 1; HLT
    bus.load(
        code_base,
        &[
            0x48, 0x89, 0xD8, // mov rax, rbx
            0x48, 0x83, 0xC0, 0x01, // add rax, 1
            0xF4, // hlt
        ],
    );

    let mut vcpu = Vcpu::new_with_mode(CpuMode::Long, bus);
    vcpu.cpu.state.set_rip(code_base);
    vcpu.cpu.state.write_reg(Register::RBX, 0xFFFF_FFFF_FFFF_FFFF);

    let mut interp = Tier0Interpreter::new(1024);
    while !vcpu.cpu.state.halted {
        interp.exec_block(&mut vcpu);
    }

    assert_eq!(vcpu.cpu.state.read_reg(Register::RAX), 0);
    assert!(vcpu.cpu.state.get_flag(RFLAGS_CF));
    assert!(vcpu.cpu.state.get_flag(RFLAGS_ZF));
}
```

For patterns around faults/exceptions (turning an `Exception` into a pending event and delivering it
through `CpuCore`), see [`docs/02-cpu-emulation.md`](./02-cpu-emulation.md).

### Memory Subsystem Tests

Paging and TLB behavior is implemented in `crates/aero-mmu` and integrated into the CPU core via
`aero_cpu_core::PagingBus` (a `CpuBus` wrapper that performs translation and routes accesses to a
physical `aero_mmu::MemoryBus`).

Concrete, end-to-end paging tests (Tier-0 + paging + INVLPG/CR3/CR4/EFER interaction + fault
delivery) live under:

- [`crates/aero-cpu-core/tests/paging.rs`](../crates/aero-cpu-core/tests/paging.rs)

And the core page table walker / TLB unit tests live under:

- [`crates/aero-mmu/src/lib.rs`](../crates/aero-mmu/src/lib.rs) (implementation + internal tests)

### JIT vs Interpreter Memory Differential Tests

Memory is where subtle correctness bugs hide (TLB invalidation, permission checks, MMIO routing, cross-page accesses). For the baseline JIT memory fast path we need **differential tests** that run the same guest program in:

1. interpreter (reference)
2. baseline JIT (candidate)

…and compare architectural state after execution.

```rust
#[test]
fn jit_memory_loop_matches_interpreter() {
    // Program: tight RAM loop stressing loads/stores.
    // - sequential accesses (TLB miss once per page)
    // - random accesses (TLB pressure)
    // - mixed sizes (1/2/4/8/16)
    let program = assemble("
        mov rsi, 0x100000      ; base
        mov rcx, 1000000
    loop:
        mov rax, [rsi]
        add rax, 1
        mov [rsi], rax
        add rsi, 8
        dec rcx
        jnz loop
        hlt
    ");

    let interp = run_interpreter(&program);
    let jit = run_baseline_jit(&program);

    assert_eq!(jit.regs, interp.regs);
    assert_eq!(jit.flags, interp.flags);
    assert_eq!(jit.memory_digest(), interp.memory_digest());

    // Performance sanity check: the JIT should not be calling translation helpers per access.
    assert!(jit.stats.mmu_translate_calls < 10_000);
}
```

### Property tests for randomized memory ops

```rust
proptest! {
    #[test]
    fn jit_random_memory_ops_match_interpreter(ops in arbitrary_mem_ops()) {
        let interp = run_ops_interpreter(&ops);
        let jit = run_ops_baseline_jit(&ops);

        prop_assert_eq!(jit.regs, interp.regs);
        prop_assert_eq!(jit.flags, interp.flags);
        prop_assert_eq!(jit.memory_digest(), interp.memory_digest());
    }
}
```

### MMIO exit validation

```rust
#[test]
fn jit_mmio_access_causes_exit() {
    // Map an MMIO region and execute a program that touches it.
    let mmio_base = 0xFEE0_0000; // Local APIC (example)
    let program = assemble("
        mov eax, [0xFEE00030]   ; read APIC register (e.g. Local APIC version)
        hlt
    ");

    let jit = run_baseline_jit(&program);

    // The JIT must not directly load from RAM for MMIO ranges.
    assert_eq!(jit.exit_reason, ExitReason::Mmio);
    assert!(jit.stats.jit_exit_mmio_calls >= 1);
}
```

### Device Tests

```rust
#[cfg(test)]
mod device_tests {
    #[test]
    fn test_pic_irq_priority() {
        let mut pic = Pic::new();
        
        // Raise IRQ 1 and IRQ 3
        pic.raise_irq(1);
        pic.raise_irq(3);
        
        // IRQ 1 should be higher priority
        assert_eq!(pic.get_pending_irq(), Some(1));
        
        pic.acknowledge(1);
        assert_eq!(pic.get_pending_irq(), Some(3));
    }
    
    #[test]
    fn test_pit_countdown() {
        let mut pit = Pit::new();
        
        // Set channel 0 to mode 2, count 1000
        pit.write_command(0x34);  // Channel 0, mode 2, lo/hi
        pit.write_data(0, 1000 & 0xFF);
        pit.write_data(0, (1000 >> 8) & 0xFF);
        
        // Tick 500 times
        for _ in 0..500 {
            pit.tick();
        }
        
        assert_eq!(pit.read_count(0), 500);
    }
}
```

### USB (UHCI + WebHID/WebUSB passthrough) tests

The canonical browser USB/UHCI stack is `crates/aero-usb` (see [ADR 0015](./adr/0015-canonical-usb-stack.md)).
Keep correctness locked down with:

- Rust unit/integration tests under `crates/aero-usb` (including UHCI schedule + passthrough mapping tests)
- TypeScript unit/integration tests under `web/src/usb/*test.ts` (including coverage for the
  SharedArrayBuffer ring fast path negotiated by `usb.ringAttach`/`usb.ringDetach` in
  `web/src/usb/usb_proxy_ring_integration.test.ts`)
- Web smoke panels (manual) described in [`docs/webusb-passthrough.md`](./webusb-passthrough.md)

### GPU Persistent Cache Tests
  
The persistent GPU cache should have unit tests that lock down **keying and versioning**, since subtle mistakes can lead to hard-to-debug correctness or performance issues.
 
```rust
#[cfg(test)]
mod gpu_cache_tests {
    use super::*;

    #[test]
    fn cache_key_changes_with_schema_version() {
        let shader_bytes = b"dxbc...";
        let k1 = CacheKey::new(1, BackendKind::DxbcToWgsl, shader_bytes, None);
        let k2 = CacheKey::new(2, BackendKind::DxbcToWgsl, shader_bytes, None);
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_changes_with_backend_kind() {
        let shader_bytes = b"dxbc...";
        let k1 = CacheKey::new(1, BackendKind::DxbcToWgsl, shader_bytes, None);
        let k2 = CacheKey::new(1, BackendKind::HlslToWgsl, shader_bytes, None);
        assert_ne!(k1, k2);
    }
}
```

### ACPI Power Management Tests

Power-management correctness is mostly about **register semantics** and **host
orchestration**, not just table generation. Encode the expected behavior with
unit and integration tests:

```rust
#[test]
fn test_pm1_status_write_one_to_clear() {
    let cfg = AcpiPmConfig::default();
    let pm = Rc::new(RefCell::new(AcpiPmIo::new(cfg)));
    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm.clone());

    pm.borrow_mut().trigger_power_button();
    assert_ne!(pm.borrow().pm1_status() & PM1_STS_PWRBTN, 0);

    // Writing a 1 clears the bit.
    bus.write(cfg.pm1a_evt_blk, 2, PM1_STS_PWRBTN as u32);
    assert_eq!(pm.borrow().pm1_status() & PM1_STS_PWRBTN, 0);
}

#[test]
fn test_s5_shutdown_requests_poweroff() {
    let cfg = AcpiPmConfig::default();
    let powered_off = Rc::new(Cell::new(false));
    let powered_off_cb = powered_off.clone();

    let callbacks = AcpiPmCallbacks {
        request_power_off: Some(Box::new(move || powered_off_cb.set(true))),
        ..Default::default()
    };
    let pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks(cfg, callbacks)));
    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm);

    // SLP_TYP(S5) + SLP_EN
    bus.write(cfg.pm1a_cnt_blk, 2, ((SLP_TYP_S5 as u32) << 10) | (1 << 13));
    assert!(powered_off.get());
}
```
 
---
 
### Snapshot Round-Trip Tests

Every device that supports save/restore should have a unit test that validates:

1. `save_state()` produces deterministic bytes for a given state
2. `load_state()` restores an equivalent observable state

```rust
#[test]
fn test_i8042_snapshot_roundtrip() {
    let mut dev = I8042Controller::new();
    dev.inject_scancode(0x1C);

    let snap = dev.save_state();

    let mut restored = I8042Controller::new();
    restored.load_state(&snap).unwrap();

    assert_eq!(dev.read_data_port(), restored.read_data_port());
}
```
### SMP / APIC IPI Tests

Multi-core enablement needs focused tests because bugs often manifest as hangs or heisenbugs:

- **IPI delivery unit tests**
  - Decode APIC ICR writes.
  - Verify destination selection (physical destination + shorthand modes).
  - Verify delivery modes:
    - **INIT** transitions AP into *wait-for-SIPI*.
    - **SIPI** starts AP at `vector << 12`.
    - **Fixed** delivers an interrupt vector to the target vCPU.
- **AP bring-up integration test**
  - Synthetic "guest" that:
    1. Reads ACPI MADT and asserts multiple processors are present.
    2. BSP sends INIT+SIPI to start an AP.
    3. BSP sends a fixed IPI; AP observes/handles it.

## Integration Tests

### Boot Tests

```rust
#[test]
fn test_boot_sector_fixture_writes_vga_text() {
    let mut vm = VirtualMachine::new(Config {
        memory: 16 * MB,
        boot_device: BootDevice::HardDisk,
    });

    // CI-safe: generated from source via `cargo xtask fixtures`.
    vm.load_disk("tests/fixtures/boot/boot_vga_serial_8s.img");

    vm.run_for(Duration::from_millis(50));

    // VGA text mode memory starts at 0xB8000.
    let vga = vm.read_physical(0xB8000, 10);
    assert_eq!(vga, tests::fixtures::boot::boot_vga_serial::EXPECTED_VGA_TEXT_BYTES);
}

#[tokio::test]
async fn test_windows_7_boot() {
    let mut vm = VirtualMachine::new(Config {
        memory: 2 * GB,
        boot_device: BootDevice::HardDisk,
    });
    
    // Aero does not ship Windows images. This test is opt-in and requires a
    // locally-provided disk image path.
    let win7_image = std::env::var("AERO_WIN7_IMAGE")
        .expect("set AERO_WIN7_IMAGE to a locally-provided Windows 7 disk image");
    vm.load_disk(&win7_image).await;
    
    // Boot to login screen
    let screenshot = vm.run_until_stable_frame(Duration::from_secs(120)).await;
    
    // Visual regression test
    assert!(image_matches(screenshot, "expected/win7_login.png", 0.95));
}
```

### Snapshot/Restore Integration Test

Scripted scenario:

1. perform a disk write (ensure it survives flush)
2. inject keyboard input (pending i8042 bytes)
3. create a network connection (TCP proxy)
4. snapshot, reset VM, restore snapshot
5. verify disk data + input bytes are preserved, and network follows the configured restore policy (drop/reconnect)

```rust
#[test]
fn test_io_snapshot_restore() {
    let mut vm = TestVm::new();

    vm.disk_write(0, b"hello");
    vm.key_press("A");
    vm.tcp_connect("example.com:80");

    let snap = vm.snapshot();
    vm.reset();
    vm.restore(&snap);

    assert_eq!(vm.disk_read(0, 5), b"hello");
    assert!(vm.has_pending_keyboard_bytes());
    assert!(vm.network_is_reconnected_or_dropped_per_policy());
}
```

### Graphics Tests
  
Graphics correctness needs **two layers**:

1. **Non-GPU tests (fast, deterministic):** shader/pipeline validation, pipeline key hashing, render-state caching, command encoding, etc. These can run as normal Rust unit tests or `wasm-bindgen-test` in a non-GPU JS environment.
2. **Real-GPU smoke tests (browser E2E):** ensure WebGPU and the WebGL2 fallback can initialize, render, and read back pixels. These should run in real browsers via Playwright.

For `wasm-bindgen-test` suites, prefer running via `wasm-pack` (e.g. `wasm-pack test --node`) so tests execute in a JS environment without requiring GPU access.

```rust
#[wasm_bindgen_test]
async fn test_vga_text_mode() {
    let mut vga = VgaEmulator::new();
    
    // Set text mode
    vga.set_mode(0x03);
    
    // Write character at position (0, 0)
    vga.write_char(0, 0, 'A', 0x07);
    
    // Render to framebuffer
    let framebuffer = vga.render();
    
    // Check that 'A' is rendered correctly
    let expected = load_reference_image("vga_text_A.png");
    assert_image_matches(&framebuffer, &expected);
}

#[wasm_bindgen_test]
async fn test_vbe_lfb_mode_1024x768x32() {
    let mut gpu = AeroGpuEmulator::new();

    // Set VBE mode (0x118 suggested) with linear framebuffer bit set.
    gpu.bios_int10_vbe_set_mode(0x118 | (1 << 14));

    // Draw a simple pattern into the reported LFB physical address.
    let lfb = gpu.current_scanout().base_paddr();
    gpu.mem_write_u32(lfb, 0x00FF_0000); // top-left pixel: red (B8G8R8X8)

    let frame = gpu.present().await;
    assert_eq!(frame.resolution(), (1024, 768));
    assert_eq!(frame.pixel(0, 0), Rgba::new(255, 0, 0, 255));
}

#[wasm_bindgen_test]
async fn test_scanout_handoff_vbe_to_wddm() {
    let mut gpu = AeroGpuEmulator::new();

    // Boot uses VBE LFB.
    gpu.bios_int10_vbe_set_mode(0x118 | (1 << 14));
    let legacy_lfb = gpu.current_scanout().base_paddr();
    gpu.mem_write_u32(legacy_lfb, 0x0000_FF00); // green
    let legacy_frame = gpu.present().await;
    assert_eq!(legacy_frame.pixel(0, 0), Rgba::new(0, 255, 0, 255));

    // WDDM driver claims scanout via AeroGPU MMIO registers.
    let wddm_fb = gpu.allocate_wddm_framebuffer(1024, 768, PixelFormat::B8G8R8X8);
    gpu.mem_write_u32(wddm_fb.base_paddr(), 0x0000_0000); // black
    gpu.mmio_set_scanout(wddm_fb.base_paddr(), 1024, 768, 1024 * 4, PixelFormat::B8G8R8X8);

    // Now the visible frame must come from the WDDM-programmed framebuffer.
    let wddm_frame = gpu.present().await;
    assert_eq!(wddm_frame.pixel(0, 0), Rgba::new(0, 0, 0, 255));
}

#[wasm_bindgen_test]
async fn test_directx_triangle() {
    let mut gpu = GpuEmulator::new().await;
    
    // Submit D3D9 draw call
    gpu.set_vertex_shader(PASSTHROUGH_VS);
    gpu.set_pixel_shader(RED_PS);
    gpu.draw_triangle(&[
        Vertex { pos: [0.0, 0.5, 0.0], color: [1.0, 0.0, 0.0, 1.0] },
        Vertex { pos: [-0.5, -0.5, 0.0], color: [1.0, 0.0, 0.0, 1.0] },
        Vertex { pos: [0.5, -0.5, 0.0], color: [1.0, 0.0, 0.0, 1.0] },
    ]);
    
    let output = gpu.present().await;
    
    assert!(output.contains_red_triangle());

    // PF-007 graphics telemetry: even this trivial scene should record
    // at least one draw call and at least one pipeline bind.
    let stats = gpu.last_frame_telemetry();
    assert_eq!(stats.graphics.draw_calls, 1);
    assert!(stats.graphics.pipeline_switches >= 1);

    // GPU timing is best-effort (timestamp-query feature). In environments
    // without support (common in headless CI), it must be null/None rather
    // than causing a failure.
    if stats.graphics.gpu_timing.supported && stats.graphics.gpu_timing.enabled {
        assert!(stats.graphics.gpu_time_ms.is_some());
    } else {
        assert!(stats.graphics.gpu_time_ms.is_none());
    }
}
```

#### Browser GPU smoke tests (Playwright)

The repository includes a minimal harness page (`web/src/pages/gpu_smoke.html`) and a dedicated smoke-test GPU worker (`web/src/workers/gpu_smoke.worker.js`) used by Playwright (`tests/e2e/playwright/gpu_smoke.spec.ts`).

The smoke test does:

- create a canvas and transfer it to the worker (`OffscreenCanvas`)
- `present_test_pattern` (renders a deterministic quadrant pattern)
- `request_screenshot` (GPU readback into an RGBA buffer)
- SHA-256 hash compare against an expected value

WebGPU is treated as **optional** (gated on capability detection); the forced WebGL2 fallback smoke test is **required** so CI continues to validate the fallback path even if headless WebGPU is unavailable.

#### WebGPU notes for headless CI

Chromium's WebGPU availability varies by environment and version. When running headless, it may require browser flags (e.g. `--enable-unsafe-webgpu`) to expose `navigator.gpu`. If WebGPU is still unavailable (or readback fails), the WebGPU smoke test should be skipped rather than failing the suite, while the WebGL2 forced test remains mandatory.

#### D3D9Ex (DWM-facing) smoke test

Windows 7 composition uses **D3D9Ex**, so we need at least one guest-side test that exercises:

- `Direct3DCreate9Ex`
- `CreateDeviceEx`
- `PresentEx`
- `GetPresentStats` / `GetLastPresentCount`

The test should validate that the calls succeed and that present counts are monotonic (full-fidelity timing is not required for initial bring-up).

See: [D3D9Ex / DWM Compatibility](./16-d3d9ex-dwm-compatibility.md#tests).

#### D3D10/11 Conformance Scenes (SM4/SM5)

As D3D10/11 support comes online, grow a small suite of shader-based scenes that render to an offscreen texture and use pixel-compare against known-good outputs.
The intent is to validate the translator at the level D3D apps actually stress:

- constant buffers (cbuffers) and update patterns
- resource views (SRV/RTV/DSV, later UAV)
- input layout semantics mapping
- blend/depth/rasterizer state objects
- instancing and `baseVertex`

See: [16 - Direct3D 10/11 Translation (SM4/SM5 → WebGPU)](./16-d3d10-11-translation.md#conformance-suite-sm45--d3d11-features)

```rust
#[wasm_bindgen_test]
async fn test_d3d11_sm5_constant_buffer_updates() {
    let mut gpu = GpuEmulator::new().await;

    // Load SM5 DXBC shaders (VS/PS).
    gpu.d3d11_set_vertex_shader(SM5_TRIANGLE_VS_DXBC);
    gpu.d3d11_set_pixel_shader(SM5_COLOR_PS_DXBC);

    // Update cb0 every frame (WRITE_DISCARD-like pattern).
    for frame in 0..4 {
        gpu.d3d11_update_constant_buffer(0, &FrameConstants {
            color: [frame as f32 / 3.0, 0.0, 0.0, 1.0],
        });
        gpu.d3d11_draw_triangle();
    }

    let output = gpu.present().await;
    assert!(image_matches(output, "expected/d3d11_sm5_cb_updates.png", 0.995));
}
```

### Input Tests
 
```rust
#[test]
fn test_keyboard_scancode_translation() {
    let input = InputHandler::new();
    
    // Test common keys
    assert_eq!(input.translate_keycode("KeyA"), 0x1C);
    assert_eq!(input.translate_keycode("Space"), 0x29);
    assert_eq!(input.translate_keycode("Enter"), 0x5A);
    
    // Test extended keys
    let (scancode, extended) = input.translate_keycode_extended("ArrowUp");
    assert_eq!(scancode, 0x75);
    assert!(extended);
}

#[test]
fn test_mouse_packet_generation() {
    let mut mouse = Ps2Mouse::new();
    mouse.set_mode(MouseMode::Stream);
    
    // Move mouse
    mouse.movement(10, -5, 0);
    
    // Get packet
    let packet = mouse.get_packet();
    assert_eq!(packet.len(), 3);
    assert_eq!(packet[1], 10);   // X movement
    assert_eq!(packet[2], 5);    // Y movement (inverted)
}
```

---

## End-to-End Tests

### Application Compatibility Tests

```rust
// NOTE: End-to-end Windows application tests require a user-supplied Windows
// installation and are not expected to run in default OSS CI.
#[tokio::test]
async fn test_notepad() {
    let vm = boot_windows_7().await;
    
    // Launch notepad
    vm.press_keys(&["Win", "r"]);
    vm.type_text("notepad");
    vm.press_key("Enter");
    
    // Wait for notepad window
    vm.wait_for_window("Untitled - Notepad", Duration::from_secs(10)).await;
    
    // Type some text
    vm.type_text("Hello, World!");
    
    // Verify text appeared
    let screenshot = vm.take_screenshot();
    assert!(ocr_contains(screenshot, "Hello, World!"));
}

#[tokio::test]
async fn test_calculator() {
    let vm = boot_windows_7().await;
    
    vm.launch_application("calc.exe").await;
    
    // Perform calculation: 2 + 2 =
    vm.click_button("2");
    vm.click_button("+");
    vm.click_button("2");
    vm.click_button("=");
    
    // Check result
    let result = vm.read_calculator_display();
    assert_eq!(result, "4");
}

#[tokio::test]
async fn test_internet_explorer() {
    let vm = boot_windows_7().await;
    
    // Launch IE
    vm.launch_application("iexplore.exe").await;
    
    // Navigate to test page
    vm.type_in_address_bar("http://example.com");
    vm.press_key("Enter");
    
    // Wait for page load
    vm.wait_for_page_load(Duration::from_secs(30)).await;
    
    // Verify content
    let screenshot = vm.take_screenshot();
    assert!(ocr_contains(screenshot, "Example Domain"));
}
```

### Guest-side GPU driver validation (Windows 7)

To validate the AeroGPU WDDM driver stack end-to-end **inside a Windows 7 guest**, use the guest-side test suite in:

* `drivers/aerogpu/tests/win7/`

This suite contains small D3D9Ex/D3D11 programs that render a known pattern, read back pixels to assert correctness, and print a clear `PASS:`/`FAIL:` line (non-zero exit code on failure). A `run_all.cmd` harness is included to execute the suite and aggregate results.

These tests are intended for Win7 VMs with AeroGPU installed and are not expected to run in default OSS CI.

### Performance Benchmarks

Rust microbenchmarks in this repo use Criterion and live under `crates/*/benches/`. For CPU work,
start with:

- `crates/aero-cpu-core/benches/` (Criterion harness)

Note: the current `emulator_critical` microbench targets the legacy interpreter dispatch loop and
is gated behind `--features legacy-interp` (see [`docs/TESTING.md`](./TESTING.md) for the exact
commands used in CI). Tier-0 and future JIT microbenches should follow the same Criterion structure
but drive `exec::Tier0Interpreter` / `exec::ExecDispatcher`.

For D3D10/11 specifically, add a “many draws” microbench once the translation layer exists:

- 1–10k draw calls with a stable pipeline key (measures per-draw binding overhead)
- pipeline churn test (measures pipeline-cache behavior and compilation costs)
- constant-buffer update bandwidth (measures ring allocator / renaming strategy)

#### Benchmark output should include JIT telemetry (PF-006)

Raw throughput numbers (e.g. MIPS) are not enough to debug regressions in a tiered JIT: a 10% slowdown can come from "execution got slower" *or* "we started compiling more / compiling slower".

For any benchmark that executes guest code, the runner should also emit a compact `jit` summary derived from PF-006 telemetry, for example:

```
bench_instruction_throughput  ...  510.2 MIPS
jit: hit_rate=98.7% blocks(t1=1234,t2=56) compile_ms(t1=87.4,t2=45.1) compile_ms/s=3.4 deopt=0 guard_fail=0 cache=100MiB/256MiB
```

This makes it possible to attribute changes quickly:

- **MIPS ↓ + compile_ms/s ↑** → compilation overhead increased (thresholds, cache misses, slower passes)
- **MIPS ↓ + hit_rate ↓** → cache thrash / poor block keys / frequent invalidation
- **MIPS ↓ + deopt ↑** → unstable Tier 2 assumptions (guard policy, profiling, invalidation)

#### Synthetic workload to validate PF-006

Maintain at least one synthetic benchmark that intentionally forces compilation:

- Execute a large number of distinct basic blocks (e.g. by generating a code buffer with many unique addresses) to trigger Tier 1 compilation.
- Run long enough to promote a subset to Tier 2 (if Tier 2 exists).

Verification criteria:

- With JIT enabled: `blocks_compiled_total > 0` and `compile_ms_total > 0` in exported/printed metrics.
- With compilation disabled (interpreter-only): all `jit.*` totals remain `0`, and benchmark overhead stays minimal.

### Guest CPU Throughput Benchmarks (PF-008)

To measure emulator CPU performance **without** booting an OS image, run small deterministic x86/x86-64 payloads inside the CPU core and compute IPS/MIPS from retired instruction counts.

These benches are designed to run as soon as the interpreter exists and to show clear speedups when JIT tiers land. They must validate correctness via checksums and **fail the run** on mismatches.

See: [Guest CPU Instruction Throughput Benchmarks](./16-guest-cpu-benchmark-suite.md).

---

## Conformance Testing

### Against Reference Implementation

The repository includes an initial differential testing harness at `crates/conformance/` that
compares a small deterministic corpus against native host execution on `x86_64` (user-mode
instructions only).

Run it locally with:

```bash
cargo test --locked -p conformance --test conformance -- --nocapture
```

CI runs a fast subset on PRs and a larger corpus on a schedule via
`.github/workflows/conformance.yml`.

In addition, `tools/qemu_diff/` (Cargo package `qemu-diff`, crate identifier `qemu_diff`) provides a
**CI-friendly differential harness** that compares Aero execution against QEMU on synthetic 16-bit
snippets (no QEMU/GPL code shipped; QEMU is an external tool invoked by tests).

- `tools/qemu_diff/` builds a tiny bootable floppy image and runs it under an external
  `qemu-system-*` binary.
- `crates/aero-cpu-core` contains snippet runners that execute under the tier-0 engine (both a
  single-step path and a batch path) and compares results against QEMU.

Run locally:

```bash
# Tier-0 batch vs tier-0 single-step equivalence (always runs)
cargo test --locked -p aero-cpu-core

# Differential tests vs QEMU (skips if QEMU is not installed)
cargo test --locked -p aero-cpu-core --features qemu-diff
```

```rust
/// Compare Aero execution against a reference backend (e.g. native host execution or QEMU)
#[test]
fn conformance_test_instructions() {
    for instruction in ALL_X86_INSTRUCTIONS {
        let aero_result = run_in_aero(&instruction);
        let qemu_result = run_in_qemu(&instruction);
        
        assert_eq!(
            aero_result.registers, 
            qemu_result.registers,
            "Instruction {} produced different results",
            instruction.name
        );
        
        assert_eq!(
            aero_result.flags,
            qemu_result.flags,
            "Instruction {} produced different flags",
            instruction.name
        );
    }
}
```

### Instruction Set Coverage

```rust
#[test]
fn test_instruction_coverage() {
    let coverage = InstructionCoverage::new();
    
    // Run test suite
    run_all_tests(&mut coverage);
    
    // Check coverage
    let report = coverage.generate_report();
    
    println!("Instruction coverage: {:.2}%", report.percentage);
    println!("Missing instructions:");
    for inst in &report.uncovered {
        println!("  - {}", inst);
    }
    
    assert!(report.percentage >= 95.0, "Instruction coverage below 95%");
}
```

---

## Browser Testing

### Cross-Browser Test Suite

To balance fast PR feedback with high-confidence compatibility coverage, CI typically splits browser automation into:

- **PR CI:** run the Playwright suite on a single browser (Chromium) for speed.
- **Cross-browser CI:** run the suite across Chromium/Firefox/WebKit on a schedule and via manual trigger.

See `.github/workflows/e2e-matrix.yml` for the scheduled cross-browser matrix.

```javascript
// playwright.config.js
module.exports = {
    projects: [
        {
            name: 'chromium',
            use: { browserName: 'chromium' },
        },
        {
            name: 'firefox',
            use: { browserName: 'firefox' },
        },
        {
            name: 'webkit',
            use: { browserName: 'webkit' },
        },
    ],
};

// tests/browser.spec.js
test.describe('Browser Compatibility', () => {
    test('initializes emulator', async ({ page }) => {
        await page.goto('/');
        
        const status = await page.evaluate(() => {
            return window.aero.getStatus();
        });
        
        expect(status.initialized).toBe(true);
        expect(status.webgpu).toBe(true);
        expect(status.wasm_simd).toBe(true);
    });
    
    test('boots to desktop', async ({ page }) => {
        await page.goto('/');
        await page.click('#start-button');
        
        // Wait for boot
        await page.waitForFunction(() => {
            return window.aero.isBooted();
        }, { timeout: 120000 });
        
        // Take screenshot
        const screenshot = await page.screenshot();
        expect(screenshot).toMatchSnapshot('desktop.png');
    });
});
```

### Browser Integration Test: Persistent Shader Cache

Use a browser automation test to verify persistence across reloads:

1. Load the app.
2. Trigger a shader translation/compile path that is known to populate the persistent cache.
3. Capture telemetry counters (hits/misses, bytes written).
4. Reload the page (new JS context).
5. Trigger the same shader path again.
6. Assert that **persistent cache hits** increased and translation work did not run.

```javascript
test('persists shader translations across reload', async ({ page }) => {
  await page.goto('/');

  // Ensure clean slate.
  await page.evaluate(() => window.aero.gpu.clearCache());

  // Warm the cache.
  await page.evaluate(async () => {
    await window.aero.gpu.compileKnownShaderSetForTests();
  });
  const warmStats = await page.evaluate(() => window.aero.gpu.getCacheTelemetry());
  expect(warmStats.bytes_written).toBeGreaterThan(0);

  // New session.
  await page.reload();

  await page.evaluate(async () => {
    await window.aero.gpu.compileKnownShaderSetForTests();
  });
  const coldStats = await page.evaluate(() => window.aero.gpu.getCacheTelemetry());
  expect(coldStats.persistent_hits).toBeGreaterThan(0);
});
```

### GPU Presenter Color/Alpha Validation

In addition to full-system screenshots, we need a deterministic GPU-only validation that catches:

- double-applied gamma (too dark / too bright output)
- incorrect canvas alpha mode (premultiplied vs opaque haloing)
- backend-dependent Y-flips / UV convention mismatches

Use a simple **test card** (grayscale ramp + alpha gradient + corner markers) and hash the
presented pixels per backend. See `web/src/gpu/validation-scene.ts` and the Playwright spec
`tests/e2e/web/gpu_color.spec.ts`.

### CSP/COOP/COEP regression tests (implemented)

This repo includes a small WASM/JIT CSP PoC app plus Playwright coverage that asserts:

- COOP/COEP is enabled (`crossOriginIsolated === true`)
- a strict CSP without `wasm-unsafe-eval` blocks dynamic wasm compilation and triggers a fallback
- adding `script-src 'wasm-unsafe-eval'` enables dynamic compilation again

Entry points:

- PoC app: `web/public/wasm-jit-csp/` (served by `server/poc-server.mjs`)
- Tests: `tests/e2e/csp-fallback.spec.ts`
- Run: `npm run test:e2e`

---

## GPU Golden-Image Correctness Tests (Playwright)

The graphics subsystem needs **deterministic, automated correctness tests** that can catch subtle rendering regressions without requiring a full Windows boot.

This repository includes a minimal Playwright-based harness that:

- Renders **deterministic microtests**:
  - VGA-style text (chars + attrs)
  - VBE-style LFB color bars
  - WebGL2/WebGPU direct rendering microtests
  - GPU backend “smoke page” (`web/gpu-smoke.html`) capture
  - GPU trace replay (`tests/fixtures/triangle.aerogputrace`) capture
- Captures the rendered frame as **raw RGBA bytes** (WebGL2 `readPixels`, WebGPU buffer readback).
- Compares the output against committed **golden PNGs**.
- On failure, writes **expected/actual/diff** images as Playwright test artifacts.

Key files:

- `tests/e2e/playwright/gpu_golden.spec.ts` — microtests + capture
- `tests/e2e/playwright/utils/image_diff.ts` — pixel diff + artifact emission
- `tests/golden/*.png` — committed goldens (synthetic scenes only)
- `playwright.gpu.config.ts` — Playwright config (Chromium+WebGPU flags + Firefox WebGL2)

Local usage:

```bash
# Generate/update goldens (pure CPU generation, no browser required)
npm ci
npm run generate:goldens

# Run golden tests (requires Playwright browsers; CI installs/caches them via `.github/actions/setup-playwright`)
npm run test:gpu
```

---

## Visual regression (Playwright screenshots)

We use **Playwright screenshot assertions** as a "golden image" visual regression suite.

### Conventions

- **Snapshot location:** `tests/e2e/__screenshots__/…`
  - Configured via `playwright.config.ts` `snapshotPathTemplate`.
- **Naming:** each screenshot name is tied to:
  - test file path (`{testFilePath}`)
  - the screenshot name passed to `toHaveScreenshot('…')` (`{arg}`)
  - Playwright project (`{-projectName}`; typically `chromium`)
  - OS platform (`{-platform}`; `linux`/`darwin`/`win32`) to avoid cross-OS font rasterization churn.
  - Example: `tests/e2e/__screenshots__/visual.spec.ts/aero-window-chromium-linux.png`
- **Scope:** snapshots must only cover **synthetic scenes/UI we own**.
  - Do **not** add screenshots of Windows 7 or any copyrighted imagery.

### Rendering stability

Defaults live in `playwright.config.ts`:

- Fixed `viewport` and `deviceScaleFactor` for deterministic output.
- `reducedMotion: 'reduce'` + test-injected CSS to disable animations/transitions.
- Deterministic fonts: prefer explicitly declaring a font family known to exist in CI (e.g. `DejaVu Sans` on Linux) instead of relying on `system-ui`.
- Screenshot tolerance via:
  - `expect.toHaveScreenshot.maxDiffPixelRatio`
  - per-test overrides when needed.

### Developer workflow

- Run E2E + visual regression tests:
  - `npm run test:e2e`
- Update screenshot baselines after an intentional UI change:
  - `npm run test:e2e:update`
- Review diffs locally in the HTML report:
  - `npm run test:e2e:report` (opens `playwright-report/`)

### CI integration

The GitHub Actions workflow uploads artifacts on failures (including screenshot diffs):

- `playwright-report/` (HTML report)
- `test-results/` (attachments: diffs, traces, videos)

This makes snapshot mismatches easy to review directly from CI.

---

## Continuous Integration

### Running WASM unit tests locally

```bash
cd crates/aero-wasm
wasm-pack test --node
```

### GitHub Actions Workflow

```yaml
name: Aero CI

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup Rust
        id: setup-rust
        uses: ./.github/actions/setup-rust
        with:
          toolchain: stable
          targets: wasm32-unknown-unknown
          locked: always

      - name: Install wasm-pack
        uses: taiki-e/install-action@v2
        with:
          tool: wasm-pack
      
      - name: Run unit tests
        run: cargo test ${{ steps.setup-rust.outputs.cargo_locked_flag }} --all-features
      
      - name: Run WASM tests (node)
        working-directory: crates/aero-wasm
        run: wasm-pack test --node
      
      - name: Build
        run: cargo build ${{ steps.setup-rust.outputs.cargo_locked_flag }} --release --target wasm32-unknown-unknown
      
  browser-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup Node workspace
        uses: ./.github/actions/setup-node-workspace
        env:
          PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: "1"
      
      - name: Setup Playwright (cached)
        uses: ./.github/actions/setup-playwright
        with:
          browsers: chromium
      
      - name: Run browser tests
        run: npx playwright test
      
  benchmarks:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Run benchmarks
        run: |
          cargo bench --locked
          # Criterion writes results to `target/criterion/`. Move them out so they
          # don't get overwritten and so we can compare them against a baseline.
          rm -rf target/bench-new/criterion
          mkdir -p target/bench-new
          mv target/criterion target/bench-new/criterion
      
      - name: Compare with baseline
        run: |
          # See `.github/workflows/bench.yml` for the full baseline download + PR
          # base/head comparison logic.
          python3 scripts/bench_compare.py \
            --base baseline/target/bench-new/criterion \
            --new target/bench-new/criterion \
            --thresholds-file bench/perf_thresholds.json \
            --profile pr-smoke
```

#### CI note: GPU timing is optional

GPU timing via WebGPU timestamp queries (`timestamp-query`) should be treated as **informational**:

- Headless CI or software WebGPU adapters may not expose timestamp queries.
- Tests and smoke perf runs should validate counter-based metrics (draw calls, pipeline switches, uploads) without requiring GPU timing.
- If a CI lane wants to assert GPU timing, gate it behind an explicit opt-in (e.g. `AERO_ENABLE_GPU_TIMING=1`) and skip when unsupported.

---

## Guest driver validation (virtio)

For the paravirtualized “fast path” devices (virtio-blk/net/snd/input), validation requires both:

1. Host-side/unit tests for shared protocol structs (layout/ABI), and
2. In-guest smoke tests (Device Manager binding + basic throughput checks).

See `drivers/README.md` for the current driver-pack workflow and the minimal in-guest validation checklist.

---

## Test Data Management

**Important:** This repository must not include proprietary OS media (e.g., Windows ISOs/images) or other disallowed binary fixtures. Keep fixtures small and open-source, and prefer generating or downloading test assets during local/CI setup. See [Fixtures & Test Assets Policy](./FIXTURES.md).

### Disk Image Fixtures

```rust
// Generate minimal test disk images
fn create_test_disk(scenario: TestScenario) -> DiskImage {
    match scenario {
        TestScenario::EmptyDisk => DiskImage::empty(8 * MB),

        // CI-safe: checked in as tiny deterministic binaries generated from source.
        TestScenario::BootFixture => {
            DiskImage::from_file("tests/fixtures/boot/boot_vga_serial_8s.img")
        }

        // Open-source OS images are valuable too, but are typically generated or
        // downloaded during local setup (not committed).
        TestScenario::BootableDos => create_freedos_image(),

        // Aero does not ship Windows disk images. When developing locally, keep
        // any Windows image outside the repo and plumb it in via configuration.
        TestScenario::BootableWindows7 => DiskImage::from_path(
            std::env::var("AERO_WIN7_IMAGE")
                .expect("set AERO_WIN7_IMAGE to a locally-provided Windows 7 disk image"),
        ),
    }
}
```

---

## Next Steps

- See [Legal Considerations](./13-legal-considerations.md) for test image licensing
- See [Project Milestones](./14-project-milestones.md) for testing phases
