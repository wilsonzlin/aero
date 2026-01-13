#![allow(dead_code)]

use std::io::Cursor;

use aero_snapshot::{MmuState, SectionId};

pub const MMUS_SECTION_ID: SectionId = SectionId(8);

pub fn encode_mmus_section(version: u16, entries: &[(u32, MmuState)]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());

    for (apic_id, mmu) in entries {
        let mut entry = Vec::new();
        entry.extend_from_slice(&apic_id.to_le_bytes());
        if version == 1 {
            mmu.encode_v1(&mut entry).unwrap();
        } else {
            mmu.encode_v2(&mut entry).unwrap();
        }
        payload.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        payload.extend_from_slice(&entry);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&MMUS_SECTION_ID.0.to_le_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Replace any existing `MMU` or `MMUS` section with a synthetic `MMUS` section.
///
/// This is used by xtask CLI tests to exercise the `MMUS` code path without depending on snapshot
/// writers to have switched formats yet.
pub fn with_mmus_section(snapshot: &[u8], version: u16, entries: &[(u32, MmuState)]) -> Vec<u8> {
    const FILE_HEADER_LEN: usize = 16;

    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(snapshot)).unwrap();
    assert!(
        snapshot.len() >= FILE_HEADER_LEN,
        "snapshot too short for header"
    );

    let mut out = Vec::new();
    out.extend_from_slice(&snapshot[..FILE_HEADER_LEN]);

    let mut inserted = false;
    for section in &index.sections {
        let header_start = section
            .offset
            .checked_sub(16)
            .expect("section offset underflow") as usize;
        let payload_len: usize = section.len.try_into().expect("section len fits usize");
        let end = header_start + 16 + payload_len;
        assert!(end <= snapshot.len(), "section range out of bounds");

        if section.id == SectionId::MMU || section.id == MMUS_SECTION_ID {
            if !inserted {
                out.extend_from_slice(&encode_mmus_section(version, entries));
                inserted = true;
            }
            continue;
        }

        out.extend_from_slice(&snapshot[header_start..end]);
    }

    if !inserted {
        out.extend_from_slice(&encode_mmus_section(version, entries));
    }

    out
}
