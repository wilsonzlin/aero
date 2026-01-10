use super::builder::TableBuilder;
use super::SmbiosConfig;

pub fn push_all(config: &SmbiosConfig, builder: &mut TableBuilder) {
    let handles = Handles::new(config.cpu_count);

    push_type0_bios_information(builder, handles.type0);
    push_type1_system_information(config, builder, handles.type1);
    push_type2_baseboard_information(builder, handles.type2, handles.type3);
    push_type3_chassis_information(builder, handles.type3);

    for cpu_index in 0..config.cpu_count.max(1) {
        let handle = handles.type4_base.wrapping_add(cpu_index as u16);
        push_type4_processor_information(builder, handle, cpu_index);
    }

    push_type16_physical_memory_array(config, builder, handles.type16);
    push_type17_memory_device(config, builder, handles.type17, handles.type16);
    push_type19_memory_array_mapped_address(config, builder, handles.type19, handles.type16);

    push_type32_system_boot_information(builder, handles.type32);
    push_type127_end_of_table(builder, handles.type127);
}

struct Handles {
    type0: u16,
    type1: u16,
    type2: u16,
    type3: u16,
    type4_base: u16,
    type16: u16,
    type17: u16,
    type19: u16,
    type32: u16,
    type127: u16,
}

impl Handles {
    fn new(cpu_count: u8) -> Self {
        // Keep handles stable/deterministic (useful for diffing and tests).
        // We also leave gaps so future tables (e.g. Type 7 cache) can be added
        // without having to re-number everything.
        let cpu_count = cpu_count.max(1) as u16;
        let type4_base = 0x0400;
        let next = type4_base + cpu_count;

        Self {
            type0: 0x0000,
            type1: 0x0100,
            type2: 0x0200,
            type3: 0x0300,
            type4_base,
            type16: next + 0x100, // keep some spacing after CPUs
            type17: next + 0x110,
            type19: next + 0x120,
            type32: next + 0x200,
            type127: next + 0x2FF,
        }
    }
}

fn push_type0_bios_information(builder: &mut TableBuilder, handle: u16) {
    let strings = ["Aero", "Aero BIOS", "01/01/2026"];

    let mut formatted = Vec::with_capacity(0x18 - 4);
    formatted.push(1); // Vendor
    formatted.push(2); // BIOS Version
    formatted.extend_from_slice(&0xF000u16.to_le_bytes()); // BIOS starting address segment
    formatted.push(3); // BIOS Release Date
    formatted.push(0); // BIOS ROM Size: (n * 64KB) - 1 => 0 = 64KB
    formatted.extend_from_slice(&0u64.to_le_bytes()); // BIOS Characteristics
    formatted.extend_from_slice(&[0, 0]); // BIOS Characteristics Extension Bytes
    formatted.push(1); // System BIOS Major Release
    formatted.push(0); // System BIOS Minor Release
    formatted.push(0xFF); // Embedded Controller Firmware Major Release (unknown/not present)
    formatted.push(0xFF); // Embedded Controller Firmware Minor Release

    builder.push_structure(0, handle, &formatted, &strings);
}

fn push_type1_system_information(config: &SmbiosConfig, builder: &mut TableBuilder, handle: u16) {
    let strings = ["Aero", "Aero VM", "1.0", "00000000"];

    let uuid = deterministic_uuid(config);

    let mut formatted = Vec::with_capacity(0x1B - 4);
    formatted.push(1); // Manufacturer
    formatted.push(2); // Product Name
    formatted.push(3); // Version
    formatted.push(4); // Serial Number
    formatted.extend_from_slice(&uuid);
    formatted.push(0x06); // Wake-up Type: Power Switch
    formatted.push(0); // SKU Number
    formatted.push(0); // Family

    builder.push_structure(1, handle, &formatted, &strings);
}

fn push_type2_baseboard_information(builder: &mut TableBuilder, handle: u16, chassis_handle: u16) {
    let strings = [
        "Aero",           // Manufacturer
        "Aero Baseboard", // Product
        "1.0",            // Version
        "00000000",       // Serial
        "Mainboard",      // Location in chassis
    ];

    let mut formatted = Vec::with_capacity(0x0F - 4);
    formatted.push(1); // Manufacturer
    formatted.push(2); // Product
    formatted.push(3); // Version
    formatted.push(4); // Serial Number
    formatted.push(0); // Asset Tag Number
    formatted.push(0); // Feature Flags
    formatted.push(5); // Location in Chassis
    formatted.extend_from_slice(&chassis_handle.to_le_bytes());
    formatted.push(0x0A); // Board Type: Motherboard
    formatted.push(0); // Number of Contained Object Handles

    builder.push_structure(2, handle, &formatted, &strings);
}

fn push_type3_chassis_information(builder: &mut TableBuilder, handle: u16) {
    let strings = ["Aero", "1.0", "00000000"];

    let mut formatted = Vec::with_capacity(0x14 - 4);
    formatted.push(1); // Manufacturer
    formatted.push(0x03); // Type: Desktop
    formatted.push(2); // Version
    formatted.push(3); // Serial Number
    formatted.push(0); // Asset Tag Number
    formatted.push(0x03); // Boot-up State: Safe
    formatted.push(0x03); // Power Supply State: Safe
    formatted.push(0x03); // Thermal State: Safe
    formatted.push(0x02); // Security Status: Unknown
    formatted.extend_from_slice(&0u32.to_le_bytes()); // OEM-defined
    formatted.push(0); // Height
    formatted.push(1); // Number of Power Cords
    formatted.push(0); // Contained Element Count
    formatted.push(0); // Contained Element Record Length

    builder.push_structure(3, handle, &formatted, &strings);
}

fn push_type4_processor_information(builder: &mut TableBuilder, handle: u16, cpu_index: u8) {
    let socket = format!("CPU{}", cpu_index);
    let strings = [socket.as_str(), "Aero", "Aero CPU"];

    let mut formatted = Vec::with_capacity(0x1A - 4);
    formatted.push(1); // Socket Designation
    formatted.push(3); // Processor Type: Central Processor
    formatted.push(2); // Processor Family: Unknown
    formatted.push(2); // Manufacturer
    formatted.extend_from_slice(&0u64.to_le_bytes()); // Processor ID
    formatted.push(3); // Processor Version
    formatted.push(0); // Voltage (unknown)
    formatted.extend_from_slice(&0u16.to_le_bytes()); // External Clock
    formatted.extend_from_slice(&0u16.to_le_bytes()); // Max Speed
    formatted.extend_from_slice(&0u16.to_le_bytes()); // Current Speed
    formatted.push(0x41); // Status: CPU enabled + populated
    formatted.push(0x02); // Processor Upgrade: Unknown

    builder.push_structure(4, handle, &formatted, &strings);
}

fn push_type16_physical_memory_array(
    config: &SmbiosConfig,
    builder: &mut TableBuilder,
    handle: u16,
) {
    let max_capacity_kb = (config.ram_bytes / 1024).min(u64::from(u32::MAX)) as u32;

    let mut formatted = Vec::with_capacity(0x0F - 4);
    formatted.push(0x03); // Location: System Board
    formatted.push(0x03); // Use: System Memory
    formatted.push(0x03); // Memory Error Correction: None
    formatted.extend_from_slice(&max_capacity_kb.to_le_bytes());
    formatted.extend_from_slice(&0xFFFFu16.to_le_bytes()); // Memory Error Information Handle
    formatted.extend_from_slice(&1u16.to_le_bytes()); // Number of Memory Devices

    builder.push_structure(16, handle, &formatted, &[]);
}

fn push_type17_memory_device(
    config: &SmbiosConfig,
    builder: &mut TableBuilder,
    handle: u16,
    array_handle: u16,
) {
    let size_mb = (config.ram_bytes / (1024 * 1024)).min(u64::from(u16::MAX)) as u16;

    let strings = ["DIMM0", "BANK0"];

    let mut formatted = Vec::with_capacity(0x15 - 4);
    formatted.extend_from_slice(&array_handle.to_le_bytes()); // Physical Memory Array Handle
    formatted.extend_from_slice(&0xFFFFu16.to_le_bytes()); // Memory Error Information Handle
    formatted.extend_from_slice(&64u16.to_le_bytes()); // Total Width
    formatted.extend_from_slice(&64u16.to_le_bytes()); // Data Width
    formatted.extend_from_slice(&size_mb.to_le_bytes()); // Size (MB)
    formatted.push(0x09); // Form Factor: DIMM
    formatted.push(0); // Device Set
    formatted.push(1); // Device Locator
    formatted.push(2); // Bank Locator
    formatted.push(0x18); // Memory Type: DDR3
    formatted.extend_from_slice(&0u16.to_le_bytes()); // Type Detail

    builder.push_structure(17, handle, &formatted, &strings);
}

fn push_type19_memory_array_mapped_address(
    config: &SmbiosConfig,
    builder: &mut TableBuilder,
    handle: u16,
    array_handle: u16,
) {
    let size_kb = (config.ram_bytes / 1024).min(u64::from(u32::MAX));
    let end_kb_inclusive = size_kb.saturating_sub(1) as u32;

    let mut formatted = Vec::with_capacity(0x0F - 4);
    formatted.extend_from_slice(&0u32.to_le_bytes()); // Starting Address (KB)
    formatted.extend_from_slice(&end_kb_inclusive.to_le_bytes()); // Ending Address (KB)
    formatted.extend_from_slice(&array_handle.to_le_bytes()); // Memory Array Handle
    formatted.push(1); // Partition Width

    builder.push_structure(19, handle, &formatted, &[]);
}

fn push_type32_system_boot_information(builder: &mut TableBuilder, handle: u16) {
    let mut formatted = Vec::with_capacity(0x0B - 4);
    formatted.extend_from_slice(&[0u8; 6]); // Reserved
    formatted.push(0); // Boot Status: No errors

    builder.push_structure(32, handle, &formatted, &[]);
}

fn push_type127_end_of_table(builder: &mut TableBuilder, handle: u16) {
    builder.push_structure(127, handle, &[], &[]);
}

fn deterministic_uuid(config: &SmbiosConfig) -> [u8; 16] {
    let mut data = Vec::new();
    data.extend_from_slice(b"AeroSMBIOS");
    data.extend_from_slice(&config.ram_bytes.to_le_bytes());
    data.push(config.cpu_count);
    data.extend_from_slice(&config.uuid_seed.to_le_bytes());

    let mut uuid = fnv1a_128(&data).to_be_bytes();
    // RFC 4122 variant + version bits.
    uuid[6] = (uuid[6] & 0x0F) | 0x40;
    uuid[8] = (uuid[8] & 0x3F) | 0x80;

    // SMBIOS stores the first three UUID fields little-endian.
    uuid[0..4].reverse();
    uuid[4..6].reverse();
    uuid[6..8].reverse();

    uuid
}

fn fnv1a_128(bytes: &[u8]) -> u128 {
    const OFFSET_BASIS: u128 = 0x6c62272e07bb014262b821756295c58d;
    const FNV_PRIME: u128 = 0x0000000001000000000000000000013B;

    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u128;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}
