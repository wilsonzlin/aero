# 12 - Testing Strategy & Validation

## Overview

Comprehensive testing is critical for an emulator. We must verify correctness at the instruction level, system level, and application level.

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

---

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
    
    vm.load_disk("test_images/windows7.img").await;
    
    // Boot to login screen
    let screenshot = vm.run_until_stable_frame(Duration::from_secs(120)).await;
    
    // Visual regression test
    assert!(image_matches(screenshot, "expected/win7_login.png", 0.95));
}
```

### Graphics Tests

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

---

## Test Data Management

### Disk Image Fixtures

```rust
// Generate minimal test disk images
fn create_test_disk(scenario: TestScenario) -> DiskImage {
    match scenario {
        TestScenario::EmptyDisk => DiskImage::empty(100 * MB),
        TestScenario::BootableDos => create_freedos_image(),
        TestScenario::BootableWindows7 => load_reference_image("win7_base.img"),
    }
}
```

---

## Next Steps

- See [Legal Considerations](./13-legal-considerations.md) for test image licensing
- See [Project Milestones](./14-project-milestones.md) for testing phases
