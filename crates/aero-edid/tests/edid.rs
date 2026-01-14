#[test]
fn edid_has_valid_header_and_checksum() {
    let edid = aero_edid::read_edid(0).expect("missing base EDID");
    assert_eq!(
        &edid[0..8],
        &[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]
    );

    let sum = edid.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    assert_eq!(sum, 0);
}

#[test]
fn edid_includes_1024x768_dtd() {
    let edid = aero_edid::read_edid(0).expect("missing base EDID");
    assert_eq!(
        &edid[54..72],
        &[
            0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
            0x11, 0x00, 0x00, 0x18
        ]
    );
}

#[test]
fn read_edid_returns_none_for_extension_blocks() {
    // The generated EDID advertises 0 extension blocks, so only block 0 should exist.
    assert!(aero_edid::read_edid(1).is_none());
    assert!(aero_edid::read_edid(2).is_none());
}

fn checksum_ok(edid: &[u8; aero_edid::EDID_BLOCK_SIZE]) -> bool {
    edid.iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b))
        == 0
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Dtd {
    h_active: u16,
    v_active: u16,
    pixel_clock_hz: u64,
    h_total: u32,
    v_total: u32,
}

impl Dtd {
    fn refresh_hz(self) -> f64 {
        let denom = self.h_total as f64 * self.v_total as f64;
        if denom == 0.0 {
            return 0.0;
        }
        self.pixel_clock_hz as f64 / denom
    }
}

fn parse_dtd(bytes: &[u8]) -> Option<Dtd> {
    if bytes.len() != 18 {
        return None;
    }
    let pixel_clock_10khz = u16::from_le_bytes([bytes[0], bytes[1]]);
    if pixel_clock_10khz == 0 {
        return None;
    }
    let h_active = bytes[2] as u16 | (((bytes[4] & 0xF0) as u16) << 4);
    let h_blank = bytes[3] as u16 | (((bytes[4] & 0x0F) as u16) << 8);
    let v_active = bytes[5] as u16 | (((bytes[7] & 0xF0) as u16) << 4);
    let v_blank = bytes[6] as u16 | (((bytes[7] & 0x0F) as u16) << 8);
    Some(Dtd {
        h_active,
        v_active,
        pixel_clock_hz: pixel_clock_10khz as u64 * 10_000,
        h_total: h_active as u32 + h_blank as u32,
        v_total: v_active as u32 + v_blank as u32,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RangeLimits {
    min_v_rate_hz: u8,
    max_v_rate_hz: u8,
    min_h_rate_khz: u8,
    max_h_rate_khz: u8,
    max_pixel_clock_10mhz: u8,
}

fn parse_range_limits(bytes: &[u8]) -> Option<RangeLimits> {
    if bytes.len() != 18 {
        return None;
    }
    if bytes[0..5] != [0, 0, 0, 0xFD, 0x00] {
        return None;
    }
    Some(RangeLimits {
        min_v_rate_hz: bytes[5],
        max_v_rate_hz: bytes[6],
        min_h_rate_khz: bytes[7],
        max_h_rate_khz: bytes[8],
        max_pixel_clock_10mhz: bytes[9],
    })
}

#[test]
fn generate_edid_preferred_mode_is_sane() {
    let preferred = aero_edid::Timing::new(1920, 1080, 60);
    let edid = aero_edid::generate_edid(preferred);
    assert!(checksum_ok(&edid));

    let dtd = parse_dtd(&edid[54..72]).expect("missing preferred DTD");
    assert_eq!(dtd.h_active, preferred.width);
    assert_eq!(dtd.v_active, preferred.height);
    let refresh = dtd.refresh_hz();
    assert!((refresh - 60.0).abs() < 0.75, "refresh={refresh}");

    let range = parse_range_limits(&edid[90..108]).expect("missing range limits descriptor");
    let required_pclk_10mhz = ((dtd.pixel_clock_hz + 9_999_999) / 10_000_000) as u8;
    assert!(range.max_pixel_clock_10mhz >= required_pclk_10mhz);
}

#[test]
fn generate_edid_synthesized_mode_is_sane() {
    // 1366×768 is not part of our hardcoded known DTD table, so this exercises
    // the synthesizer path via the public API.
    let preferred = aero_edid::Timing::new(1366, 768, 60);
    let edid = aero_edid::generate_edid(preferred);
    assert!(checksum_ok(&edid));

    let dtd = parse_dtd(&edid[54..72]).expect("missing preferred DTD");
    assert_eq!(dtd.h_active, preferred.width);
    assert_eq!(dtd.v_active, preferred.height);
    let refresh = dtd.refresh_hz();
    assert!((refresh - 60.0).abs() < 1.0, "refresh={refresh}");
}

#[test]
fn generate_edid_high_resolution_mode_is_sane() {
    // This timing is within the DTD pixel clock limit, but would exceed it if the
    // generator always used the default blanking heuristic. Ensure `generate_edid`
    // keeps refresh near the requested value (i.e. does not clamp the clock).
    let preferred = aero_edid::Timing::new(4095, 2160, 60);
    let edid = aero_edid::generate_edid(preferred);
    assert!(checksum_ok(&edid));

    let dtd = parse_dtd(&edid[54..72]).expect("missing preferred DTD");
    assert_eq!(dtd.h_active, preferred.width);
    assert_eq!(dtd.v_active, preferred.height);
    let refresh = dtd.refresh_hz();
    assert!((refresh - 60.0).abs() < 0.75, "refresh={refresh}");
}

#[test]
fn generate_edid_rejects_unrepresentable_preferred_mode() {
    // Even with minimal blanking, this would exceed 655.35MHz and cannot be represented in a DTD.
    let edid = aero_edid::generate_edid(aero_edid::Timing::new(4095, 4095, 60));
    // Should fall back to the legacy 1024x768@60 DTD.
    assert_eq!(
        &edid[54..72],
        &[
            0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
            0x11, 0x00, 0x00, 0x18
        ]
    );
}

#[test]
fn generate_edid_rejects_excessive_refresh_rate() {
    // EDID range limits encode rates as u8, so >255Hz cannot be represented consistently.
    let edid = aero_edid::generate_edid(aero_edid::Timing::new(640, 480, 300));
    assert_eq!(
        &edid[54..72],
        &[
            0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
            0x11, 0x00, 0x00, 0x18
        ]
    );
}
