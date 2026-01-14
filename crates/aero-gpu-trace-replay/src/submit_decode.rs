use crate::alloc_table_dump::{
    decode_alloc_table_entries_le, AllocTableDumpError, DecodedAllocEntry,
};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdDecodeError, AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuCmdStreamIter,
};
use std::collections::HashMap;

/// A reference from the command stream to a guest-backed allocation (by `backing_alloc_id`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackingAllocRef {
    /// Byte offset into the cmd stream (from the start of the stream).
    pub cmd_offset: usize,
    pub opcode: AerogpuCmdOpcode,
    /// Resource handle in the command payload (buffer/texture handle).
    pub resource_handle: u32,
    pub backing_alloc_id: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeSubmitError {
    #[error(transparent)]
    AllocTable(#[from] AllocTableDumpError),

    #[error("cmd stream decode error at offset {offset}: {err:?}")]
    CmdStreamDecode {
        offset: usize,
        err: AerogpuCmdDecodeError,
    },

    #[error(
        "alloc table contains duplicate alloc_id {alloc_id} mapping to different GPAs (0x{first_gpa:016x} vs 0x{second_gpa:016x})"
    )]
    ConflictingAllocIdGpa {
        alloc_id: u32,
        first_gpa: u64,
        second_gpa: u64,
    },

    #[error(
        "cmd stream references backing_alloc_id {alloc_id} at offset {cmd_offset} ({opcode:?}, handle {resource_handle}), but alloc table does not contain this alloc_id"
    )]
    MissingAllocId {
        alloc_id: u32,
        cmd_offset: usize,
        opcode: AerogpuCmdOpcode,
        resource_handle: u32,
    },

    #[error(
        "cmd stream packet at offset {cmd_offset} ({opcode:?}) is too small for expected fields (need >= {min_payload_bytes} payload bytes, found {found_payload_bytes})"
    )]
    PacketPayloadTooSmall {
        cmd_offset: usize,
        opcode: AerogpuCmdOpcode,
        min_payload_bytes: usize,
        found_payload_bytes: usize,
    },

    #[error("cmd stream offset overflow")]
    OffsetOverflow,
}

#[derive(Clone, Debug)]
pub struct DecodeSubmitReport {
    pub alloc_entries: Vec<DecodedAllocEntry>,
    /// Map for `alloc_id -> entry` lookup (validated for conflicting duplicates).
    pub alloc_map: HashMap<u32, DecodedAllocEntry>,
    pub backing_alloc_refs: Vec<BackingAllocRef>,
}

/// Decode a cmd stream + alloc table pair and cross-check:
/// - any non-zero `backing_alloc_id` referenced by the cmd stream is present in the alloc table
/// - the alloc table does not contain conflicting duplicates (`alloc_id` mapping to different `gpa`)
pub fn decode_submit(
    cmd_stream_bytes: &[u8],
    alloc_table_bytes: &[u8],
) -> Result<DecodeSubmitReport, DecodeSubmitError> {
    let alloc_entries = decode_alloc_table_entries_le(alloc_table_bytes)?;

    let mut alloc_map: HashMap<u32, DecodedAllocEntry> = HashMap::new();
    for entry in &alloc_entries {
        if let Some(existing) = alloc_map.get(&entry.alloc_id) {
            if existing.gpa != entry.gpa {
                return Err(DecodeSubmitError::ConflictingAllocIdGpa {
                    alloc_id: entry.alloc_id,
                    first_gpa: existing.gpa,
                    second_gpa: entry.gpa,
                });
            }
            continue;
        }
        alloc_map.insert(entry.alloc_id, *entry);
    }

    let backing_alloc_refs = collect_backing_alloc_refs(cmd_stream_bytes)?;

    for r in &backing_alloc_refs {
        if r.backing_alloc_id == 0 {
            continue;
        }
        if !alloc_map.contains_key(&r.backing_alloc_id) {
            return Err(DecodeSubmitError::MissingAllocId {
                alloc_id: r.backing_alloc_id,
                cmd_offset: r.cmd_offset,
                opcode: r.opcode,
                resource_handle: r.resource_handle,
            });
        }
    }

    Ok(DecodeSubmitReport {
        alloc_entries,
        alloc_map,
        backing_alloc_refs,
    })
}

fn collect_backing_alloc_refs(
    cmd_stream_bytes: &[u8],
) -> Result<Vec<BackingAllocRef>, DecodeSubmitError> {
    let iter = AerogpuCmdStreamIter::new(cmd_stream_bytes)
        .map_err(|err| DecodeSubmitError::CmdStreamDecode { offset: 0, err })?;

    let mut out = Vec::new();
    let mut offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    for packet in iter {
        let packet = packet.map_err(|err| DecodeSubmitError::CmdStreamDecode { offset, err })?;

        match packet.opcode {
            Some(AerogpuCmdOpcode::CreateBuffer) => {
                // Payload layout (prefix): u32 buffer_handle; u32 usage_flags; u64 size_bytes;
                // u32 backing_alloc_id; u32 backing_offset_bytes; ...
                const MIN_PAYLOAD: usize = 24;
                if packet.payload.len() < MIN_PAYLOAD {
                    return Err(DecodeSubmitError::PacketPayloadTooSmall {
                        cmd_offset: offset,
                        opcode: AerogpuCmdOpcode::CreateBuffer,
                        min_payload_bytes: MIN_PAYLOAD,
                        found_payload_bytes: packet.payload.len(),
                    });
                }
                let resource_handle = u32::from_le_bytes(packet.payload[0..4].try_into().unwrap());
                let backing_alloc_id =
                    u32::from_le_bytes(packet.payload[16..20].try_into().unwrap());
                out.push(BackingAllocRef {
                    cmd_offset: offset,
                    opcode: AerogpuCmdOpcode::CreateBuffer,
                    resource_handle,
                    backing_alloc_id,
                });
            }
            Some(AerogpuCmdOpcode::CreateTexture2d) => {
                // Payload layout (prefix): u32 texture_handle; u32 usage_flags; u32 format;
                // u32 width; u32 height; u32 mip_levels; u32 array_layers; u32 row_pitch_bytes;
                // u32 backing_alloc_id; u32 backing_offset_bytes; ...
                const MIN_PAYLOAD: usize = 40;
                if packet.payload.len() < MIN_PAYLOAD {
                    return Err(DecodeSubmitError::PacketPayloadTooSmall {
                        cmd_offset: offset,
                        opcode: AerogpuCmdOpcode::CreateTexture2d,
                        min_payload_bytes: MIN_PAYLOAD,
                        found_payload_bytes: packet.payload.len(),
                    });
                }
                let resource_handle = u32::from_le_bytes(packet.payload[0..4].try_into().unwrap());
                let backing_alloc_id =
                    u32::from_le_bytes(packet.payload[32..36].try_into().unwrap());
                out.push(BackingAllocRef {
                    cmd_offset: offset,
                    opcode: AerogpuCmdOpcode::CreateTexture2d,
                    resource_handle,
                    backing_alloc_id,
                });
            }
            _ => {}
        }

        let cmd_size = packet.hdr.size_bytes as usize;
        offset = offset
            .checked_add(cmd_size)
            .ok_or(DecodeSubmitError::OffsetOverflow)?;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
    use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AEROGPU_ALLOC_TABLE_MAGIC};
    use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

    fn push_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn build_alloc_table(entries: &[(u32, u64, u64, u32)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, AEROGPU_ALLOC_TABLE_MAGIC);
        push_u32(&mut bytes, AEROGPU_ABI_VERSION_U32);
        push_u32(&mut bytes, 0); // size_bytes patched later
        push_u32(&mut bytes, entries.len() as u32);
        push_u32(&mut bytes, AerogpuAllocEntry::SIZE_BYTES as u32);
        push_u32(&mut bytes, 0); // reserved0

        for &(alloc_id, gpa, size_bytes, flags) in entries {
            push_u32(&mut bytes, alloc_id);
            push_u32(&mut bytes, flags);
            push_u64(&mut bytes, gpa);
            push_u64(&mut bytes, size_bytes);
            push_u64(&mut bytes, 0); // reserved0
        }

        let size_bytes = bytes.len() as u32;
        bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());
        bytes
    }

    #[test]
    fn decode_submit_ok_when_alloc_present() {
        let mut w = AerogpuCmdWriter::new();
        w.create_buffer(1, 0, 4, 123, 0);
        let cmd = w.finish();

        let alloc = build_alloc_table(&[(123, 0x1000, 4, 0)]);
        let report = decode_submit(&cmd, &alloc).unwrap();
        assert_eq!(report.backing_alloc_refs.len(), 1);
        assert_eq!(report.backing_alloc_refs[0].backing_alloc_id, 123);
    }

    #[test]
    fn decode_submit_errors_on_missing_alloc() {
        let mut w = AerogpuCmdWriter::new();
        w.create_buffer(1, 0, 4, 2, 0);
        let cmd = w.finish();

        let alloc = build_alloc_table(&[(1, 0x1000, 4, 0)]);
        let err = decode_submit(&cmd, &alloc).unwrap_err();
        assert!(matches!(
            err,
            DecodeSubmitError::MissingAllocId { alloc_id: 2, .. }
        ));
        assert!(err.to_string().contains("does not contain"));
    }

    #[test]
    fn decode_submit_rejects_conflicting_duplicate_alloc_ids() {
        let mut w = AerogpuCmdWriter::new();
        w.create_buffer(1, 0, 4, 1, 0);
        let cmd = w.finish();

        let alloc = build_alloc_table(&[(1, 0x1000, 4, 0), (1, 0x2000, 4, 0)]);
        let err = decode_submit(&cmd, &alloc).unwrap_err();
        assert!(matches!(
            err,
            DecodeSubmitError::ConflictingAllocIdGpa { alloc_id: 1, .. }
        ));
    }

    #[test]
    fn decode_submit_allows_duplicate_alloc_ids_with_same_gpa() {
        let mut w = AerogpuCmdWriter::new();
        w.create_buffer(1, 0, 4, 1, 0);
        let cmd = w.finish();

        let alloc = build_alloc_table(&[(1, 0x1000, 4, 0), (1, 0x1000, 4, 0)]);
        decode_submit(&cmd, &alloc).unwrap();
    }
}
