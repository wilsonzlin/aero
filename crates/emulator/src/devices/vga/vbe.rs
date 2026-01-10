use super::edid;

pub fn read_edid(block: u16) -> Option<[u8; edid::EDID_BLOCK_SIZE]> {
    edid::read_edid(block)
}
