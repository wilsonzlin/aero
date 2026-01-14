use std::sync::Arc;

use aero_shared::scanout_state::{ScanoutState, SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_WDDM};
use emulator::devices::aerogpu_regs::mmio;
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::io::pci::{MmioDevice, PciDevice};
use memory::MemoryBus;

struct DummyMemory;

impl MemoryBus for DummyMemory {
    fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
        buf.fill(0);
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
}

fn new_test_device(scanout_state: Arc<ScanoutState>) -> AeroGpuPciDevice {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0, 0);
    dev.set_scanout_state(Some(scanout_state));
    // Enable PCI MMIO decode + bus mastering so register writes behave like a real enumerated device.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev
}

#[test]
fn scanout_state_updates_on_fb_gpa_flips_while_enabled() {
    let mut mem = DummyMemory;
    let scanout_state = Arc::new(ScanoutState::new());
    let mut dev = new_test_device(scanout_state.clone());

    // Program a minimal valid scanout configuration. The scanout state publisher intentionally
    // waits to publish WDDM scanout ownership until the config is valid (so legacy VGA/VBE output
    // is not suppressed by incomplete register programming).
    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 480;
    const BYTES_PER_PIXEL: u32 = 4;
    const PITCH_BYTES: u32 = WIDTH * BYTES_PER_PIXEL;

    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, WIDTH);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, HEIGHT);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, PITCH_BYTES);

    // Program an initial framebuffer address and enable scanout0 (WDDM).
    let fb0 = 0x0010_0000u64;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb0 as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb0 >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    let snap0 = scanout_state.snapshot();
    assert_eq!(snap0.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap0.base_paddr(), fb0);

    // Flip to a new framebuffer address. Drivers typically write LO then HI.
    let fb1 = 0x1234_5678_9abc_def0u64;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb1 as u32);

    // Must not publish a torn 64-bit update after only the LO write.
    let snap_after_lo = scanout_state.snapshot();
    assert_eq!(snap_after_lo.base_paddr(), fb0);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb1 >> 32) as u32);
    let snap1 = scanout_state.snapshot();
    assert_eq!(snap1.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap1.base_paddr(), fb1);

    // Flip again.
    let fb2 = 0x0fed_cba9_8765_4321u64;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb2 as u32);
    // Must not publish a torn 64-bit update after only the LO write.
    let snap_after_lo2 = scanout_state.snapshot();
    assert_eq!(snap_after_lo2.base_paddr(), fb1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb2 >> 32) as u32);
    let snap2 = scanout_state.snapshot();
    assert_eq!(snap2.base_paddr(), fb2);
}

#[test]
fn reset_reverts_scanout_state_to_legacy() {
    let mut mem = DummyMemory;
    let scanout_state = Arc::new(ScanoutState::new());
    let mut dev = new_test_device(scanout_state.clone());

    // Program a valid scanout configuration and enable it so WDDM owns scanout.
    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 480;
    const BYTES_PER_PIXEL: u32 = 4;
    const PITCH_BYTES: u32 = WIDTH * BYTES_PER_PIXEL;
    const FB: u64 = 0x1234_5678_9abc_def0;

    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, WIDTH);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, HEIGHT);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, PITCH_BYTES);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, FB as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (FB >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    let snap0 = scanout_state.snapshot();
    assert_eq!(snap0.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap0.base_paddr(), FB);

    // Reset should implicitly disable scanout0 and publish legacy scanout ownership.
    dev.reset();

    let snap1 = scanout_state.snapshot();
    assert_eq!(snap1.source, SCANOUT_SOURCE_LEGACY_TEXT);
}

#[test]
fn reset_clears_torn_fb_gpa_tracking() {
    let mut mem = DummyMemory;
    let scanout_state = Arc::new(ScanoutState::new());
    let mut dev = new_test_device(scanout_state.clone());

    // Simulate a torn FB address update: write LO but not HI.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, 0xdead_beef);

    // If a reset occurs in this state, we must not leave the publisher permanently blocked on the
    // stale LO write.
    dev.reset();

    // After reset, a valid scanout configuration should be able to claim WDDM scanout. If a reset
    // occurs mid-`FB_GPA` update (LO written without HI), it must not leave scanout state
    // publication permanently blocked.
    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 64;
    const PITCH_BYTES: u32 = WIDTH * 4;
    const FB: u64 = 0x1234_0000;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, WIDTH);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, HEIGHT);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, PITCH_BYTES);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, FB as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (FB >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), FB);
}

#[test]
fn enable_before_fb_gpa_does_not_steal_legacy_until_config_valid() {
    let mut mem = DummyMemory;
    let scanout_state = Arc::new(ScanoutState::new());
    let mut dev = new_test_device(scanout_state.clone());

    // Program everything except FB_GPA, then enable scanout. Windows drivers may briefly enable
    // scanout while FB_GPA=0 during early initialization; the device must not claim WDDM scanout in
    // this state (or it would blank the boot display).
    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 480;
    const BYTES_PER_PIXEL: u32 = 4;
    const PITCH_BYTES: u32 = WIDTH * BYTES_PER_PIXEL;

    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, WIDTH);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, HEIGHT);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, PITCH_BYTES);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );

    let legacy = scanout_state.snapshot();
    assert_eq!(legacy.source, SCANOUT_SOURCE_LEGACY_TEXT);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    let snap_after_enable = scanout_state.snapshot();
    assert_eq!(snap_after_enable, legacy);

    // Program a real framebuffer address while ENABLE remains 1. Claim/publish must occur only
    // once the combined 64-bit FB_GPA has been committed (HI written).
    let fb = 0x1234_5678_9abc_def0u64;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb as u32);
    let snap_after_lo = scanout_state.snapshot();
    assert_eq!(snap_after_lo, legacy);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb >> 32) as u32);
    let snap_after_hi = scanout_state.snapshot();
    assert_eq!(snap_after_hi.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap_after_hi.base_paddr(), fb);
    assert_eq!(snap_after_hi.width, WIDTH);
    assert_eq!(snap_after_hi.height, HEIGHT);
    assert_eq!(snap_after_hi.pitch_bytes, PITCH_BYTES);
}

#[test]
fn disable_before_claim_keeps_legacy_scanout() {
    let mut mem = DummyMemory;
    let scanout_state = Arc::new(ScanoutState::new());
    let mut dev = new_test_device(scanout_state.clone());

    // Stage a scanout config, but keep FB_GPA=0 (invalid).
    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 480;
    const BYTES_PER_PIXEL: u32 = 4;
    const PITCH_BYTES: u32 = WIDTH * BYTES_PER_PIXEL;

    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, WIDTH);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, HEIGHT);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, PITCH_BYTES);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );

    let legacy = scanout_state.snapshot();
    assert_eq!(legacy.source, SCANOUT_SOURCE_LEGACY_TEXT);

    // Enable scanout with invalid FB_GPA: must not steal legacy.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    assert_eq!(scanout_state.snapshot(), legacy);

    // Disable again before the configuration ever becomes valid. This must continue to present the
    // legacy scanout (no WDDM disabled/blank descriptor should be published because WDDM never
    // claimed ownership).
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 0);
    assert_eq!(scanout_state.snapshot(), legacy);
}

#[test]
fn invalid_pitch_does_not_claim_wddm_scanout_until_fixed() {
    let mut mem = DummyMemory;
    let scanout_state = Arc::new(ScanoutState::new());
    let mut dev = new_test_device(scanout_state.clone());

    // Program a scanout config with an invalid pitch (not a multiple of bytes-per-pixel) and
    // enable scanout. This must not claim WDDM scanout ownership (legacy display must remain
    // active).
    const WIDTH: u32 = 1;
    const HEIGHT: u32 = 1;
    const FB: u64 = 0x1234_5678_9abc_def0;
    const INVALID_PITCH_BYTES: u32 = 5; // not multiple of 4
    const VALID_PITCH_BYTES: u32 = 4;

    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, WIDTH);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, HEIGHT);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, INVALID_PITCH_BYTES);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, FB as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (FB >> 32) as u32);

    let legacy = scanout_state.snapshot();
    assert_eq!(legacy.source, SCANOUT_SOURCE_LEGACY_TEXT);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    assert_eq!(scanout_state.snapshot(), legacy);

    // Fix the pitch while scanout remains enabled. This must cause WDDM scanout to be claimed and
    // published immediately.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, VALID_PITCH_BYTES);
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), FB);
    assert_eq!(snap.width, WIDTH);
    assert_eq!(snap.height, HEIGHT);
    assert_eq!(snap.pitch_bytes, VALID_PITCH_BYTES);
}

#[test]
fn fb_gpa_overflow_does_not_claim_wddm_scanout_until_fixed() {
    let mut mem = DummyMemory;
    let scanout_state = Arc::new(ScanoutState::new());
    let mut dev = new_test_device(scanout_state.clone());

    // Program a scanout config whose address arithmetic overflows (`FB_GPA + scanout_size` wraps).
    // This must not claim WDDM scanout ownership.
    const WIDTH: u32 = 1;
    const HEIGHT: u32 = 2;
    const PITCH_BYTES: u32 = 4;
    // This overflows when adding end_offset (= 8 bytes for HEIGHT=2, PITCH=4, ROW_BYTES=4).
    const FB_OVERFLOW: u64 = u64::MAX - 4;
    const FB_VALID: u64 = 0x0010_0000;

    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, WIDTH);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, HEIGHT);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, PITCH_BYTES);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, FB_OVERFLOW as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (FB_OVERFLOW >> 32) as u32);

    let legacy = scanout_state.snapshot();
    assert_eq!(legacy.source, SCANOUT_SOURCE_LEGACY_TEXT);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    assert_eq!(scanout_state.snapshot(), legacy);

    // Fix the framebuffer address while scanout remains enabled; WDDM scanout should now claim and
    // publish the valid descriptor.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, FB_VALID as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (FB_VALID >> 32) as u32);
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), FB_VALID);
    assert_eq!(snap.width, WIDTH);
    assert_eq!(snap.height, HEIGHT);
    assert_eq!(snap.pitch_bytes, PITCH_BYTES);
}
