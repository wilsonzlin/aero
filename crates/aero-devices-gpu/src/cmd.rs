use core::mem::offset_of;

use aero_protocol::aerogpu::aerogpu_cmd as protocol_cmd;

pub const CMD_STREAM_MAGIC_OFFSET: u64 =
    offset_of!(protocol_cmd::AerogpuCmdStreamHeader, magic) as u64;
pub const CMD_STREAM_ABI_VERSION_OFFSET: u64 =
    offset_of!(protocol_cmd::AerogpuCmdStreamHeader, abi_version) as u64;
pub const CMD_STREAM_SIZE_BYTES_OFFSET: u64 =
    offset_of!(protocol_cmd::AerogpuCmdStreamHeader, size_bytes) as u64;
pub const CMD_STREAM_FLAGS_OFFSET: u64 =
    offset_of!(protocol_cmd::AerogpuCmdStreamHeader, flags) as u64;
pub const CMD_STREAM_RESERVED0_OFFSET: u64 =
    offset_of!(protocol_cmd::AerogpuCmdStreamHeader, reserved0) as u64;
pub const CMD_STREAM_RESERVED1_OFFSET: u64 =
    offset_of!(protocol_cmd::AerogpuCmdStreamHeader, reserved1) as u64;

pub const CMD_STREAM_HEADER_SIZE_BYTES: u32 =
    protocol_cmd::AerogpuCmdStreamHeader::SIZE_BYTES as u32;

pub const CMD_HDR_OPCODE_OFFSET: u64 = offset_of!(protocol_cmd::AerogpuCmdHdr, opcode) as u64;
pub const CMD_HDR_SIZE_BYTES_OFFSET: u64 =
    offset_of!(protocol_cmd::AerogpuCmdHdr, size_bytes) as u64;
pub const CMD_HDR_SIZE_BYTES: u32 = protocol_cmd::AerogpuCmdHdr::SIZE_BYTES as u32;

pub const CMD_PRESENT_SCANOUT_ID_OFFSET: u64 =
    offset_of!(protocol_cmd::AerogpuCmdPresent, scanout_id) as u64;
pub const CMD_PRESENT_FLAGS_OFFSET: u64 = offset_of!(protocol_cmd::AerogpuCmdPresent, flags) as u64;
pub const CMD_PRESENT_SIZE_BYTES: u32 = protocol_cmd::AerogpuCmdPresent::SIZE_BYTES as u32;
