use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdDecodeError, AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuCmdStreamIter,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Debug, thiserror::Error)]
pub enum CmdStreamDecodeError {
    #[error("cmd stream decode error: {0:?}")]
    Decode(AerogpuCmdDecodeError),

    #[error("json encode error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<AerogpuCmdDecodeError> for CmdStreamDecodeError {
    fn from(value: AerogpuCmdDecodeError) -> Self {
        Self::Decode(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdStreamListingFormat {
    Text,
    Json,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CmdStreamPacketListing {
    /// Byte offset of the packet from the start of the cmd stream (i.e. after the stream header).
    pub offset: usize,
    pub opcode_u32: u32,
    pub opcode: Option<AerogpuCmdOpcode>,
    pub size_bytes: u32,
    /// Best-effort decoded fields for known opcodes.
    pub decoded: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CmdStreamListingRecord {
    Packet(CmdStreamPacketListing),
    Error {
        offset: usize,
        err: AerogpuCmdDecodeError,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CmdStreamHeaderListing {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub flags: u32,
    pub reserved0: u32,
    pub reserved1: u32,
}

impl From<AerogpuCmdStreamHeader> for CmdStreamHeaderListing {
    fn from(value: AerogpuCmdStreamHeader) -> Self {
        Self {
            magic: value.magic,
            abi_version: value.abi_version,
            size_bytes: value.size_bytes,
            flags: value.flags,
            reserved0: value.reserved0,
            reserved1: value.reserved1,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CmdStreamDecodeReport {
    pub header: CmdStreamHeaderListing,
    pub input_len_bytes: usize,
    pub records: Vec<CmdStreamListingRecord>,
}

impl CmdStreamDecodeReport {
    pub fn to_text(&self) -> String {
        let mut out = String::new();

        let hdr = &self.header;
        let magic_fourcc = fourcc_le(hdr.magic);
        let trailing = self.input_len_bytes.saturating_sub(hdr.size_bytes as usize);
        let _ = writeln!(
            out,
            "header magic=0x{magic:08x} ({magic_fourcc}) abi_version=0x{abi:08x} size_bytes={size} flags=0x{flags:08x} input_len_bytes={input_len} trailing_bytes={trailing}",
            magic = hdr.magic,
            abi = hdr.abi_version,
            size = hdr.size_bytes,
            flags = hdr.flags,
            input_len = self.input_len_bytes,
            trailing = trailing,
        );

        for rec in &self.records {
            match rec {
                CmdStreamListingRecord::Packet(pkt) => {
                    let opcode_str = match pkt.opcode {
                        Some(op) => format!("{op:?}"),
                        None => format!("Unknown(0x{:08x})", pkt.opcode_u32),
                    };
                    let _ = write!(
                        out,
                        "0x{offset:08x} {opcode_str} size_bytes={size_bytes}",
                        offset = pkt.offset,
                        opcode_str = opcode_str,
                        size_bytes = pkt.size_bytes
                    );

                    if !pkt.decoded.is_empty() {
                        for (k, v) in &pkt.decoded {
                            let v_str = match v {
                                Value::String(s) => s.clone(),
                                _ => v.to_string(),
                            };
                            let _ = write!(out, " {k}={v_str}");
                        }
                    }
                    out.push('\n');
                }
                CmdStreamListingRecord::Error { offset, err } => {
                    let _ = writeln!(
                        out,
                        "0x{offset:08x} ERROR {kind} {details}",
                        offset = offset,
                        kind = cmd_decode_error_kind(err),
                        details = cmd_decode_error_details(err)
                    );
                }
            }
        }

        out
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.to_json_value())
    }

    pub fn to_json_value(&self) -> Value {
        let hdr = &self.header;
        let trailing = self.input_len_bytes.saturating_sub(hdr.size_bytes as usize);

        let packets: Vec<Value> = self
            .records
            .iter()
            .map(|rec| match rec {
                CmdStreamListingRecord::Packet(pkt) => json!({
                    "type": "packet",
                    "offset": pkt.offset,
                    "opcode_u32": pkt.opcode_u32,
                    "opcode": pkt.opcode.map(|o| format!("{o:?}")),
                    "size_bytes": pkt.size_bytes,
                    "decoded": pkt.decoded,
                }),
                CmdStreamListingRecord::Error { offset, err } => json!({
                    "type": "error",
                    "offset": offset,
                    "error": cmd_decode_error_to_json(err),
                }),
            })
            .collect();

        json!({
            "schema_version": 1,
            "header": {
                "magic_u32": hdr.magic,
                "magic_fourcc": fourcc_le(hdr.magic),
                "abi_version_u32": hdr.abi_version,
                "size_bytes": hdr.size_bytes,
                "flags": hdr.flags,
                "reserved0": hdr.reserved0,
                "reserved1": hdr.reserved1,
            },
            "input_len_bytes": self.input_len_bytes,
            "trailing_bytes": trailing,
            "records": packets,
        })
    }
}

/// Decode an AeroGPU cmd stream into a report suitable for text/JSON rendering.
///
/// This is best-effort: packet decode errors are captured as `CmdStreamListingRecord::Error`
/// and the report will contain all packets successfully decoded up to the first error.
pub fn decode_cmd_stream(bytes: &[u8]) -> Result<CmdStreamDecodeReport, AerogpuCmdDecodeError> {
    let iter = AerogpuCmdStreamIter::new(bytes)?;
    let header: CmdStreamHeaderListing = (*iter.header()).into();

    let mut records = Vec::new();
    let mut offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    for pkt in iter {
        match pkt {
            Ok(pkt) => {
                let size_bytes = pkt.hdr.size_bytes;
                let decoded = decode_known_fields(&pkt);
                records.push(CmdStreamListingRecord::Packet(CmdStreamPacketListing {
                    offset,
                    opcode_u32: pkt.hdr.opcode,
                    opcode: pkt.opcode,
                    size_bytes,
                    decoded,
                }));

                offset = match offset.checked_add(size_bytes as usize) {
                    Some(v) => v,
                    None => {
                        records.push(CmdStreamListingRecord::Error {
                            offset,
                            err: AerogpuCmdDecodeError::CountOverflow,
                        });
                        break;
                    }
                };
            }
            Err(err) => {
                records.push(CmdStreamListingRecord::Error { offset, err });
                break;
            }
        }
    }

    Ok(CmdStreamDecodeReport {
        header,
        input_len_bytes: bytes.len(),
        records,
    })
}

/// Convenience helper for CLI/tools/tests that just want a rendered listing.
pub fn render_cmd_stream_listing(
    bytes: &[u8],
    format: CmdStreamListingFormat,
) -> Result<String, CmdStreamDecodeError> {
    let report = decode_cmd_stream(bytes)?;
    Ok(match format {
        CmdStreamListingFormat::Text => report.to_text(),
        CmdStreamListingFormat::Json => report.to_json_pretty()?,
    })
}

fn fourcc_le(v: u32) -> String {
    let bytes = v.to_le_bytes();
    let mut s = String::with_capacity(4);
    for &b in &bytes {
        let c = if b.is_ascii_graphic() || b == b' ' {
            b as char
        } else {
            '.'
        };
        s.push(c);
    }
    s
}

fn decode_known_fields(
    pkt: &aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdPacket<'_>,
) -> BTreeMap<String, Value> {
    let mut out: BTreeMap<String, Value> = BTreeMap::new();

    let Some(op) = pkt.opcode else {
        return out;
    };

    match op {
        AerogpuCmdOpcode::CreateBuffer => {
            // struct aerogpu_cmd_create_buffer (payload excludes hdr):
            // u32 buffer_handle; u32 usage_flags; u64 size_bytes; u32 backing_alloc_id; u32 backing_offset_bytes; ...
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("buffer_handle".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("usage_flags".into(), json!(v));
            }
            if let Some(v) = read_u64_le(pkt.payload, 8) {
                out.insert("size_bytes".into(), json!(v.to_string()));
            }
            if let Some(v) = read_u32_le(pkt.payload, 16) {
                out.insert("backing_alloc_id".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 20) {
                out.insert("backing_offset_bytes".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::CreateTexture2d => {
            // u32 texture_handle; u32 usage_flags; u32 format; u32 width; u32 height; ...
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("texture_handle".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("usage_flags".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 8) {
                out.insert("format".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 12) {
                out.insert("width".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 16) {
                out.insert("height".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 20) {
                out.insert("mip_levels".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 24) {
                out.insert("array_layers".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 28) {
                out.insert("row_pitch_bytes".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 32) {
                out.insert("backing_alloc_id".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 36) {
                out.insert("backing_offset_bytes".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::DestroyResource => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("resource_handle".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::ResourceDirtyRange => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("resource_handle".into(), json!(v));
            }
            if let Some(v) = read_u64_le(pkt.payload, 8) {
                out.insert("offset_bytes".into(), json!(v.to_string()));
            }
            if let Some(v) = read_u64_le(pkt.payload, 16) {
                out.insert("size_bytes".into(), json!(v.to_string()));
            }
        }
        AerogpuCmdOpcode::UploadResource => match pkt.decode_upload_resource_payload_le() {
            Ok((cmd, data)) => {
                let resource_handle = cmd.resource_handle;
                let offset_bytes = cmd.offset_bytes;
                let size_bytes = cmd.size_bytes;
                out.insert("resource_handle".into(), json!(resource_handle));
                out.insert("offset_bytes".into(), json!(offset_bytes.to_string()));
                out.insert("size_bytes".into(), json!(size_bytes.to_string()));
                out.insert("data_len".into(), json!(data.len()));
            }
            Err(err) => {
                out.insert("decode_error".into(), json!(format!("{:?}", err)));
            }
        },
        AerogpuCmdOpcode::CopyBuffer => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("dst_buffer".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("src_buffer".into(), json!(v));
            }
            if let Some(v) = read_u64_le(pkt.payload, 8) {
                out.insert("dst_offset_bytes".into(), json!(v.to_string()));
            }
            if let Some(v) = read_u64_le(pkt.payload, 16) {
                out.insert("src_offset_bytes".into(), json!(v.to_string()));
            }
            if let Some(v) = read_u64_le(pkt.payload, 24) {
                out.insert("size_bytes".into(), json!(v.to_string()));
            }
            if let Some(v) = read_u32_le(pkt.payload, 32) {
                out.insert("flags".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::CreateShaderDxbc => match pkt.decode_create_shader_dxbc_payload_le() {
            Ok((cmd, dxbc)) => {
                let shader_handle = cmd.shader_handle;
                let stage = cmd.stage;
                let dxbc_size_bytes = cmd.dxbc_size_bytes;
                out.insert("shader_handle".into(), json!(shader_handle));
                out.insert("stage".into(), json!(stage));
                out.insert("dxbc_size_bytes".into(), json!(dxbc_size_bytes));
                out.insert("dxbc_len".into(), json!(dxbc.len()));
            }
            Err(err) => {
                out.insert("decode_error".into(), json!(format!("{:?}", err)));
            }
        },
        AerogpuCmdOpcode::CreateInputLayout => match pkt.decode_create_input_layout_payload_le() {
            Ok((cmd, blob)) => {
                let input_layout_handle = cmd.input_layout_handle;
                let blob_size_bytes = cmd.blob_size_bytes;
                out.insert("input_layout_handle".into(), json!(input_layout_handle));
                out.insert("blob_size_bytes".into(), json!(blob_size_bytes));
                out.insert("blob_len".into(), json!(blob.len()));
            }
            Err(err) => {
                out.insert("decode_error".into(), json!(format!("{:?}", err)));
            }
        },
        AerogpuCmdOpcode::SetRenderTargets => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("color_count".into(), json!(v));
                // Attempt to decode the first few handles (always fixed size).
                let mut colors = Vec::new();
                for i in 0..(v.min(8)) {
                    if let Some(h) = read_u32_le(pkt.payload, 8 + i as usize * 4) {
                        colors.push(Value::from(h));
                    }
                }
                if !colors.is_empty() {
                    out.insert("colors".into(), Value::Array(colors));
                }
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("depth_stencil".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::SetViewport => {
            if let Some(v) = read_f32_le(pkt.payload, 0) {
                out.insert("x".into(), json!(v));
            }
            if let Some(v) = read_f32_le(pkt.payload, 4) {
                out.insert("y".into(), json!(v));
            }
            if let Some(v) = read_f32_le(pkt.payload, 8) {
                out.insert("width".into(), json!(v));
            }
            if let Some(v) = read_f32_le(pkt.payload, 12) {
                out.insert("height".into(), json!(v));
            }
            if let Some(v) = read_f32_le(pkt.payload, 16) {
                out.insert("min_depth".into(), json!(v));
            }
            if let Some(v) = read_f32_le(pkt.payload, 20) {
                out.insert("max_depth".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::SetVertexBuffers => match pkt.decode_set_vertex_buffers_payload_le() {
            Ok((cmd, bindings)) => {
                let start_slot = cmd.start_slot;
                let buffer_count = cmd.buffer_count;
                out.insert("start_slot".into(), json!(start_slot));
                out.insert("buffer_count".into(), json!(buffer_count));
                if let Some(first) = bindings.first() {
                    let vb0_buffer = first.buffer;
                    let vb0_stride_bytes = first.stride_bytes;
                    let vb0_offset_bytes = first.offset_bytes;
                    out.insert("vb0_buffer".into(), json!(vb0_buffer));
                    out.insert("vb0_stride_bytes".into(), json!(vb0_stride_bytes));
                    out.insert("vb0_offset_bytes".into(), json!(vb0_offset_bytes));
                }
            }
            Err(err) => {
                out.insert("decode_error".into(), json!(format!("{:?}", err)));
            }
        },
        AerogpuCmdOpcode::SetPrimitiveTopology => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("topology".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::SetConstantBuffers => match pkt.decode_set_constant_buffers_payload_le() {
            Ok((cmd, bindings)) => {
                let shader_stage = cmd.shader_stage;
                let start_slot = cmd.start_slot;
                let buffer_count = cmd.buffer_count;
                let stage_ex = cmd.reserved0;
                out.insert("shader_stage".into(), json!(shader_stage));
                out.insert("start_slot".into(), json!(start_slot));
                out.insert("buffer_count".into(), json!(buffer_count));
                if shader_stage == 2 && stage_ex != 0 {
                    out.insert("stage_ex".into(), json!(stage_ex));
                }
                if let Some(first) = bindings.first() {
                    let cb0_buffer = first.buffer;
                    let cb0_offset_bytes = first.offset_bytes;
                    let cb0_size_bytes = first.size_bytes;
                    out.insert("cb0_buffer".into(), json!(cb0_buffer));
                    out.insert("cb0_offset_bytes".into(), json!(cb0_offset_bytes));
                    out.insert("cb0_size_bytes".into(), json!(cb0_size_bytes));
                }
            }
            Err(err) => {
                out.insert("decode_error".into(), json!(format!("{:?}", err)));
            }
        },
        AerogpuCmdOpcode::SetShaderResourceBuffers => {
            match pkt.decode_set_shader_resource_buffers_payload_le() {
                Ok((cmd, bindings)) => {
                    let shader_stage = cmd.shader_stage;
                    let start_slot = cmd.start_slot;
                    let buffer_count = cmd.buffer_count;
                    let stage_ex = cmd.reserved0;
                    out.insert("shader_stage".into(), json!(shader_stage));
                    out.insert("start_slot".into(), json!(start_slot));
                    out.insert("buffer_count".into(), json!(buffer_count));
                    if shader_stage == 2 && stage_ex != 0 {
                        out.insert("stage_ex".into(), json!(stage_ex));
                    }
                    if let Some(first) = bindings.first() {
                        let srv0_buffer = first.buffer;
                        let srv0_offset_bytes = first.offset_bytes;
                        let srv0_size_bytes = first.size_bytes;
                        out.insert("srv0_buffer".into(), json!(srv0_buffer));
                        out.insert("srv0_offset_bytes".into(), json!(srv0_offset_bytes));
                        out.insert("srv0_size_bytes".into(), json!(srv0_size_bytes));
                    }
                }
                Err(err) => {
                    out.insert("decode_error".into(), json!(format!("{:?}", err)));
                }
            }
        }
        AerogpuCmdOpcode::SetUnorderedAccessBuffers => {
            match pkt.decode_set_unordered_access_buffers_payload_le() {
                Ok((cmd, bindings)) => {
                    let shader_stage = cmd.shader_stage;
                    let start_slot = cmd.start_slot;
                    let uav_count = cmd.uav_count;
                    let stage_ex = cmd.reserved0;
                    out.insert("shader_stage".into(), json!(shader_stage));
                    out.insert("start_slot".into(), json!(start_slot));
                    out.insert("uav_count".into(), json!(uav_count));
                    if shader_stage == 2 && stage_ex != 0 {
                        out.insert("stage_ex".into(), json!(stage_ex));
                    }
                    if let Some(first) = bindings.first() {
                        let uav0_buffer = first.buffer;
                        let uav0_offset_bytes = first.offset_bytes;
                        let uav0_size_bytes = first.size_bytes;
                        let uav0_initial_count = first.initial_count;
                        out.insert("uav0_buffer".into(), json!(uav0_buffer));
                        out.insert("uav0_offset_bytes".into(), json!(uav0_offset_bytes));
                        out.insert("uav0_size_bytes".into(), json!(uav0_size_bytes));
                        out.insert("uav0_initial_count".into(), json!(uav0_initial_count));
                    }
                }
                Err(err) => {
                    out.insert("decode_error".into(), json!(format!("{:?}", err)));
                }
            }
        }
        AerogpuCmdOpcode::Dispatch => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("group_count_x".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("group_count_y".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 8) {
                out.insert("group_count_z".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::Clear => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("flags".into(), json!(v));
            }
            // color_rgba_f32[4] start at payload offset 4.
            let mut rgba = Vec::new();
            for i in 0..4 {
                if let Some(f) = read_f32_le(pkt.payload, 4 + i * 4) {
                    rgba.push(Value::from(f));
                }
            }
            if rgba.len() == 4 {
                out.insert("color_rgba".into(), Value::Array(rgba));
            }
        }
        AerogpuCmdOpcode::Draw => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("vertex_count".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("instance_count".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 8) {
                out.insert("first_vertex".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 12) {
                out.insert("first_instance".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::DrawIndexed => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("index_count".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("instance_count".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 8) {
                out.insert("first_index".into(), json!(v));
            }
            if let Some(v) = read_i32_le(pkt.payload, 12) {
                out.insert("base_vertex".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 16) {
                out.insert("first_instance".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::Present => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("scanout_id".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("flags".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::PresentEx => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("scanout_id".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("flags".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 8) {
                out.insert("d3d9_present_flags".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::ExportSharedSurface => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("resource_handle".into(), json!(v));
            }
            if let Some(v) = read_u64_le(pkt.payload, 8) {
                out.insert("share_token".into(), json!(v.to_string()));
            }
        }
        AerogpuCmdOpcode::ImportSharedSurface => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("out_resource_handle".into(), json!(v));
            }
            if let Some(v) = read_u64_le(pkt.payload, 8) {
                out.insert("share_token".into(), json!(v.to_string()));
            }
        }
        AerogpuCmdOpcode::ReleaseSharedSurface => {
            if let Some(v) = read_u64_le(pkt.payload, 0) {
                out.insert("share_token".into(), json!(v.to_string()));
            }
        }
        AerogpuCmdOpcode::Flush => {}
        AerogpuCmdOpcode::Nop => {}
        AerogpuCmdOpcode::DebugMarker => {}
        // Everything else: no additional decode for now.
        _ => {}
    }

    out
}

fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    let bytes = buf.get(off..off + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_i32_le(buf: &[u8], off: usize) -> Option<i32> {
    read_u32_le(buf, off).map(|v| v as i32)
}

fn read_u64_le(buf: &[u8], off: usize) -> Option<u64> {
    let bytes = buf.get(off..off + 8)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

fn read_f32_le(buf: &[u8], off: usize) -> Option<f32> {
    read_u32_le(buf, off).map(f32::from_bits)
}

fn cmd_decode_error_kind(err: &AerogpuCmdDecodeError) -> &'static str {
    match err {
        AerogpuCmdDecodeError::BufferTooSmall => "BufferTooSmall",
        AerogpuCmdDecodeError::BadMagic { .. } => "BadMagic",
        AerogpuCmdDecodeError::Abi(_) => "Abi",
        AerogpuCmdDecodeError::BadSizeBytes { .. } => "BadSizeBytes",
        AerogpuCmdDecodeError::SizeNotAligned { .. } => "SizeNotAligned",
        AerogpuCmdDecodeError::PacketOverrunsStream { .. } => "PacketOverrunsStream",
        AerogpuCmdDecodeError::UnexpectedOpcode { .. } => "UnexpectedOpcode",
        AerogpuCmdDecodeError::PayloadSizeMismatch { .. } => "PayloadSizeMismatch",
        AerogpuCmdDecodeError::CountOverflow => "CountOverflow",
    }
}

fn cmd_decode_error_details(err: &AerogpuCmdDecodeError) -> String {
    match err {
        AerogpuCmdDecodeError::BadMagic { found } => format!("found=0x{found:08x}"),
        AerogpuCmdDecodeError::Abi(inner) => format!("{inner:?}"),
        AerogpuCmdDecodeError::BadSizeBytes { found } => format!("found={found}"),
        AerogpuCmdDecodeError::SizeNotAligned { found } => format!("found={found}"),
        AerogpuCmdDecodeError::PacketOverrunsStream {
            offset,
            packet_size_bytes,
            stream_size_bytes,
        } => format!(
            "offset={offset} packet_size_bytes={packet_size_bytes} stream_size_bytes={stream_size_bytes}"
        ),
        AerogpuCmdDecodeError::UnexpectedOpcode { found, expected } => {
            format!("found=0x{found:08x} expected={expected:?}")
        }
        AerogpuCmdDecodeError::PayloadSizeMismatch { expected, found } => {
            format!("expected={expected} found={found}")
        }
        AerogpuCmdDecodeError::BufferTooSmall => String::new(),
        AerogpuCmdDecodeError::CountOverflow => String::new(),
    }
}

fn cmd_decode_error_to_json(err: &AerogpuCmdDecodeError) -> Value {
    match err {
        AerogpuCmdDecodeError::BufferTooSmall => json!({
            "kind": "BufferTooSmall",
        }),
        AerogpuCmdDecodeError::BadMagic { found } => json!({
            "kind": "BadMagic",
            "found": found,
        }),
        AerogpuCmdDecodeError::Abi(inner) => json!({
            "kind": "Abi",
            "details": format!("{inner:?}"),
        }),
        AerogpuCmdDecodeError::BadSizeBytes { found } => json!({
            "kind": "BadSizeBytes",
            "found": found,
        }),
        AerogpuCmdDecodeError::SizeNotAligned { found } => json!({
            "kind": "SizeNotAligned",
            "found": found,
        }),
        AerogpuCmdDecodeError::PacketOverrunsStream {
            offset,
            packet_size_bytes,
            stream_size_bytes,
        } => json!({
            "kind": "PacketOverrunsStream",
            "offset": offset,
            "packet_size_bytes": packet_size_bytes,
            "stream_size_bytes": stream_size_bytes,
        }),
        AerogpuCmdDecodeError::UnexpectedOpcode { found, expected } => json!({
            "kind": "UnexpectedOpcode",
            "found": found,
            "expected": format!("{expected:?}"),
        }),
        AerogpuCmdDecodeError::PayloadSizeMismatch { expected, found } => json!({
            "kind": "PayloadSizeMismatch",
            "expected": expected,
            "found": found,
        }),
        AerogpuCmdDecodeError::CountOverflow => json!({
            "kind": "CountOverflow",
        }),
    }
}
