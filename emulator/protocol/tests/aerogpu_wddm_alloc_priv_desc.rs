use aero_protocol::aerogpu::aerogpu_wddm_alloc::{
    aerogpu_wddm_alloc_priv_desc_format, aerogpu_wddm_alloc_priv_desc_height,
    aerogpu_wddm_alloc_priv_desc_pack, aerogpu_wddm_alloc_priv_desc_present,
    aerogpu_wddm_alloc_priv_desc_width, AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER,
    AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT, AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH,
};

#[test]
fn wddm_alloc_priv_desc_pack_unpack_roundtrips_and_matches_bit_layout() {
    let format_u32: u32 = 0x1122_3344;
    let width_u32: u32 = 640;
    let height_u32: u32 = 480;

    let expected = AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER
        | (format_u32 as u64 & 0xFFFF_FFFF)
        | ((width_u32 as u64 & 0xFFFF) << 32)
        | ((height_u32 as u64 & 0x7FFF) << 48);

    let packed = aerogpu_wddm_alloc_priv_desc_pack(format_u32, width_u32, height_u32);
    assert_eq!(packed, expected, "packed descriptor bits");

    assert!(aerogpu_wddm_alloc_priv_desc_present(packed));
    assert_eq!(aerogpu_wddm_alloc_priv_desc_format(packed), format_u32);
    assert_eq!(aerogpu_wddm_alloc_priv_desc_width(packed), width_u32);
    assert_eq!(aerogpu_wddm_alloc_priv_desc_height(packed), height_u32);
}

#[test]
fn wddm_alloc_priv_desc_marker_controls_present_bit() {
    assert!(!aerogpu_wddm_alloc_priv_desc_present(0));
    // High bit clear (bit63 not set).
    assert!(!aerogpu_wddm_alloc_priv_desc_present(1u64 << 62));
    assert!(aerogpu_wddm_alloc_priv_desc_present(
        AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER
    ));
}

#[test]
fn wddm_alloc_priv_desc_pack_masks_width_and_height() {
    let format_u32: u32 = 0xAABB_CCDD;
    let width_u32: u32 = 0x1_2345;
    let height_u32: u32 = 0xFFFF;

    let packed = aerogpu_wddm_alloc_priv_desc_pack(format_u32, width_u32, height_u32);
    assert!(aerogpu_wddm_alloc_priv_desc_present(packed));
    assert_eq!(aerogpu_wddm_alloc_priv_desc_format(packed), format_u32);
    assert_eq!(aerogpu_wddm_alloc_priv_desc_width(packed), 0x2345);
    assert_eq!(aerogpu_wddm_alloc_priv_desc_height(packed), 0x7FFF);
}

#[test]
fn wddm_alloc_priv_desc_max_constants_match_mask_ranges() {
    assert_eq!(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH, 0xFFFF);
    assert_eq!(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT, 0x7FFF);
}
