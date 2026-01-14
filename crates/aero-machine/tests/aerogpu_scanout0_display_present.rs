use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_devices_gpu::backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend,
};
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use memory::MemoryBus;
use pretty_assertions::assert_eq;

#[derive(Debug, Clone)]
struct StaticScanoutBackend {
    scanout: AeroGpuBackendScanout,
}

impl StaticScanoutBackend {
    fn new(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let mut rgba8 = vec![0u8; (width * height * 4) as usize];
        rgba8[..4].copy_from_slice(&rgba);
        Self {
            scanout: AeroGpuBackendScanout {
                width,
                height,
                rgba8,
            },
        }
    }
}

impl AeroGpuCommandBackend for StaticScanoutBackend {
    fn reset(&mut self) {}

    fn submit(
        &mut self,
        _mem: &mut dyn MemoryBus,
        _submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        Ok(())
    }

    fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
        Vec::new()
    }

    fn read_scanout_rgba8(&mut self, scanout_id: u32) -> Option<AeroGpuBackendScanout> {
        (scanout_id == 0).then(|| self.scanout.clone())
    }
}

#[test]
fn aerogpu_scanout0_display_present_reads_guest_framebuffer() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    // Enable A20 so guest physical addresses above 1MiB behave normally. This is required for
    // deterministic access to PCI MMIO BARs and for any framebuffer allocations above 1MiB.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Resolve AeroGPU BAR0 base assigned by BIOS POST.
    let aerogpu_bdf = profile::AEROGPU.bdf;
    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(aerogpu_bdf)
            .expect("AeroGPU must exist when enable_aerogpu=true");

        // Ensure BAR0 MMIO decode is enabled (COMMAND.MSE) and allow the device to initiate memory
        // reads (COMMAND.BME). The host-side `Machine::display_present` scanout readback path is
        // gated on BME to match real PCI device semantics.
        cfg.set_command(0x0006); // MEM + BME

        cfg.bar_range(profile::AEROGPU_BAR0_INDEX)
            .expect("missing AeroGPU BAR0")
            .base
    };
    assert_ne!(bar0_base, 0);

    // Guest framebuffer backing scanout0.
    let fb_gpa = 0x0020_0000u64;
    let (width, height) = (2u32, 2u32);
    let pitch_bytes = width * 4;

    // Use a WDDM-supported scanout format (`B8G8R8A8Unorm`). In guest memory this is BGRA order,
    // while the machine's host-facing framebuffer cache stores pixels as
    // `u32::from_le_bytes([r, g, b, a])`.
    let rgba = [0x12u8, 0x34, 0x56, 0x78];
    let pixel_bgra = [rgba[2], rgba[1], rgba[0], rgba[3]];
    let mut fb = vec![0u8; (width * height * 4) as usize];
    fb[0..4].copy_from_slice(&pixel_bgra);
    m.write_physical(fb_gpa, &fb);

    // Program scanout0 configuration over BAR0 MMIO.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        width,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        height,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        pitch_bytes,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    m.display_present();

    assert_eq!(m.display_resolution(), (width, height));
    assert_eq!(m.display_framebuffer()[0], u32::from_le_bytes(rgba));
}

#[test]
fn aerogpu_scanout0_display_present_prefers_backend_scanout_when_available() {
    let (width, height) = (2u32, 2u32);

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    // Enable A20 so guest physical addresses above 1MiB behave normally.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Install a backend that returns a known scanout. `display_present` should prefer this over
    // reading the guest scanout surface directly.
    let backend_rgba = [0xDEu8, 0xAD, 0xBE, 0xEF];
    let backend = StaticScanoutBackend::new(width, height, backend_rgba);
    let aerogpu = m.aerogpu().expect("AeroGPU MMIO device missing");
    aerogpu.borrow_mut().set_backend(Box::new(backend));

    // Resolve AeroGPU BAR0 base assigned by BIOS POST and enable PCI COMMAND.MEM+BME.
    let aerogpu_bdf = profile::AEROGPU.bdf;
    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(aerogpu_bdf)
            .expect("AeroGPU must exist when enable_aerogpu=true");
        cfg.set_command(0x0006); // MEM + BME
        cfg.bar_range(profile::AEROGPU_BAR0_INDEX)
            .expect("missing AeroGPU BAR0")
            .base
    };

    // Program a guest framebuffer with a *different* pixel so we can detect whether `display_present`
    // is reading from guest memory or from the backend.
    let fb_gpa = 0x0020_0000u64;
    let pitch_bytes = width * 4;
    let guest_rgba = [0x12u8, 0x34, 0x56, 0x78];
    let guest_pixel_bgra = [guest_rgba[2], guest_rgba[1], guest_rgba[0], guest_rgba[3]];
    let mut fb = vec![0u8; (width * height * 4) as usize];
    fb[0..4].copy_from_slice(&guest_pixel_bgra);
    m.write_physical(fb_gpa, &fb);

    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        width,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        height,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        pitch_bytes,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    m.display_present();

    assert_eq!(m.display_resolution(), (width, height));
    assert_eq!(m.display_framebuffer()[0], u32::from_le_bytes(backend_rgba));
}
