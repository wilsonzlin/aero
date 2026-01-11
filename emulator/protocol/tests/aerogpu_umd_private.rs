use aero_protocol::aerogpu::aerogpu_umd_private::{
    AerogpuUmdPrivateDecodeError, AerogpuUmdPrivateV1, AEROGPU_UMDPRIV_STRUCT_VERSION_V1,
};

fn write_u32_le(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn decode_umd_private_v1_accepts_extended_size_bytes() {
    let mut buf = vec![0u8; AerogpuUmdPrivateV1::SIZE_BYTES];
    write_u32_le(&mut buf, 0, (AerogpuUmdPrivateV1::SIZE_BYTES as u32) + 16);
    write_u32_le(&mut buf, 4, AEROGPU_UMDPRIV_STRUCT_VERSION_V1);

    let decoded = AerogpuUmdPrivateV1::decode_from_le_bytes_checked(&buf).unwrap();
    let size_bytes = decoded.size_bytes;
    assert_eq!(size_bytes, (AerogpuUmdPrivateV1::SIZE_BYTES as u32) + 16);
}

#[test]
fn decode_umd_private_v1_rejects_too_small_size_bytes() {
    let mut buf = vec![0u8; AerogpuUmdPrivateV1::SIZE_BYTES];
    write_u32_le(&mut buf, 0, (AerogpuUmdPrivateV1::SIZE_BYTES as u32) - 1);
    write_u32_le(&mut buf, 4, AEROGPU_UMDPRIV_STRUCT_VERSION_V1);

    let err = AerogpuUmdPrivateV1::decode_from_le_bytes_checked(&buf)
        .err()
        .unwrap();
    assert!(matches!(
        err,
        AerogpuUmdPrivateDecodeError::BadSizeField { .. }
    ));
}

#[test]
fn decode_umd_private_v1_rejects_unsupported_struct_version() {
    let mut buf = vec![0u8; AerogpuUmdPrivateV1::SIZE_BYTES];
    write_u32_le(&mut buf, 0, AerogpuUmdPrivateV1::SIZE_BYTES as u32);
    write_u32_le(&mut buf, 4, AEROGPU_UMDPRIV_STRUCT_VERSION_V1 + 1);

    let err = AerogpuUmdPrivateV1::decode_from_le_bytes_checked(&buf)
        .err()
        .unwrap();
    assert!(matches!(
        err,
        AerogpuUmdPrivateDecodeError::UnsupportedStructVersion { .. }
    ));
}
