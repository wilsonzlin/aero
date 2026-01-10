pub fn acpi_checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

pub fn generate_checksum_byte(bytes: &[u8]) -> u8 {
    (0u8).wrapping_sub(acpi_checksum(bytes))
}
