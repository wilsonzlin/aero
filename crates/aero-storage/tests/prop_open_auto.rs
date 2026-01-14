#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{DiskImage, MemBackend};
use proptest::prelude::*;

const MAX_IMAGE_BYTES: usize = 64 * 1024;

fn qcow2_truncated_header_strategy() -> impl Strategy<Value = Vec<u8>> {
    // The QCOW2 header is at least 72 bytes, but `detect_format` intentionally treats any file
    // starting with the magic as QCOW2 (even if truncated) so the user gets a structured
    // corruption error instead of falling back to raw.
    //
    // Generate a variety of truncated lengths, but keep the total bytes bounded.
    (4usize..72).prop_flat_map(|len| {
        let tail_len = len - 4;
        prop::collection::vec(any::<u8>(), tail_len).prop_map(move |tail| {
            let mut out = Vec::with_capacity(len);
            out.extend_from_slice(b"QFI\xfb");
            out.extend_from_slice(&tail);
            out
        })
    })
}

fn aerosparse_truncated_header_strategy() -> impl Strategy<Value = Vec<u8>> {
    // AeroSparse headers are 64 bytes. Generate truncated buffers that still include the magic.
    (8usize..64).prop_flat_map(|len| {
        let tail_len = len - 8;
        prop::collection::vec(any::<u8>(), tail_len).prop_map(move |tail| {
            let mut out = Vec::with_capacity(len);
            out.extend_from_slice(b"AEROSPAR");
            out.extend_from_slice(&tail);
            out
        })
    })
}

fn vhd_truncated_footer_strategy() -> impl Strategy<Value = Vec<u8>> {
    // VHD footers are 512 bytes. Generate truncated buffers that still include the cookie.
    (8usize..512).prop_flat_map(|len| {
        let tail_len = len - 8;
        prop::collection::vec(any::<u8>(), tail_len).prop_map(move |tail| {
            let mut out = Vec::with_capacity(len);
            out.extend_from_slice(b"conectix");
            out.extend_from_slice(&tail);
            out
        })
    })
}

fn qcow2_magic_invalid_size() -> Vec<u8> {
    // A QCOW2-looking buffer with a size field that fails validation.
    // Keep the image tiny so we don't exercise expensive parsing.
    let mut header = vec![0u8; 72];
    header[0..4].copy_from_slice(b"QFI\xfb");
    header[4..8].copy_from_slice(&2u32.to_be_bytes()); // version 2

    // l1_table_offset / refcount_table_offset must not overlap the header. Use aligned offsets.
    header[40..48].copy_from_slice(&512u64.to_be_bytes());
    header[48..56].copy_from_slice(&1024u64.to_be_bytes());

    // size is big-endian at offset 24..32 and must be non-zero + 512-byte aligned.
    header[24..32].copy_from_slice(&1u64.to_be_bytes());
    header
}

fn aerosparse_magic_invalid_sizes() -> Vec<u8> {
    // A minimally plausible AeroSparse header (magic + version), but invalid sizing fields.
    let mut header = vec![0u8; 64];
    header[0..8].copy_from_slice(b"AEROSPAR");
    header[8..12].copy_from_slice(&1u32.to_le_bytes()); // version
    header[12..16].copy_from_slice(&64u32.to_le_bytes()); // header_size
    header[32..40].copy_from_slice(&64u64.to_le_bytes()); // table_offset
    // Leave the rest zero (block_size_bytes=0, disk_size_bytes=0, etc) so `open_auto` reports a
    // structured error.
    header
}

fn vhd_magic_invalid_sizes() -> Vec<u8> {
    // Construct an image that looks like a VHD (valid-looking footer cookie/fields) but whose
    // overall file length is invalid (not a multiple of 512).
    //
    // This should be detected as VHD and then fail in `VhdDisk::open` with a structured error.
    let file_len = 1025usize; // intentionally misaligned
    let footer_offset = file_len - 512;
    let mut buf = vec![0u8; file_len];

    let footer = &mut buf[footer_offset..footer_offset + 512];
    footer[0..8].copy_from_slice(b"conectix");
    // file_format_version at 12..16
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    // data_offset at 16..24 (fixed disks use u64::MAX)
    footer[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    // current_size at 48..56 (must be non-zero + sector aligned)
    footer[48..56].copy_from_slice(&512u64.to_be_bytes());
    // disk_type at 60..64 (2=fixed)
    footer[60..64].copy_from_slice(&2u32.to_be_bytes());

    buf
}

fn untrusted_image_bytes_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        // General random bytes, bounded to keep runtime and memory predictable.
        10 => prop::collection::vec(any::<u8>(), 0..=MAX_IMAGE_BYTES),
        // Very small buffers are common "interesting" cases for header parsing.
        5 => prop::collection::vec(any::<u8>(), 0..=16),
        // Truncated headers/footers for known formats.
        3 => qcow2_truncated_header_strategy(),
        3 => aerosparse_truncated_header_strategy(),
        3 => vhd_truncated_footer_strategy(),
        // Correct magic but invalid sizing/shape.
        2 => Just(qcow2_magic_invalid_size()),
        2 => Just(aerosparse_magic_invalid_sizes()),
        2 => Just(vhd_magic_invalid_sizes()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        max_shrink_iters: 4096,
        .. ProptestConfig::default()
    })]

    #[test]
    fn open_auto_never_panics_on_untrusted_bytes(bytes in untrusted_image_bytes_strategy()) {
        let len = bytes.len();
        let head = bytes.iter().take(16).copied().collect::<Vec<u8>>();

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let backend = MemBackend::from_vec(bytes);
            let _ = DiskImage::open_auto(backend);
        }));

        prop_assert!(res.is_ok(), "DiskImage::open_auto panicked (len={len}, head={head:02x?})");
    }
}

