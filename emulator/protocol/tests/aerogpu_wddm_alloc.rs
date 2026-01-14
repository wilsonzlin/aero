use aero_protocol::aerogpu::aerogpu_wddm_alloc::{
    AerogpuWddmAllocKind, AerogpuWddmAllocPriv, AerogpuWddmAllocPrivAny, AerogpuWddmAllocPrivV2,
    AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE, AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED,
    AEROGPU_WDDM_ALLOC_PRIV_MAGIC, AEROGPU_WDDM_ALLOC_PRIV_VERSION,
    AEROGPU_WDDM_ALLOC_PRIV_VERSION_2,
};

fn write_u32_le(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn write_u64_le(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn decode_wddm_alloc_priv_v1_decodes_expected_bytes() {
    let mut buf = vec![0u8; AerogpuWddmAllocPriv::SIZE_BYTES];
    write_u32_le(&mut buf, 0, AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
    write_u32_le(&mut buf, 4, AEROGPU_WDDM_ALLOC_PRIV_VERSION);
    write_u32_le(&mut buf, 8, 0x1122_3344);
    write_u32_le(
        &mut buf,
        12,
        AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED | AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE,
    );
    write_u64_le(&mut buf, 16, 0x0102_0304_0506_0708);
    write_u64_le(&mut buf, 24, 0x1111_2222_3333_4444);
    write_u64_le(&mut buf, 32, 0x5555_6666_7777_8888);

    let decoded = AerogpuWddmAllocPriv::decode_from_le_bytes(&buf).unwrap();
    // Avoid taking references to packed fields inside `assert_eq!` by copying to locals first.
    let magic = decoded.magic;
    let version = decoded.version;
    let alloc_id = decoded.alloc_id;
    let flags = decoded.flags;
    let share_token = decoded.share_token;
    let size_bytes = decoded.size_bytes;
    let reserved0 = decoded.reserved0;

    assert_eq!(magic, AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
    assert_eq!(version, AEROGPU_WDDM_ALLOC_PRIV_VERSION);
    assert_eq!(alloc_id, 0x1122_3344);
    assert_eq!(
        flags,
        AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED | AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE
    );
    assert_eq!(share_token, 0x0102_0304_0506_0708);
    assert_eq!(size_bytes, 0x1111_2222_3333_4444);
    assert_eq!(reserved0, 0x5555_6666_7777_8888);
}

#[test]
fn decode_wddm_alloc_priv_v2_decodes_expected_bytes() {
    let mut buf = vec![0u8; AerogpuWddmAllocPrivV2::SIZE_BYTES];
    write_u32_le(&mut buf, 0, AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
    write_u32_le(&mut buf, 4, AEROGPU_WDDM_ALLOC_PRIV_VERSION_2);
    write_u32_le(&mut buf, 8, 0x99AA_BBCC);
    write_u32_le(&mut buf, 12, AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED);
    write_u64_le(&mut buf, 16, 0x1020_3040_5060_7080);
    write_u64_le(&mut buf, 24, 0x1000);
    write_u64_le(&mut buf, 32, 0);
    write_u32_le(&mut buf, 40, AerogpuWddmAllocKind::Texture2d as u32);
    write_u32_le(&mut buf, 44, 1920);
    write_u32_le(&mut buf, 48, 1080);
    write_u32_le(&mut buf, 52, 87);
    write_u32_le(&mut buf, 56, 1920 * 4);
    write_u32_le(&mut buf, 60, 0);

    let decoded = AerogpuWddmAllocPrivV2::decode_from_le_bytes(&buf).unwrap();
    let version = decoded.version;
    let kind = decoded.kind;
    let width = decoded.width;
    let height = decoded.height;
    let format = decoded.format;
    let row_pitch_bytes = decoded.row_pitch_bytes;
    let reserved1 = decoded.reserved1;

    assert_eq!(version, AEROGPU_WDDM_ALLOC_PRIV_VERSION_2);
    assert_eq!(kind, AerogpuWddmAllocKind::Texture2d as u32);
    assert_eq!(width, 1920);
    assert_eq!(height, 1080);
    assert_eq!(format, 87);
    assert_eq!(row_pitch_bytes, 1920 * 4);
    assert_eq!(reserved1, 0);
}

#[test]
fn decode_wddm_alloc_priv_any_validates_magic_and_version() {
    // Bad magic is rejected.
    {
        let mut buf = vec![0u8; AerogpuWddmAllocPriv::SIZE_BYTES];
        write_u32_le(&mut buf, 0, 0xDEAD_BEEF);
        write_u32_le(&mut buf, 4, AEROGPU_WDDM_ALLOC_PRIV_VERSION);
        assert!(
            AerogpuWddmAllocPrivAny::decode_from_le_bytes(&buf).is_none(),
            "expected bad magic to be rejected"
        );
    }

    // Unknown version is rejected.
    {
        let mut buf = vec![0u8; AerogpuWddmAllocPriv::SIZE_BYTES];
        write_u32_le(&mut buf, 0, AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
        write_u32_le(&mut buf, 4, 999);
        assert!(
            AerogpuWddmAllocPrivAny::decode_from_le_bytes(&buf).is_none(),
            "expected unknown version to be rejected"
        );
    }

    // V1 is accepted.
    {
        let mut buf = vec![0u8; AerogpuWddmAllocPriv::SIZE_BYTES];
        write_u32_le(&mut buf, 0, AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
        write_u32_le(&mut buf, 4, AEROGPU_WDDM_ALLOC_PRIV_VERSION);
        assert!(
            matches!(
                AerogpuWddmAllocPrivAny::decode_from_le_bytes(&buf),
                Some(AerogpuWddmAllocPrivAny::V1(_))
            ),
            "expected v1 to decode as V1"
        );
    }

    // V2 is accepted.
    {
        let mut buf = vec![0u8; AerogpuWddmAllocPrivV2::SIZE_BYTES];
        write_u32_le(&mut buf, 0, AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
        write_u32_le(&mut buf, 4, AEROGPU_WDDM_ALLOC_PRIV_VERSION_2);
        assert!(
            matches!(
                AerogpuWddmAllocPrivAny::decode_from_le_bytes(&buf),
                Some(AerogpuWddmAllocPrivAny::V2(_))
            ),
            "expected v2 to decode as V2"
        );
    }
}
