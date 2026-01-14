#![no_std]

pub const EDID_BLOCK_SIZE: usize = 128;

const MAX_PIXEL_CLOCK_HZ: u64 = u16::MAX as u64 * 10_000;
// Minimal blanking required by the synthesizer's porch/sync choices.
const MIN_H_BLANK: u32 = 24; // 8+8+8, aligned to 8 pixels.
const MIN_V_BLANK: u32 = 15; // 3+6+6.

/// Display timing information for the preferred mode encoded in the EDID.
///
/// Only a subset of timing parameters are exposed publicly; the generator will
/// synthesize a full Detailed Timing Descriptor (DTD).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub width: u16,
    pub height: u16,
    pub refresh_hz: u16,
}

impl Timing {
    /// The legacy/default preferred timing: 1024×768 @ 60Hz.
    pub const DEFAULT: Self = Self::new(1024, 768, 60);

    pub const fn new(width: u16, height: u16, refresh_hz: u16) -> Self {
        Self {
            width,
            height,
            refresh_hz,
        }
    }

    fn is_plausible(self) -> bool {
        // EDID Detailed Timing Descriptors store active pixel counts as 12-bit
        // values, and pixel clock as a 16-bit value in 10kHz units.
        //
        // We treat timings that cannot possibly be represented in a single base
        // EDID Detailed Timing Descriptor as implausible (e.g. extremely high
        // resolutions or refresh rates that would require a pixel clock above
        // 655.35MHz, even with minimal blanking).
        self.width != 0
            && self.height != 0
            && self.refresh_hz != 0
            // Range limits descriptor stores rates in u8 (Hz/kHz), so avoid
            // generating internally inconsistent EDIDs for extreme refresh rates.
            && self.refresh_hz <= u8::MAX as u16
            && (self.width as u32) <= 0x0FFF
            && (self.height as u32) <= 0x0FFF
            // Range limits descriptor also stores horizontal rate in kHz as u8.
            // Avoid generating a preferred mode whose horizontal scan rate
            // cannot be represented.
            && {
                // The range limits descriptor cannot represent horizontal scan
                // rates above 255kHz. The DTD's horizontal frequency depends on
                // the total vertical line count (`v_total`) and refresh:
                //
                //   h_freq_khz ≈ v_total * refresh_hz / 1000
                //
                // We can always reduce vertical blanking down to the minimum
                // porch/sync requirements, so check the best-case (minimum)
                // horizontal frequency.
                let v_active = self.height as u32;
                let v_total_min = v_active + MIN_V_BLANK;
                // Horizontal frequency in kHz is v_total * refresh / 1000.
                let h_freq_khz = ((v_total_min as u64) * (self.refresh_hz as u64) + 500) / 1000;
                h_freq_khz <= u8::MAX as u64
            }
            && {
                let min_pixel_clock_hz = (self.width as u64 + MIN_H_BLANK as u64)
                    * (self.height as u64 + MIN_V_BLANK as u64)
                    * self.refresh_hz as u64;
                min_pixel_clock_hz <= MAX_PIXEL_CLOCK_HZ
            }
    }
}

pub fn read_edid(block: u16) -> Option<[u8; EDID_BLOCK_SIZE]> {
    match block {
        // Backwards compatible: block 0 is always the base EDID using the
        // legacy 1024×768@60 preferred timing.
        0 => Some(generate_edid(Timing::DEFAULT)),
        _ => None,
    }
}

/// Generate a base EDID block (128 bytes) with a configurable preferred timing.
///
/// The returned EDID is self-contained (extension count 0) and includes a
/// single preferred Detailed Timing Descriptor (DTD) as the first descriptor.
pub fn generate_edid(preferred: Timing) -> [u8; EDID_BLOCK_SIZE] {
    // Avoid panics and avoid generating obviously invalid EDIDs if a caller
    // passes nonsense. Fall back to the legacy mode.
    let preferred = if preferred.is_plausible() {
        preferred
    } else {
        Timing::DEFAULT
    };

    let mut edid = [0u8; EDID_BLOCK_SIZE];

    // Header
    edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);

    // Manufacturer: "AER"
    edid[8..10].copy_from_slice(&0x04B2u16.to_be_bytes());
    // Product code (arbitrary)
    edid[10..12].copy_from_slice(&0x0001u16.to_le_bytes());
    // Serial number (unused)
    edid[12..16].copy_from_slice(&0u32.to_le_bytes());
    // Week/year of manufacture
    edid[16] = 1;
    edid[17] = 34; // 1990 + 34 = 2024
                   // EDID version/revision
    edid[18] = 1;
    edid[19] = 4;
    // Video input: digital, interface unspecified
    edid[20] = 0x80;
    // Screen size in cm
    edid[21] = 34;
    edid[22] = 27;
    // Gamma: 2.20
    edid[23] = 120;
    // Features: sRGB + preferred timing mode
    edid[24] = 0x06;

    // Chromaticity coordinates (sRGB-ish).
    edid[25] = 0xEE;
    edid[26] = 0x91;
    edid[27] = 0xA3;
    edid[28] = 0x54;
    edid[29] = 0x4C;
    edid[30] = 0x99;
    edid[31] = 0x26;
    edid[32] = 0x0F;
    edid[33] = 0x50;
    edid[34] = 0x54;

    // Established timings: 640x480@60, 800x600@60, 1024x768@60.
    edid[35] = 0x21;
    edid[36] = 0x08;
    edid[37] = 0x00;

    // Standard timings.
    fill_standard_timings(&mut edid, preferred);

    // Detailed timing descriptor #1: preferred timing.
    let preferred_dtd = dtd_bytes_for_timing(preferred);
    edid[54..72].copy_from_slice(&preferred_dtd);

    // Detailed descriptor #2: monitor name.
    edid[72..90].copy_from_slice(&[
        0x00, 0x00, 0x00, 0xFC, 0x00, b'A', b'E', b'R', b'O', b' ', b'V', b'G', b'A', 0x0A, 0x20,
        0x20, 0x20, 0x20,
    ]);

    // Detailed descriptor #3: range limits.
    edid[90..108].copy_from_slice(&range_limits_descriptor(preferred, &preferred_dtd));

    // Detailed descriptor #4: unused.
    edid[108..126].copy_from_slice(&[
        0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ]);

    // Extension block count.
    edid[126] = 0;
    edid[127] = checksum_byte(&edid);

    edid
}

fn align_up_u32(v: u32, align: u32) -> u32 {
    if align == 0 {
        return v;
    }
    v.div_ceil(align) * align
}

fn align_down_u32(v: u32, align: u32) -> u32 {
    if align == 0 {
        return v;
    }
    v / align * align
}

fn fill_standard_timings(edid: &mut [u8; EDID_BLOCK_SIZE], preferred: Timing) {
    // EDID standard timings are a small, fixed-size list. We keep the legacy
    // modes (1024×768/800×600/640×480 @ 60Hz) and, when possible, also include
    // the preferred timing so the EDID advertises it in multiple standard
    // places.
    //
    // This intentionally does not change the default EDID bytes: for the legacy
    // 1024×768@60 preferred timing, the output matches the previous hardcoded
    // standard timing table.
    let candidates = [
        preferred,
        Timing::new(1024, 768, 60),
        Timing::new(800, 600, 60),
        Timing::new(640, 480, 60),
    ];

    let mut slots = [[0x01u8, 0x01u8]; 8];
    let mut idx = 0usize;
    for t in candidates {
        if idx >= slots.len() {
            break;
        }
        let Some(bytes) = encode_standard_timing(t) else {
            continue;
        };
        if slots[..idx].contains(&bytes) {
            continue;
        }
        slots[idx] = bytes;
        idx += 1;
    }

    for (i, pair) in slots.iter().enumerate() {
        let base = 38 + i * 2;
        edid[base] = pair[0];
        edid[base + 1] = pair[1];
    }
}

fn encode_standard_timing(timing: Timing) -> Option<[u8; 2]> {
    // Standard timing encoding (EDID 1.4):
    // - Horizontal active = (byte0 + 31) * 8
    // - Aspect ratio = byte1 bits 7-6
    // - Refresh = (byte1 bits 5-0) + 60
    if timing.refresh_hz < 60 || timing.refresh_hz > 123 {
        return None;
    }
    if !timing.width.is_multiple_of(8) {
        return None;
    }

    let horiz_code = (timing.width / 8).checked_sub(31)?;
    if horiz_code > u8::MAX as u16 {
        return None;
    }

    let w = timing.width as u32;
    let h = timing.height as u32;
    let aspect_bits = if w * 10 == h * 16 {
        0u8 // 16:10
    } else if w * 3 == h * 4 {
        1u8 // 4:3
    } else if w * 4 == h * 5 {
        2u8 // 5:4
    } else if w * 9 == h * 16 {
        3u8 // 16:9
    } else {
        return None;
    };

    let refresh_code = (timing.refresh_hz - 60) as u8;
    Some([(horiz_code as u8), (aspect_bits << 6) | refresh_code])
}

fn range_limits_descriptor(preferred: Timing, preferred_dtd: &[u8; 18]) -> [u8; 18] {
    // Range limits are used by OSes to sanity-check which modes are legal. In
    // particular, the max pixel clock should be >= the preferred mode's pixel
    // clock, otherwise the preferred mode may be discarded.
    //
    // This crate always advertises the legacy 640×480/800×600/1024×768@60 modes
    // as established/standard timings, so we keep those baseline ranges and
    // widen them as needed to cover the preferred mode.
    let pixel_clock_10khz = u16::from_le_bytes([preferred_dtd[0], preferred_dtd[1]]);
    let pixel_clock_hz = pixel_clock_10khz as u64 * 10_000;

    let h_active = preferred_dtd[2] as u32 | (((preferred_dtd[4] & 0xF0) as u32) << 4);
    let h_blank = preferred_dtd[3] as u32 | (((preferred_dtd[4] & 0x0F) as u32) << 8);
    let h_total = h_active + h_blank;
    let h_freq_khz = if h_total == 0 {
        0u32
    } else {
        let denom = h_total as u64 * 1000;
        ((pixel_clock_hz + denom / 2) / denom) as u32
    };

    // Baseline modes: 50-75Hz vertical, 30-80kHz horizontal, 80MHz max pixel clock.
    let min_v_rate_hz = preferred.refresh_hz.clamp(1, 50) as u8;
    let max_v_rate_hz = preferred.refresh_hz.saturating_add(15).clamp(75, 255) as u8;

    let min_h_rate_khz = h_freq_khz.clamp(1, 30) as u8;
    let max_h_rate_khz = h_freq_khz.saturating_add(10).clamp(80, 255) as u8;

    let required_pclk_10mhz = pixel_clock_hz.div_ceil(10_000_000);
    let max_pixel_clock_10mhz = required_pclk_10mhz.saturating_add(1).clamp(8, 255) as u8;

    let mut desc = [0u8; 18];
    desc[0] = 0;
    desc[1] = 0;
    desc[2] = 0;
    desc[3] = 0xFD;
    desc[4] = 0x00;
    desc[5] = min_v_rate_hz;
    desc[6] = max_v_rate_hz;
    desc[7] = min_h_rate_khz;
    desc[8] = max_h_rate_khz;
    desc[9] = max_pixel_clock_10mhz;
    desc
}

fn dtd_bytes_for_timing(timing: Timing) -> [u8; 18] {
    if let Some(bytes) = known_dtd_bytes(timing) {
        return bytes;
    }
    synthesize_dtd_bytes(timing)
}

fn known_dtd_bytes(t: Timing) -> Option<[u8; 18]> {
    // These are common, standards-based timings (VESA DMT / CEA-861) encoded as
    // EDID Detailed Timing Descriptors. Keeping these around makes the default
    // EDID stable and ensures the most common modes look "real" to guests.
    Some(match (t.width, t.height, t.refresh_hz) {
        // 1024×768 @ 60Hz (VESA DMT).
        (1024, 768, 60) => [
            0x64, 0x19, // pixel clock: 65.00 MHz
            0x00, 0x40, 0x41, // hactive=1024, hblank=320
            0x00, 0x26, 0x30, // vactive=768, vblank=38
            0x18, 0x88, // hsync offset=24, hsync pulse=136
            0x36, 0x00, // vsync offset=3, vsync pulse=6
            0x54, 0x0E, 0x11, // image size: 340mm x 270mm
            0x00, 0x00, // borders
            0x18, // flags: digital separate sync, -hsync, -vsync
        ],
        // 800×600 @ 60Hz (VESA DMT).
        (800, 600, 60) => [
            0xA0, 0x0F, // pixel clock: 40.00 MHz
            0x20, 0x00, 0x31, // hactive=800, hblank=256
            0x58, 0x1C, 0x20, // vactive=600, vblank=28
            0x28, 0x80, // hsync offset=40, hsync pulse=128
            0x14, 0x00, // vsync offset=1, vsync pulse=4
            0x54, 0x0E, 0x11, // image size: 340mm x 270mm
            0x00, 0x00, // borders
            0x1E, // flags: digital separate sync, +hsync, +vsync
        ],
        // 640×480 @ 60Hz (VESA DMT / VGA).
        (640, 480, 60) => [
            0xD6, 0x09, // pixel clock: 25.18 MHz (rounded)
            0x80, 0xA0, 0x20, // hactive=640, hblank=160
            0xE0, 0x2D, 0x10, // vactive=480, vblank=45
            0x10, 0x60, // hsync offset=16, hsync pulse=96
            0xA2, 0x00, // vsync offset=10, vsync pulse=2
            0x54, 0x0E, 0x11, // image size: 340mm x 270mm
            0x00, 0x00, // borders
            0x18, // flags: digital separate sync, -hsync, -vsync
        ],
        // 1280×1024 @ 60Hz (VESA DMT).
        (1280, 1024, 60) => [
            0x30, 0x2A, // pixel clock: 108.00 MHz
            0x00, 0x98, 0x51, // hactive=1280, hblank=408
            0x00, 0x2A, 0x40, // vactive=1024, vblank=42
            0x30, 0x70, // hsync offset=48, hsync pulse=112
            0x13, 0x00, // vsync offset=1, vsync pulse=3
            0x54, 0x0E, 0x11, // image size: 340mm x 270mm
            0x00, 0x00, // borders
            0x1E, // flags: digital separate sync, +hsync, +vsync
        ],
        // 1280×720 @ 60Hz (CEA-861 720p60).
        (1280, 720, 60) => [
            0x01, 0x1D, // pixel clock: 74.25 MHz
            0x00, 0x72, 0x51, // hactive=1280, hblank=370
            0xD0, 0x1E, 0x20, // vactive=720, vblank=30
            0x6E, 0x28, // hsync offset=110, hsync pulse=40
            0x55, 0x00, // vsync offset=5, vsync pulse=5
            0x54, 0x0E, 0x11, // image size: 340mm x 270mm
            0x00, 0x00, // borders
            0x1E, // flags: digital separate sync, +hsync, +vsync
        ],
        // 1920×1080 @ 60Hz (CEA-861 1080p60).
        (1920, 1080, 60) => [
            0x02, 0x3A, // pixel clock: 148.50 MHz
            0x80, 0x18, 0x71, // hactive=1920, hblank=280
            0x38, 0x2D, 0x40, // vactive=1080, vblank=45
            0x58, 0x2C, // hsync offset=88, hsync pulse=44
            0x45, 0x00, // vsync offset=4, vsync pulse=5
            0x54, 0x0E, 0x11, // image size: 340mm x 270mm
            0x00, 0x00, // borders
            0x1E, // flags: digital separate sync, +hsync, +vsync
        ],
        _ => return None,
    })
}

fn synthesize_dtd_bytes(timing: Timing) -> [u8; 18] {
    // A simple, allocation-free timing synthesizer. This is intentionally
    // conservative and aims to produce a plausible, standards-like non-reduced
    // blanking mode.
    //
    // For common modes we use exact VESA/CEA timings above, which keeps the
    // default EDID stable and makes guest OSes happier.
    let h_active = timing.width as u32;
    let v_active = timing.height as u32;
    let refresh_hz = timing.refresh_hz as u32;

    // Horizontal blanking: ~20% of active, at least 160px, aligned to 8px.
    let mut h_blank = align_up_u32(h_active.div_ceil(5), 8).max(160);
    h_blank = h_blank.min(0x0FFF);

    // Vertical blanking: ~5% of active, at least enough for porches.
    let v_front_porch: u32 = 3;
    let v_sync_width: u32 = 6;
    let v_back_porch_min: u32 = 6;
    let min_v_blank = v_front_porch + v_sync_width + v_back_porch_min;
    let mut v_blank = v_active.div_ceil(20).clamp(min_v_blank, 0x0FFF);

    // Ensure the implied horizontal scan rate can be represented in the range
    // limits descriptor (u8 kHz). The DTD's horizontal frequency depends only
    // on `v_total` and the requested refresh rate, so clamp vertical blanking
    // down when required.
    //
    // We use a bound that keeps the *rounded-to-nearest* kHz value <= 255:
    //   (v_total * refresh + 500) / 1000 <= 255
    //   v_total * refresh <= 255_499
    let v_total_max = (255_499u64 / refresh_hz.max(1) as u64) as u32;
    let v_blank_max = v_total_max.saturating_sub(v_active).min(0x0FFF);
    if v_blank_max < min_v_blank {
        return known_dtd_bytes(Timing::DEFAULT).expect("missing default DTD");
    }
    v_blank = v_blank.min(v_blank_max).max(min_v_blank);

    // If the synthesized total would exceed the maximum EDID DTD pixel clock,
    // reduce blanking until it fits (or fall back).
    let mut h_total = h_active + h_blank;
    let mut v_total = v_active + v_blank;
    let mut pixel_clock_hz = (h_total as u64)
        .saturating_mul(v_total as u64)
        .saturating_mul(refresh_hz as u64);
    if pixel_clock_hz > MAX_PIXEL_CLOCK_HZ {
        fn fit_h_blank(h_active: u32, v_total: u32, refresh_hz: u32, h_blank: u32) -> Option<u32> {
            let denom = v_total as u64 * refresh_hz as u64;
            if denom == 0 {
                return None;
            }
            let h_total_max = (MAX_PIXEL_CLOCK_HZ / denom) as u32;
            let min_h_total = h_active.saturating_add(MIN_H_BLANK);
            if h_total_max < min_h_total {
                return None;
            }
            let mut h_blank_max = h_total_max.saturating_sub(h_active).min(0x0FFF);
            h_blank_max = align_down_u32(h_blank_max, 8);
            if h_blank_max < MIN_H_BLANK {
                return None;
            }
            Some(h_blank.min(h_blank_max).max(MIN_H_BLANK))
        }

        // First try to keep the vertical blanking as-is and reduce horizontal blanking.
        if let Some(new_h_blank) = fit_h_blank(h_active, v_total, refresh_hz, h_blank) {
            h_blank = new_h_blank;
        } else {
            // If we still can't fit, reduce vertical blanking to the minimum and retry.
            v_blank = v_blank.min(MIN_V_BLANK.max(min_v_blank));
            v_total = v_active + v_blank;
            if let Some(new_h_blank) = fit_h_blank(h_active, v_total, refresh_hz, h_blank) {
                h_blank = new_h_blank;
            } else {
                // Should be unreachable if `Timing::is_plausible` is used, but
                // preserve invariants by falling back to the legacy default.
                return known_dtd_bytes(Timing::DEFAULT).expect("missing default DTD");
            }
        }

        h_total = h_active + h_blank;
        pixel_clock_hz = (h_total as u64)
            .saturating_mul(v_total as u64)
            .saturating_mul(refresh_hz as u64);
    }

    // Horizontal sync/porches (must fit within blanking).
    let h_front_porch_min = 8;
    let h_sync_width_min = 8;
    let h_back_porch_min = 8;

    let mut h_front_porch = align_up_u32(h_blank / 8, 8).max(h_front_porch_min);
    let mut h_sync_width = align_up_u32(h_blank / 4, 8).max(h_sync_width_min);

    let needed = h_front_porch + h_sync_width + h_back_porch_min;
    if needed > h_blank {
        // Clamp to something that fits within blanking; preserve invariants by
        // shrinking sync widths first.
        let available = h_blank.saturating_sub(h_front_porch + h_back_porch_min);
        h_sync_width = h_sync_width
            .min(available)
            .max(h_sync_width_min.min(available));
        let available = h_blank.saturating_sub(h_sync_width + h_back_porch_min);
        h_front_porch = h_front_porch
            .min(available)
            .max(h_front_porch_min.min(available));
    }

    // Pixel clock (10kHz units). Round to nearest.
    let mut pixel_clock_10khz = ((pixel_clock_hz + 5_000) / 10_000) as u32;
    if pixel_clock_10khz == 0 {
        pixel_clock_10khz = 1;
    }
    // `Timing::is_plausible` and the blanking clamp logic above should ensure we
    // never need to truncate the clock, but keep a defensive clamp.
    pixel_clock_10khz = pixel_clock_10khz.min(u16::MAX as u32);

    // EDID DTD sync fields are limited in size (10-bit horizontal, 6-bit
    // vertical). Clamp conservatively.
    let h_sync_offset = h_front_porch.min(0x03FF);
    let h_sync_pulse = h_sync_width.min(0x03FF);
    let v_sync_offset = v_front_porch.min(0x003F);
    let v_sync_pulse = v_sync_width.min(0x003F);

    // Physical size: keep the legacy constant to avoid changing the default
    // EDID bytes. These are stored as 12-bit values.
    let h_size_mm: u32 = 340;
    let v_size_mm: u32 = 270;

    encode_dtd(
        pixel_clock_10khz as u16,
        h_active as u16,
        h_blank as u16,
        v_active as u16,
        v_blank as u16,
        h_sync_offset as u16,
        h_sync_pulse as u16,
        v_sync_offset as u16,
        v_sync_pulse as u16,
        h_size_mm as u16,
        v_size_mm as u16,
        0x1E, // flags: digital separate sync, +hsync, +vsync
    )
}

#[allow(clippy::too_many_arguments)]
fn encode_dtd(
    pixel_clock_10khz: u16,
    h_active: u16,
    h_blank: u16,
    v_active: u16,
    v_blank: u16,
    h_sync_offset: u16,
    h_sync_pulse: u16,
    v_sync_offset: u16,
    v_sync_pulse: u16,
    h_size_mm: u16,
    v_size_mm: u16,
    flags: u8,
) -> [u8; 18] {
    let mut dtd = [0u8; 18];
    dtd[0..2].copy_from_slice(&pixel_clock_10khz.to_le_bytes());

    dtd[2] = (h_active & 0x00FF) as u8;
    dtd[3] = (h_blank & 0x00FF) as u8;
    dtd[4] = ((h_active >> 8) as u8 & 0x0F) << 4 | ((h_blank >> 8) as u8 & 0x0F);

    dtd[5] = (v_active & 0x00FF) as u8;
    dtd[6] = (v_blank & 0x00FF) as u8;
    dtd[7] = ((v_active >> 8) as u8 & 0x0F) << 4 | ((v_blank >> 8) as u8 & 0x0F);

    dtd[8] = (h_sync_offset & 0x00FF) as u8;
    dtd[9] = (h_sync_pulse & 0x00FF) as u8;
    dtd[10] = ((v_sync_offset & 0x000F) as u8) << 4 | ((v_sync_pulse & 0x000F) as u8);

    dtd[11] = ((h_sync_offset >> 8) as u8 & 0x03) << 6
        | ((h_sync_pulse >> 8) as u8 & 0x03) << 4
        | ((v_sync_offset >> 4) as u8 & 0x03) << 2
        | ((v_sync_pulse >> 4) as u8 & 0x03);

    dtd[12] = (h_size_mm & 0x00FF) as u8;
    dtd[13] = (v_size_mm & 0x00FF) as u8;
    dtd[14] = ((h_size_mm >> 8) as u8 & 0x0F) << 4 | ((v_size_mm >> 8) as u8 & 0x0F);

    // Borders.
    dtd[15] = 0;
    dtd[16] = 0;
    dtd[17] = flags;
    dtd
}

fn checksum_byte(edid: &[u8; EDID_BLOCK_SIZE]) -> u8 {
    let sum = edid[..EDID_BLOCK_SIZE - 1]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    (0u8).wrapping_sub(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDID_HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

    fn checksum_ok(edid: &[u8; EDID_BLOCK_SIZE]) -> bool {
        edid.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)) == 0
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct DetailedTimingDescriptor {
        pixel_clock_hz: u64,
        h_active: u16,
        h_blank: u16,
        v_active: u16,
        v_blank: u16,
        h_sync_offset: u16,
        h_sync_pulse: u16,
        v_sync_offset: u16,
        v_sync_pulse: u16,
        flags: u8,
    }

    impl DetailedTimingDescriptor {
        fn h_total(self) -> u32 {
            self.h_active as u32 + self.h_blank as u32
        }

        fn v_total(self) -> u32 {
            self.v_active as u32 + self.v_blank as u32
        }

        fn refresh_hz(self) -> f64 {
            let total = self.h_total() as f64 * self.v_total() as f64;
            if total == 0.0 {
                return 0.0;
            }
            self.pixel_clock_hz as f64 / total
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct RangeLimitsDescriptor {
        min_v_rate_hz: u8,
        max_v_rate_hz: u8,
        min_h_rate_khz: u8,
        max_h_rate_khz: u8,
        max_pixel_clock_10mhz: u8,
    }

    fn parse_dtd(bytes: &[u8]) -> Option<DetailedTimingDescriptor> {
        assert_eq!(bytes.len(), 18);
        let pixel_clock_10khz = u16::from_le_bytes([bytes[0], bytes[1]]);
        if pixel_clock_10khz == 0 {
            return None;
        }

        let h_active = bytes[2] as u16 | (((bytes[4] & 0xF0) as u16) << 4);
        let h_blank = bytes[3] as u16 | (((bytes[4] & 0x0F) as u16) << 8);
        let v_active = bytes[5] as u16 | (((bytes[7] & 0xF0) as u16) << 4);
        let v_blank = bytes[6] as u16 | (((bytes[7] & 0x0F) as u16) << 8);

        let h_sync_offset = bytes[8] as u16 | ((((bytes[11] & 0xC0) >> 6) as u16) << 8);
        let h_sync_pulse = bytes[9] as u16 | ((((bytes[11] & 0x30) >> 4) as u16) << 8);

        let v_sync_offset = ((bytes[10] >> 4) as u16) | ((((bytes[11] & 0x0C) >> 2) as u16) << 4);
        let v_sync_pulse = (bytes[10] & 0x0F) as u16 | (((bytes[11] & 0x03) as u16) << 4);

        Some(DetailedTimingDescriptor {
            pixel_clock_hz: pixel_clock_10khz as u64 * 10_000,
            h_active,
            h_blank,
            v_active,
            v_blank,
            h_sync_offset,
            h_sync_pulse,
            v_sync_offset,
            v_sync_pulse,
            flags: bytes[17],
        })
    }

    fn parse_range_limits_descriptor(bytes: &[u8]) -> Option<RangeLimitsDescriptor> {
        assert_eq!(bytes.len(), 18);
        if bytes[0..5] != [0, 0, 0, 0xFD, 0x00] {
            return None;
        }

        Some(RangeLimitsDescriptor {
            min_v_rate_hz: bytes[5],
            max_v_rate_hz: bytes[6],
            min_h_rate_khz: bytes[7],
            max_h_rate_khz: bytes[8],
            max_pixel_clock_10mhz: bytes[9],
        })
    }

    fn parse_manufacturer_id(raw: u16) -> Option<[u8; 3]> {
        // EDID manufacturer ID encoding: 5-bit packed letters (A=1..Z=26).
        let a = ((raw >> 10) & 0x1F) as u8;
        let b = ((raw >> 5) & 0x1F) as u8;
        let c = (raw & 0x1F) as u8;

        fn decode(v: u8) -> Option<u8> {
            if (1..=26).contains(&v) {
                Some(b'A' + v - 1)
            } else {
                None
            }
        }

        Some([decode(a)?, decode(b)?, decode(c)?])
    }

    fn parse_standard_timing(bytes: [u8; 2]) -> Option<(u16, u16, u16)> {
        // EDID uses 0x01,0x01 to represent "unused".
        if bytes == [0x01, 0x01] {
            return None;
        }

        // Horizontal resolution in pixels: (byte1 + 31) * 8.
        let h_active = (bytes[0] as u16 + 31) * 8;
        if h_active == 0 {
            return None;
        }

        let aspect = bytes[1] >> 6;
        let v_active = match aspect {
            // 00: 16:10
            0 => (h_active as u32 * 10 / 16) as u16,
            // 01: 4:3
            1 => (h_active as u32 * 3 / 4) as u16,
            // 10: 5:4
            2 => (h_active as u32 * 4 / 5) as u16,
            // 11: 16:9
            3 => (h_active as u32 * 9 / 16) as u16,
            _ => return None,
        };

        let refresh = (bytes[1] & 0x3F) as u16 + 60;
        Some((h_active, v_active, refresh))
    }

    fn validate_descriptor(desc: &[u8]) {
        assert_eq!(desc.len(), 18);
        if let Some(dtd) = parse_dtd(desc) {
            // Basic sanity checks that catch malformed descriptors.
            assert!(dtd.h_active > 0);
            assert!(dtd.v_active > 0);
            assert!(dtd.h_blank > 0);
            assert!(dtd.v_blank > 0);
            assert!(dtd.pixel_clock_hz > 0);

            // Sync / porches must fit within blanking.
            assert!(dtd.h_sync_offset > 0);
            assert!(dtd.h_sync_pulse > 0);
            assert!(dtd.h_sync_offset as u32 + dtd.h_sync_pulse as u32 <= dtd.h_blank as u32);

            assert!(dtd.v_sync_offset > 0);
            assert!(dtd.v_sync_pulse > 0);
            assert!(dtd.v_sync_offset as u32 + dtd.v_sync_pulse as u32 <= dtd.v_blank as u32);

            // Totals should be non-zero and plausible.
            assert!(dtd.h_total() > dtd.h_active as u32);
            assert!(dtd.v_total() > dtd.v_active as u32);

            // "Digital separate sync" should be used for a modern virtual display.
            assert_eq!(dtd.flags & 0x18, 0x18);
        } else {
            // Monitor descriptor: the first 3 bytes are 0 for non-DTDs.
            assert_eq!(&desc[0..3], &[0, 0, 0]);
            let tag = desc[3];
            // We currently emit:
            // - 0xFC monitor name
            // - 0xFD range limits
            // - 0x10 unused (dummy) descriptor
            assert!(
                matches!(tag, 0xFC | 0xFD | 0x10),
                "unexpected monitor descriptor tag: {tag:#04x}"
            );
        }
    }

    #[test]
    fn edid_header_and_checksum_are_valid() {
        let edid = read_edid(0).expect("missing base EDID");
        assert_eq!(&edid[0..8], &EDID_HEADER);
        assert!(checksum_ok(&edid));

        // The checksum byte should match our checksum function.
        assert_eq!(edid[127], checksum_byte(&edid));
    }

    #[test]
    fn base_edid_fields_are_reasonable() {
        let edid = read_edid(0).expect("missing base EDID");

        // Manufacturer ID: "AER"
        let mfg_raw = u16::from_be_bytes([edid[8], edid[9]]);
        assert_eq!(
            parse_manufacturer_id(mfg_raw).expect("invalid manufacturer ID"),
            *b"AER"
        );

        // Product code: 0x0001 (little-endian).
        assert_eq!(u16::from_le_bytes([edid[10], edid[11]]), 0x0001);

        // EDID version/revision: 1.4.
        assert_eq!(edid[18], 1);
        assert_eq!(edid[19], 4);

        // Digital input flag set.
        assert_eq!(edid[20] & 0x80, 0x80);

        // Physical screen size in cm (used for DPI reporting).
        assert_eq!(edid[21], 34);
        assert_eq!(edid[22], 27);

        // Gamma: 2.20 encoded as (gamma*100) - 100.
        assert_eq!(edid[23], 120);

        // Features byte advertises sRGB + preferred timing.
        assert_eq!(edid[24] & 0x06, 0x06);

        // No extension blocks.
        assert_eq!(edid[126], 0);

        // Monitor name descriptor should be present and contain "AERO VGA".
        assert_eq!(&edid[72..77], &[0, 0, 0, 0xFC, 0x00]);
        assert_eq!(&edid[77..85], b"AERO VGA");
        assert_eq!(edid[85], 0x0A);

        // Established timings: 640x480@60, 800x600@60, 1024x768@60.
        assert_eq!(edid[35], 0x21);
        assert_eq!(edid[36], 0x08);
        assert_eq!(edid[37], 0x00);

        // Standard timings: first 3 should match the established modes; rest unused.
        let std0 = parse_standard_timing([edid[38], edid[39]]).expect("std timing #0 missing");
        let std1 = parse_standard_timing([edid[40], edid[41]]).expect("std timing #1 missing");
        let std2 = parse_standard_timing([edid[42], edid[43]]).expect("std timing #2 missing");
        assert_eq!(std0, (1024, 768, 60));
        assert_eq!(std1, (800, 600, 60));
        assert_eq!(std2, (640, 480, 60));

        for i in 3..8usize {
            let off = 38 + i * 2;
            assert_eq!([edid[off], edid[off + 1]], [0x01, 0x01]);
        }

        // Range limits descriptor should be the legacy baseline for the default mode.
        let range = parse_range_limits_descriptor(&edid[90..108]).expect("range limits missing");
        assert_eq!(
            range,
            RangeLimitsDescriptor {
                min_v_rate_hz: 50,
                max_v_rate_hz: 75,
                min_h_rate_khz: 30,
                max_h_rate_khz: 80,
                max_pixel_clock_10mhz: 8,
            }
        );
        // Reserved tail bytes must be zero.
        assert_eq!(&edid[100..108], &[0u8; 8]);
    }

    #[test]
    fn default_edid_keeps_legacy_preferred_mode_bytes() {
        let edid = generate_edid(Timing::DEFAULT);
        assert_eq!(
            &edid[54..72],
            &[
                0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
                0x11, 0x00, 0x00, 0x18
            ]
        );
    }

    #[test]
    fn detailed_descriptors_are_well_formed() {
        let edid = read_edid(0).expect("missing base EDID");
        for i in 0..4usize {
            let start = 54 + i * 18;
            let end = start + 18;
            validate_descriptor(&edid[start..end]);
        }
    }

    #[test]
    fn preferred_timing_can_be_overridden_and_parsed() {
        // Use a CEA timing that isn't the legacy mode to exercise configurability.
        let preferred = Timing::new(1920, 1080, 60);
        let edid = generate_edid(preferred);
        assert_eq!(&edid[0..8], &EDID_HEADER);
        assert!(checksum_ok(&edid));

        // The preferred mode is always encoded as DTD #1.
        let dtd = parse_dtd(&edid[54..72]).expect("preferred DTD missing");
        assert_eq!(dtd.h_active, preferred.width);
        assert_eq!(dtd.v_active, preferred.height);

        // Range limits should include the preferred mode's pixel clock and scan rates.
        let range = parse_range_limits_descriptor(&edid[90..108]).expect("range limits missing");
        let required_pclk_10mhz = dtd.pixel_clock_hz.div_ceil(10_000_000) as u8;
        assert!(
            range.max_pixel_clock_10mhz >= required_pclk_10mhz,
            "max_pixel_clock_10mhz={} required={required_pclk_10mhz}",
            range.max_pixel_clock_10mhz
        );

        let h_total = dtd.h_total() as u64;
        let h_khz = if h_total == 0 {
            0
        } else {
            ((dtd.pixel_clock_hz + (h_total * 1000) / 2) / (h_total * 1000)) as u64
        };
        assert!(
            (range.min_h_rate_khz as u64) <= h_khz && h_khz <= (range.max_h_rate_khz as u64),
            "h_khz={h_khz} range={}..={}",
            range.min_h_rate_khz,
            range.max_h_rate_khz
        );

        assert!(
            range.min_v_rate_hz <= preferred.refresh_hz as u8
                && preferred.refresh_hz as u8 <= range.max_v_rate_hz
        );

        // Refresh rate should be close to the requested one. We allow some
        // tolerance due to EDID pixel clock quantization (10kHz steps) and the
        // common practice of encoding "nominal" clocks.
        let refresh = dtd.refresh_hz();
        assert!(
            (refresh - preferred.refresh_hz as f64).abs() < 0.75,
            "refresh={refresh}"
        );
    }

    #[test]
    fn invalid_preferred_timing_falls_back_to_default() {
        let edid = generate_edid(Timing::new(0, 0, 0));
        assert_eq!(
            &edid[54..72],
            &[
                0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
                0x11, 0x00, 0x00, 0x18
            ]
        );
    }

    #[test]
    fn synthesized_timing_is_parsable_and_has_valid_ranges() {
        // 1366×768 is intentionally not in `known_dtd_bytes()` so we exercise the synthesizer.
        let preferred = Timing::new(1366, 768, 60);
        let edid = generate_edid(preferred);
        assert!(checksum_ok(&edid));

        let dtd = parse_dtd(&edid[54..72]).expect("preferred DTD missing");
        assert_eq!(dtd.h_active, preferred.width);
        assert_eq!(dtd.v_active, preferred.height);
        let refresh = dtd.refresh_hz();
        assert!(
            (refresh - preferred.refresh_hz as f64).abs() < 1.0,
            "refresh={refresh}"
        );

        let range = parse_range_limits_descriptor(&edid[90..108]).expect("range limits missing");
        let required_pclk_10mhz = dtd.pixel_clock_hz.div_ceil(10_000_000) as u8;
        assert!(range.max_pixel_clock_10mhz >= required_pclk_10mhz);
    }

    #[test]
    fn high_resolution_preferred_mode_is_synthesized_without_clock_clamping() {
        // This timing is within the DTD pixel clock limit, but the naive blanking heuristics can
        // push the pixel clock over the 655.35MHz ceiling. Ensure we shrink blanking rather than
        // clamping the clock (which would change the refresh rate).
        let preferred = Timing::new(4095, 2160, 60);
        let edid = generate_edid(preferred);
        assert!(checksum_ok(&edid));

        let dtd = parse_dtd(&edid[54..72]).expect("preferred DTD missing");
        assert_eq!(dtd.h_active, preferred.width);
        assert_eq!(dtd.v_active, preferred.height);
        assert!(dtd.pixel_clock_hz <= MAX_PIXEL_CLOCK_HZ);

        let refresh = dtd.refresh_hz();
        assert!(
            (refresh - preferred.refresh_hz as f64).abs() < 0.75,
            "refresh={refresh}"
        );
    }

    #[test]
    fn unrepresentable_preferred_mode_falls_back_to_default() {
        // Even with *zero* blanking, 4095×4095@60 would require > 655.35MHz, which cannot fit in
        // an EDID 1.4 DTD pixel clock field (16-bit in 10kHz units). We should reject it rather
        // than generating a clamped/incorrect DTD.
        let edid = generate_edid(Timing::new(4095, 4095, 60));
        assert_eq!(
            &edid[54..72],
            &[
                0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
                0x11, 0x00, 0x00, 0x18
            ]
        );
    }

    #[test]
    fn excessive_refresh_rate_falls_back_to_default() {
        // EDID range limits encode vertical rate as u8, so values above 255Hz cannot be
        // represented without internal inconsistency.
        let edid = generate_edid(Timing::new(640, 480, 300));
        assert_eq!(
            &edid[54..72],
            &[
                0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
                0x11, 0x00, 0x00, 0x18
            ]
        );
    }

    #[test]
    fn excessive_horizontal_frequency_falls_back_to_default() {
        // This timing fits within the DTD pixel clock limit, but the implied horizontal scan rate
        // is ~986kHz and cannot be represented in the range limits descriptor (u8 kHz).
        let edid = generate_edid(Timing::new(640, 4095, 240));
        assert_eq!(
            &edid[54..72],
            &[
                0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
                0x11, 0x00, 0x00, 0x18
            ]
        );
    }
}
