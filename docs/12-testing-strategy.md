# 12 - Testing Strategy & Validation

## Overview

Comprehensive testing is critical for an emulator. We must verify correctness at the instruction level, system level, and application level.

## Practical guide (running tests locally)

This document describes *what* we test and *why*. For the practical, developer-facing guide to running the full test stack locally (Rust, WASM, TypeScript, Playwright), plus common issues like COOP/COEP and WebGPU gating, see:

- [`TESTING.md`](./TESTING.md)

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

```rust
#[cfg(test)]
mod cpu_tests {
    use super::*;
    
    #[test]
    fn test_mov_reg_reg() {
        let mut cpu = CpuEmulator::new();
        
        cpu.set_reg(Reg::RAX, 0);
        cpu.set_reg(Reg::RBX, 0x12345678_9ABCDEF0);
        
        // MOV RAX, RBX (48 89 D8)
        cpu.execute_bytes(&[0x48, 0x89, 0xD8]);
        
        assert_eq!(cpu.get_reg(Reg::RAX), 0x12345678_9ABCDEF0);
        assert_eq!(cpu.rip, 3);
    }
    
    #[test]
    fn test_add_flags() {
        let mut cpu = CpuEmulator::new();
        
        // Test carry flag
        cpu.set_reg(Reg::RAX, 0xFFFFFFFF_FFFFFFFF);
        cpu.set_reg(Reg::RBX, 1);
        cpu.execute_bytes(&[0x48, 0x01, 0xD8]);  // ADD RAX, RBX
        
        assert_eq!(cpu.get_reg(Reg::RAX), 0);
        assert!(cpu.get_flag(Flag::CF));
        assert!(cpu.get_flag(Flag::ZF));
        assert!(!cpu.get_flag(Flag::SF));
    }
    
    #[test]
    fn test_overflow_flag() {
        let mut cpu = CpuEmulator::new();
        
        // Signed overflow: 0x7FFFFFFF + 1
        cpu.set_reg(Reg::EAX, 0x7FFFFFFF);
        cpu.set_reg(Reg::EBX, 1);
        cpu.execute_bytes(&[0x01, 0xD8]);  // ADD EAX, EBX
        
        assert_eq!(cpu.get_reg(Reg::EAX), 0x80000000);
        assert!(cpu.get_flag(Flag::OF));
        assert!(cpu.get_flag(Flag::SF));
        assert!(!cpu.get_flag(Flag::CF));
    }
    
    #[test]
    fn test_div_by_zero() {
        let mut cpu = CpuEmulator::new();
        
        cpu.set_reg(Reg::RAX, 100);
        cpu.set_reg(Reg::RDX, 0);
        cpu.set_reg(Reg::RCX, 0);
        
        let result = cpu.execute_bytes(&[0x48, 0xF7, 0xF1]);  // DIV RCX
        
        assert!(matches!(result, Err(Exception::DivideError)));
    }
    
    // Parameterized tests for comprehensive coverage
    #[test_case(0x00, 0x00, 0x00, false, false, true ; "0+0=0")]
    #[test_case(0x01, 0x01, 0x02, false, false, false ; "1+1=2")]
    #[test_case(0xFF, 0x01, 0x00, true, false, true ; "255+1=0 with carry")]
    #[test_case(0x7F, 0x01, 0x80, false, true, false ; "127+1=128 with overflow")]
    fn test_add_8bit(a: u8, b: u8, result: u8, cf: bool, of: bool, zf: bool) {
        let mut cpu = CpuEmulator::new();
        cpu.set_reg(Reg::AL, a as u64);
        cpu.set_reg(Reg::BL, b as u64);
        cpu.execute_bytes(&[0x00, 0xD8]);  // ADD AL, BL
        
        assert_eq!(cpu.get_reg(Reg::AL) as u8, result);
        assert_eq!(cpu.get_flag(Flag::CF), cf);
        assert_eq!(cpu.get_flag(Flag::OF), of);
        assert_eq!(cpu.get_flag(Flag::ZF), zf);
    }
}
```

### Memory Subsystem Tests

```rust
#[cfg(test)]
mod memory_tests {
    use super::*;
    
    #[test]
    fn test_page_table_walk() {
        let mut mmu = Mmu::new();
        let mut memory = MemoryBus::new(4 * GB);
        
        // Set up 4-level page tables
        setup_identity_mapping(&mut memory, 0, 4 * GB);
        mmu.set_cr3(PAGE_TABLE_BASE);
        mmu.enable_paging();
        
        // Test translation
        let paddr = mmu.translate(0x1000, AccessType::Read);
        assert_eq!(paddr, Ok(PhysAddr(0x1000)));
    }
    
    #[test]
    fn test_page_fault() {
        let mut mmu = Mmu::new();
        mmu.set_cr3(PAGE_TABLE_BASE);
        mmu.enable_paging();
        
        // Access unmapped address
        let result = mmu.translate(0xDEAD_BEEF_0000, AccessType::Read);
        
        assert!(matches!(result, Err(PageFault { .. })));
    }
    
    #[test]
    fn test_tlb_invalidation() {
        let mut mmu = Mmu::new();
        // ... setup
        
        // Populate TLB
        mmu.translate(0x1000, AccessType::Read);
        assert!(mmu.tlb_lookup(0x1000).is_some());
        
        // Invalidate
        mmu.invlpg(0x1000);
        assert!(mmu.tlb_lookup(0x1000).is_none());
    }
}
```

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
    assert!(jit.stats.mmu_translate_slow_calls < 10_000);
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
    let mmio_base = 0xFEC0_0000; // Local APIC (example)
    let program = assemble("
        mov eax, [0xFEC00030]   ; read APIC register
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
#[tokio::test]
async fn test_bios_boot() {
    let mut vm = VirtualMachine::new(Config {
        memory: 512 * MB,
        boot_device: BootDevice::HardDisk,
    });
    
    // Load FreeDOS boot disk
    vm.load_disk("test_images/freedos.img").await;
    
    // Run until we see DOS prompt
    let output = vm.run_until_output("C:\\>", Duration::from_secs(30)).await;
    
    assert!(output.contains("FreeDOS"));
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

The repository includes a minimal harness page (`web/src/pages/gpu_smoke.html`) and a dedicated smoke-test GPU worker (`web/src/workers/gpu_smoke.worker.js`) used by Playwright (`tests/playwright/gpu_smoke.spec.ts`).

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

```rust
#[bench]
fn bench_instruction_throughput(b: &mut Bencher) {
    let mut cpu = CpuEmulator::new();
    let code = assemble("
        mov rax, 1000000
    loop:
        dec rax
        jnz loop
        ret
    ");
    
    b.iter(|| {
        cpu.reset();
        cpu.execute_until_ret(&code);
    });
}

#[bench]
fn bench_memory_bandwidth(b: &mut Bencher) {
    let mut memory = MemoryBus::new(1 * GB);
    let buffer = vec![0u8; 1 * MB];
    
    b.iter(|| {
        for offset in (0..1 * GB).step_by(1 * MB) {
            memory.write_bulk(offset, &buffer);
        }
    });
    
    b.bytes = 1 * GB as u64;
}

#[bench]
fn bench_graphics_frame(b: &mut Bencher) {
    let mut gpu = GpuEmulator::new_sync();
    setup_aero_scene(&mut gpu);
    
    b.iter(|| {
        gpu.render_frame();
    });
}
```

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

```rust
/// Compare Aero execution against QEMU
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
`web/tests/gpu_color.spec.ts`.

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

- `tests/playwright/gpu_golden.spec.ts` — microtests + capture
- `tests/playwright/utils/image_diff.ts` — pixel diff + artifact emission
- `tests/golden/*.png` — committed goldens (synthetic scenes only)
- `playwright.gpu.config.ts` — Playwright config (Chromium+WebGPU flags + Firefox WebGL2)

Local usage:

```bash
# Generate/update goldens (pure CPU generation, no browser required)
npm install
npm run generate:goldens

# Run golden tests (requires Playwright browsers; CI runs `npx playwright install --with-deps`)
npm run test:gpu
```

---

## Continuous Integration

### GitHub Actions Workflow

```yaml
name: Aero CI

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Install Rust
        uses: actions-rs/toolchain@v1
        with:
          toolchain: nightly
          target: wasm32-unknown-unknown
      
      - name: Run unit tests
        run: cargo test --all-features
      
      - name: Run WASM tests
        run: wasm-pack test --headless --chrome
      
      - name: Build
        run: cargo build --release --target wasm32-unknown-unknown
      
  browser-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Install dependencies
        run: npm ci
      
      - name: Install Playwright browsers
        run: npx playwright install
      
      - name: Run browser tests
        run: npx playwright test
      
  benchmarks:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Run benchmarks
        run: cargo bench
      
       - name: Compare with baseline
         run: ./scripts/compare-benchmarks.sh
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
        TestScenario::EmptyDisk => DiskImage::empty(100 * MB),
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
