#![no_main]

use aero_gpu::aerogpu_executor::AllocTable;
use aero_gpu::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_ring as ring;
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations inside command-stream / alloc-table
/// decoding paths.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Non-zero guest physical address to place the candidate alloc-table bytes at.
const ALLOC_TABLE_GPA: u64 = 0x1000;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Treat the entire input as a candidate command stream. All errors are acceptable.
    let _ = aero_gpu::parse_cmd_stream(data);

    // If the input is large enough, also try alloc-table decoding via both the canonical protocol
    // decoder and the executor's GuestMemory-backed decoder.
    if data.len() < ring::AerogpuAllocTableHeader::SIZE_BYTES {
        return;
    }

    // Header decode/validation (canonical helpers).
    let declared_size_bytes = ring::AerogpuAllocTableHeader::decode_from_le_bytes(data)
        .ok()
        .and_then(|header| header.validate_prefix().ok().map(|_| header.size_bytes))
        .unwrap_or(0);

    // Whole-table decode (canonical helpers).
    let _ = ring::decode_alloc_table_le(data);

    // Ring + submission layouts are adjacent to alloc-table parsing in the protocol crate; decode
    // them too to harden their bounds checks.
    if let Ok(hdr) = ring::AerogpuRingHeader::decode_from_le_bytes(data) {
        let _ = hdr.validate_prefix();
    }
    if let Ok(desc) = ring::AerogpuSubmitDesc::decode_from_le_bytes(data) {
        let _ = desc.validate_prefix();
    }
    if let Ok(page) = ring::AerogpuFencePage::decode_from_le_bytes(data) {
        let _ = page.validate_prefix();
    }

    // Executor decode (GuestMemory-backed). Use the header's declared size when it is both
    // well-formed and within our input cap; otherwise, fall back to the provided buffer length.
    let table_size_bytes = match u32::try_from(data.len()) {
        Ok(len) => match declared_size_bytes {
            0 => len,
            declared
                if declared as usize <= data.len() && declared as usize <= MAX_INPUT_SIZE_BYTES =>
            {
                declared
            }
            _ => len,
        },
        Err(_) => return,
    };

    let guest_mem_size = (ALLOC_TABLE_GPA as usize).saturating_add(table_size_bytes as usize);
    if guest_mem_size > (ALLOC_TABLE_GPA as usize).saturating_add(MAX_INPUT_SIZE_BYTES) {
        return;
    }

    let mut guest = VecGuestMemory::new(guest_mem_size);
    let _ = guest.write(ALLOC_TABLE_GPA, &data[..table_size_bytes as usize]);
    let _ = AllocTable::decode_from_guest_memory(&mut guest, ALLOC_TABLE_GPA, table_size_bytes);
});
