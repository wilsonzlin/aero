use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuBlendFactor, AerogpuBlendOp, AerogpuCmdDecodeError, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AerogpuCompareFunc, AerogpuCullMode,
    AerogpuFillMode, AerogpuIndexFormat, AerogpuPrimitiveTopology, AerogpuSamplerAddressMode,
    AerogpuSamplerFilter, AerogpuShaderStage, AEROGPU_STAGE_EX_MIN_ABI_MINOR,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
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
    let abi_minor = (iter.header().abi_version & 0xFFFF) as u16;
    let header: CmdStreamHeaderListing = (*iter.header()).into();

    let mut records = Vec::new();
    let mut offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    for pkt in iter {
        match pkt {
            Ok(pkt) => {
                let size_bytes = pkt.hdr.size_bytes;
                let decoded = decode_known_fields(&pkt, abi_minor);
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
    abi_minor: u16,
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
                if let Some(name) = decode_format_name(v) {
                    out.insert("format_name".into(), Value::String(name));
                }
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
        AerogpuCmdOpcode::CreateTextureView => {
            // u32 view_handle; u32 texture_handle; u32 format; u32 base_mip_level; u32 mip_level_count;
            // u32 base_array_layer; u32 array_layer_count; ...
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("view_handle".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 4) {
                out.insert("texture_handle".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 8) {
                out.insert("format".into(), json!(v));
                if let Some(name) = decode_format_name(v) {
                    out.insert("format_name".into(), Value::String(name));
                }
            }
            if let Some(v) = read_u32_le(pkt.payload, 12) {
                out.insert("base_mip_level".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 16) {
                out.insert("mip_level_count".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 20) {
                out.insert("base_array_layer".into(), json!(v));
            }
            if let Some(v) = read_u32_le(pkt.payload, 24) {
                out.insert("array_layer_count".into(), json!(v));
            }
        }
        AerogpuCmdOpcode::DestroyTextureView => {
            if let Some(v) = read_u32_le(pkt.payload, 0) {
                out.insert("view_handle".into(), json!(v));
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
                out.insert("data_prefix".into(), json!(hex_prefix(data, 16)));
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
        AerogpuCmdOpcode::CopyTexture2d => {
            if pkt.payload.len() < 56 {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            }
            out.insert(
                "dst_texture".into(),
                json!(read_u32_le(pkt.payload, 0).unwrap()),
            );
            out.insert(
                "src_texture".into(),
                json!(read_u32_le(pkt.payload, 4).unwrap()),
            );
            out.insert(
                "dst_mip_level".into(),
                json!(read_u32_le(pkt.payload, 8).unwrap()),
            );
            out.insert(
                "dst_array_layer".into(),
                json!(read_u32_le(pkt.payload, 12).unwrap()),
            );
            out.insert(
                "src_mip_level".into(),
                json!(read_u32_le(pkt.payload, 16).unwrap()),
            );
            out.insert(
                "src_array_layer".into(),
                json!(read_u32_le(pkt.payload, 20).unwrap()),
            );
            out.insert("dst_x".into(), json!(read_u32_le(pkt.payload, 24).unwrap()));
            out.insert("dst_y".into(), json!(read_u32_le(pkt.payload, 28).unwrap()));
            out.insert("src_x".into(), json!(read_u32_le(pkt.payload, 32).unwrap()));
            out.insert("src_y".into(), json!(read_u32_le(pkt.payload, 36).unwrap()));
            out.insert("width".into(), json!(read_u32_le(pkt.payload, 40).unwrap()));
            out.insert(
                "height".into(),
                json!(read_u32_le(pkt.payload, 44).unwrap()),
            );
            out.insert("flags".into(), json!(read_u32_le(pkt.payload, 48).unwrap()));
            let reserved0 = read_u32_le(pkt.payload, 52).unwrap();
            if reserved0 != 0 {
                out.insert("reserved0".into(), json!(reserved0));
            }
        }
        AerogpuCmdOpcode::CreateShaderDxbc => match pkt.decode_create_shader_dxbc_payload_le() {
            Ok((cmd, dxbc)) => {
                let shader_handle = cmd.shader_handle;
                let stage = cmd.stage;
                let stage_ex = cmd.reserved0;
                let dxbc_size_bytes = cmd.dxbc_size_bytes;
                out.insert("shader_handle".into(), json!(shader_handle));
                out.insert("stage".into(), json!(stage));
                if let Some(name) = shader_stage_name(stage) {
                    out.insert("stage_name".into(), Value::String(name));
                }
                if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR && stage == 2 && stage_ex != 0 {
                    out.insert("stage_ex".into(), json!(stage_ex));
                    out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
                } else if stage_ex != 0 {
                    out.insert("reserved0".into(), json!(stage_ex));
                }
                out.insert("dxbc_size_bytes".into(), json!(dxbc_size_bytes));
                out.insert("dxbc_len".into(), json!(dxbc.len()));
                out.insert("dxbc_prefix".into(), json!(hex_prefix(dxbc, 16)));
            }
            Err(err) => {
                out.insert("decode_error".into(), json!(format!("{:?}", err)));
            }
        },
        AerogpuCmdOpcode::BindShaders => match pkt.decode_bind_shaders_payload_le() {
            Ok((cmd, ex)) => {
                // Avoid taking references to packed fields.
                let vs = cmd.vs;
                let ps = cmd.ps;
                let cs = cmd.cs;
                out.insert("vs".into(), json!(vs));
                out.insert("ps".into(), json!(ps));
                out.insert("cs".into(), json!(cs));
                match ex {
                    Some(ex) => {
                        let gs = ex.gs;
                        let hs = ex.hs;
                        let ds = ex.ds;
                        out.insert("gs".into(), json!(gs));
                        out.insert("hs".into(), json!(hs));
                        out.insert("ds".into(), json!(ds));
                    }
                    None => {
                        // Legacy encoding: `reserved0` is used as an optional GS handle. HS/DS are
                        // unavailable in the base packet format.
                        let gs = cmd.gs();
                        out.insert("gs".into(), json!(gs));
                        out.insert("hs".into(), json!(0));
                        out.insert("ds".into(), json!(0));
                    }
                }
            }
            Err(err) => {
                out.insert("decode_error".into(), json!(format!("{:?}", err)));
            }
        },
        AerogpuCmdOpcode::DestroyShader => {
            if let (Some(shader_handle), Some(_reserved0)) =
                (read_u32_le(pkt.payload, 0), read_u32_le(pkt.payload, 4))
            {
                out.insert("shader_handle".into(), json!(shader_handle));
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
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
        AerogpuCmdOpcode::DestroyInputLayout => {
            if let (Some(input_layout_handle), Some(_reserved0)) =
                (read_u32_le(pkt.payload, 0), read_u32_le(pkt.payload, 4))
            {
                out.insert("input_layout_handle".into(), json!(input_layout_handle));
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
        AerogpuCmdOpcode::SetInputLayout => {
            if let (Some(input_layout_handle), Some(_reserved0)) =
                (read_u32_le(pkt.payload, 0), read_u32_le(pkt.payload, 4))
            {
                out.insert("input_layout_handle".into(), json!(input_layout_handle));
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
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
        AerogpuCmdOpcode::SetScissor => {
            if pkt.payload.len() < 16 {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            }
            out.insert("x".into(), json!(read_i32_le(pkt.payload, 0).unwrap()));
            out.insert("y".into(), json!(read_i32_le(pkt.payload, 4).unwrap()));
            out.insert("width".into(), json!(read_i32_le(pkt.payload, 8).unwrap()));
            out.insert(
                "height".into(),
                json!(read_i32_le(pkt.payload, 12).unwrap()),
            );
        }
        AerogpuCmdOpcode::SetBlendState => {
            if pkt.payload.len() < 52 {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            }
            let enable = read_u32_le(pkt.payload, 0).unwrap();
            let src_factor = read_u32_le(pkt.payload, 4).unwrap();
            let dst_factor = read_u32_le(pkt.payload, 8).unwrap();
            let blend_op = read_u32_le(pkt.payload, 12).unwrap();

            out.insert("enable".into(), json!(enable));
            out.insert("src_factor".into(), json!(src_factor));
            if let Some(name) = decode_blend_factor_name(src_factor) {
                out.insert("src_factor_name".into(), Value::String(name));
            }
            out.insert("dst_factor".into(), json!(dst_factor));
            if let Some(name) = decode_blend_factor_name(dst_factor) {
                out.insert("dst_factor_name".into(), Value::String(name));
            }
            out.insert("blend_op".into(), json!(blend_op));
            if let Some(name) = decode_blend_op_name(blend_op) {
                out.insert("blend_op_name".into(), Value::String(name));
            }
            out.insert("color_write_mask".into(), json!(pkt.payload[16]));
            out.insert(
                "src_factor_alpha".into(),
                json!(read_u32_le(pkt.payload, 20).unwrap()),
            );
            out.insert(
                "dst_factor_alpha".into(),
                json!(read_u32_le(pkt.payload, 24).unwrap()),
            );
            out.insert(
                "blend_op_alpha".into(),
                json!(read_u32_le(pkt.payload, 28).unwrap()),
            );
            // blend_constant_rgba_f32[4] at payload offset 32.
            let mut rgba = Vec::new();
            for i in 0..4 {
                rgba.push(Value::from(read_f32_le(pkt.payload, 32 + i * 4).unwrap()));
            }
            out.insert("blend_constant_rgba".into(), Value::Array(rgba));
            out.insert(
                "sample_mask".into(),
                json!(read_u32_le(pkt.payload, 48).unwrap()),
            );
        }
        AerogpuCmdOpcode::SetDepthStencilState => {
            if pkt.payload.len() < 20 {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            }
            let depth_func = read_u32_le(pkt.payload, 8).unwrap();
            out.insert(
                "depth_enable".into(),
                json!(read_u32_le(pkt.payload, 0).unwrap()),
            );
            out.insert(
                "depth_write_enable".into(),
                json!(read_u32_le(pkt.payload, 4).unwrap()),
            );
            out.insert("depth_func".into(), json!(depth_func));
            if let Some(name) = decode_compare_func_name(depth_func) {
                out.insert("depth_func_name".into(), Value::String(name));
            }
            out.insert(
                "stencil_enable".into(),
                json!(read_u32_le(pkt.payload, 12).unwrap()),
            );
            out.insert("stencil_read_mask".into(), json!(pkt.payload[16]));
            out.insert("stencil_write_mask".into(), json!(pkt.payload[17]));
        }
        AerogpuCmdOpcode::SetRasterizerState => {
            if pkt.payload.len() < 24 {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            }
            let fill_mode = read_u32_le(pkt.payload, 0).unwrap();
            let cull_mode = read_u32_le(pkt.payload, 4).unwrap();
            out.insert("fill_mode".into(), json!(fill_mode));
            if let Some(name) = decode_fill_mode_name(fill_mode) {
                out.insert("fill_mode_name".into(), Value::String(name));
            }
            out.insert("cull_mode".into(), json!(cull_mode));
            if let Some(name) = decode_cull_mode_name(cull_mode) {
                out.insert("cull_mode_name".into(), Value::String(name));
            }
            out.insert(
                "front_ccw".into(),
                json!(read_u32_le(pkt.payload, 8).unwrap()),
            );
            out.insert(
                "scissor_enable".into(),
                json!(read_u32_le(pkt.payload, 12).unwrap()),
            );
            out.insert(
                "depth_bias".into(),
                json!(read_i32_le(pkt.payload, 16).unwrap()),
            );
            out.insert("flags".into(), json!(read_u32_le(pkt.payload, 20).unwrap()));
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
        AerogpuCmdOpcode::SetIndexBuffer => {
            if let (Some(buffer), Some(format), Some(offset_bytes), Some(reserved0)) = (
                read_u32_le(pkt.payload, 0),
                read_u32_le(pkt.payload, 4),
                read_u32_le(pkt.payload, 8),
                read_u32_le(pkt.payload, 12),
            ) {
                out.insert("buffer".into(), json!(buffer));
                out.insert("format".into(), json!(format));
                if let Some(name) = decode_index_format_name(format) {
                    out.insert("format_name".into(), json!(name));
                }
                out.insert("offset_bytes".into(), json!(offset_bytes));
                if reserved0 != 0 {
                    out.insert("reserved0".into(), json!(reserved0));
                }
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
        AerogpuCmdOpcode::SetPrimitiveTopology => {
            if let (Some(topology), Some(_reserved0)) =
                (read_u32_le(pkt.payload, 0), read_u32_le(pkt.payload, 4))
            {
                out.insert("topology".into(), json!(topology));
                if let Some(name) = decode_topology_name(topology) {
                    out.insert("topology_name".into(), Value::String(name));
                }
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
        AerogpuCmdOpcode::SetTexture => {
            if let (Some(shader_stage), Some(slot), Some(texture), Some(stage_ex)) = (
                read_u32_le(pkt.payload, 0),
                read_u32_le(pkt.payload, 4),
                read_u32_le(pkt.payload, 8),
                read_u32_le(pkt.payload, 12),
            ) {
                out.insert("shader_stage".into(), json!(shader_stage));
                if let Some(name) = shader_stage_name(shader_stage) {
                    out.insert("shader_stage_name".into(), Value::String(name));
                }
                out.insert("slot".into(), json!(slot));
                out.insert("texture".into(), json!(texture));
                if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR && shader_stage == 2 && stage_ex != 0
                {
                    out.insert("stage_ex".into(), json!(stage_ex));
                    out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
                } else if stage_ex != 0 {
                    out.insert("reserved0".into(), json!(stage_ex));
                }
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
        AerogpuCmdOpcode::SetSamplerState => {
            if let (Some(shader_stage), Some(slot), Some(state), Some(value)) = (
                read_u32_le(pkt.payload, 0),
                read_u32_le(pkt.payload, 4),
                read_u32_le(pkt.payload, 8),
                read_u32_le(pkt.payload, 12),
            ) {
                out.insert("shader_stage".into(), json!(shader_stage));
                if let Some(name) = shader_stage_name(shader_stage) {
                    out.insert("shader_stage_name".into(), Value::String(name));
                }
                out.insert("slot".into(), json!(slot));
                out.insert("state".into(), json!(state));
                out.insert("value".into(), json!(value));
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
        AerogpuCmdOpcode::SetRenderState => {
            if let (Some(state), Some(value)) =
                (read_u32_le(pkt.payload, 0), read_u32_le(pkt.payload, 4))
            {
                out.insert("state".into(), json!(state));
                out.insert("value".into(), json!(value));
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
        AerogpuCmdOpcode::CreateSampler => {
            if let (
                Some(sampler_handle),
                Some(filter),
                Some(address_u),
                Some(address_v),
                Some(address_w),
            ) = (
                read_u32_le(pkt.payload, 0),
                read_u32_le(pkt.payload, 4),
                read_u32_le(pkt.payload, 8),
                read_u32_le(pkt.payload, 12),
                read_u32_le(pkt.payload, 16),
            ) {
                out.insert("sampler_handle".into(), json!(sampler_handle));
                out.insert("filter".into(), json!(filter));
                if let Some(name) = decode_sampler_filter_name(filter) {
                    out.insert("filter_name".into(), Value::String(name));
                }
                out.insert("address_u".into(), json!(address_u));
                if let Some(name) = decode_sampler_address_mode_name(address_u) {
                    out.insert("address_u_name".into(), Value::String(name));
                }
                out.insert("address_v".into(), json!(address_v));
                if let Some(name) = decode_sampler_address_mode_name(address_v) {
                    out.insert("address_v_name".into(), Value::String(name));
                }
                out.insert("address_w".into(), json!(address_w));
                if let Some(name) = decode_sampler_address_mode_name(address_w) {
                    out.insert("address_w_name".into(), Value::String(name));
                }
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
        AerogpuCmdOpcode::DestroySampler => {
            if let (Some(sampler_handle), Some(_reserved0)) =
                (read_u32_le(pkt.payload, 0), read_u32_le(pkt.payload, 4))
            {
                out.insert("sampler_handle".into(), json!(sampler_handle));
            } else {
                out.insert("decode_error".into(), json!("truncated payload"));
            }
        }
        AerogpuCmdOpcode::SetSamplers => match pkt.decode_set_samplers_payload_le() {
            Ok((cmd, handles)) => {
                let shader_stage = cmd.shader_stage;
                let start_slot = cmd.start_slot;
                let sampler_count = cmd.sampler_count;
                let stage_ex = cmd.reserved0;
                out.insert("shader_stage".into(), json!(shader_stage));
                if let Some(name) = shader_stage_name(shader_stage) {
                    out.insert("shader_stage_name".into(), Value::String(name));
                }
                out.insert("start_slot".into(), json!(start_slot));
                out.insert("sampler_count".into(), json!(sampler_count));
                if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR && shader_stage == 2 && stage_ex != 0
                {
                    out.insert("stage_ex".into(), json!(stage_ex));
                    out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
                } else if stage_ex != 0 {
                    out.insert("reserved0".into(), json!(stage_ex));
                }
                if let Some(first) = handles.first() {
                    out.insert("sampler0".into(), json!(*first));
                }
            }
            Err(err) => {
                out.insert("decode_error".into(), json!(format!("{:?}", err)));
            }
        },
        AerogpuCmdOpcode::SetShaderConstantsF => {
            let Some(stage) = read_u32_le(pkt.payload, 0) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(start_register) = read_u32_le(pkt.payload, 4) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(vec4_count) = read_u32_le(pkt.payload, 8) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(stage_ex) = read_u32_le(pkt.payload, 12) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            out.insert("stage".into(), json!(stage));
            if let Some(name) = shader_stage_name(stage) {
                out.insert("stage_name".into(), Value::String(name));
            }
            out.insert("start_register".into(), json!(start_register));
            out.insert("vec4_count".into(), json!(vec4_count));
            if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR && stage == 2 && stage_ex != 0 {
                out.insert("stage_ex".into(), json!(stage_ex));
                out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
            } else if stage_ex != 0 {
                out.insert("reserved0".into(), json!(stage_ex));
            }
            if let Some(float_count) = vec4_count.checked_mul(4) {
                out.insert("float_count".into(), json!(float_count));
            }
            let Some(data_len) = vec4_count.checked_mul(16) else {
                out.insert("decode_error".into(), json!("vec4_count overflow"));
                return out;
            };
            let data_len = data_len as usize;
            let data_start = 16usize;
            let Some(data_end) = data_start.checked_add(data_len) else {
                out.insert("decode_error".into(), json!("vec4_count overflow"));
                return out;
            };
            if data_end > pkt.payload.len() {
                out.insert(
                    "decode_error".into(),
                    json!("payload too small for vec4_count"),
                );
                return out;
            }
            let data = &pkt.payload[data_start..data_end];
            out.insert("data_len".into(), json!(data.len()));
            out.insert("data_prefix".into(), json!(hex_prefix(data, 16)));
        }
        AerogpuCmdOpcode::SetShaderConstantsI => {
            let Some(stage) = read_u32_le(pkt.payload, 0) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(start_register) = read_u32_le(pkt.payload, 4) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(vec4_count) = read_u32_le(pkt.payload, 8) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(stage_ex) = read_u32_le(pkt.payload, 12) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            out.insert("stage".into(), json!(stage));
            if let Some(name) = shader_stage_name(stage) {
                out.insert("stage_name".into(), Value::String(name));
            }
            out.insert("start_register".into(), json!(start_register));
            out.insert("vec4_count".into(), json!(vec4_count));
            if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR && stage == 2 && stage_ex != 0 {
                out.insert("stage_ex".into(), json!(stage_ex));
                out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
            } else if stage_ex != 0 {
                out.insert("reserved0".into(), json!(stage_ex));
            }
            if let Some(int_count) = vec4_count.checked_mul(4) {
                out.insert("int_count".into(), json!(int_count));
            }
            let Some(data_len) = vec4_count.checked_mul(16) else {
                out.insert("decode_error".into(), json!("vec4_count overflow"));
                return out;
            };
            let data_len = data_len as usize;
            let data_start = 16usize;
            let Some(data_end) = data_start.checked_add(data_len) else {
                out.insert("decode_error".into(), json!("vec4_count overflow"));
                return out;
            };
            if data_end > pkt.payload.len() {
                out.insert(
                    "decode_error".into(),
                    json!("payload too small for vec4_count"),
                );
                return out;
            }
            let data = &pkt.payload[data_start..data_end];
            out.insert("data_len".into(), json!(data.len()));
            out.insert("data_prefix".into(), json!(hex_prefix(data, 16)));
        }
        AerogpuCmdOpcode::SetShaderConstantsB => {
            let Some(stage) = read_u32_le(pkt.payload, 0) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(start_register) = read_u32_le(pkt.payload, 4) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(bool_count) = read_u32_le(pkt.payload, 8) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            let Some(stage_ex) = read_u32_le(pkt.payload, 12) else {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            };
            out.insert("stage".into(), json!(stage));
            if let Some(name) = shader_stage_name(stage) {
                out.insert("stage_name".into(), Value::String(name));
            }
            out.insert("start_register".into(), json!(start_register));
            out.insert("bool_count".into(), json!(bool_count));
            if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR && stage == 2 && stage_ex != 0 {
                out.insert("stage_ex".into(), json!(stage_ex));
                out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
            } else if stage_ex != 0 {
                out.insert("reserved0".into(), json!(stage_ex));
            }
            let Some(data_len) = bool_count.checked_mul(16) else {
                out.insert("decode_error".into(), json!("bool_count overflow"));
                return out;
            };
            let data_len = data_len as usize;
            let data_start = 16usize;
            let Some(data_end) = data_start.checked_add(data_len) else {
                out.insert("decode_error".into(), json!("bool_count overflow"));
                return out;
            };
            if data_end > pkt.payload.len() {
                out.insert(
                    "decode_error".into(),
                    json!("payload too small for bool_count"),
                );
                return out;
            }
            let data = &pkt.payload[data_start..data_end];
            out.insert("data_len".into(), json!(data.len()));
            out.insert("data_prefix".into(), json!(hex_prefix(data, 16)));
        }
        AerogpuCmdOpcode::SetConstantBuffers => {
            match pkt.decode_set_constant_buffers_payload_le() {
                Ok((cmd, bindings)) => {
                    let shader_stage = cmd.shader_stage;
                    let start_slot = cmd.start_slot;
                    let buffer_count = cmd.buffer_count;
                    let stage_ex = cmd.reserved0;
                    out.insert("shader_stage".into(), json!(shader_stage));
                    if let Some(name) = shader_stage_name(shader_stage) {
                        out.insert("shader_stage_name".into(), Value::String(name));
                    }
                    out.insert("start_slot".into(), json!(start_slot));
                    out.insert("buffer_count".into(), json!(buffer_count));
                    if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                        && shader_stage == 2
                        && stage_ex != 0
                    {
                        out.insert("stage_ex".into(), json!(stage_ex));
                        out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
                    } else if stage_ex != 0 {
                        out.insert("reserved0".into(), json!(stage_ex));
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
            }
        }
        AerogpuCmdOpcode::SetShaderResourceBuffers => {
            match pkt.decode_set_shader_resource_buffers_payload_le() {
                Ok((cmd, bindings)) => {
                    let shader_stage = cmd.shader_stage;
                    let start_slot = cmd.start_slot;
                    let buffer_count = cmd.buffer_count;
                    let stage_ex = cmd.reserved0;
                    out.insert("shader_stage".into(), json!(shader_stage));
                    if let Some(name) = shader_stage_name(shader_stage) {
                        out.insert("shader_stage_name".into(), Value::String(name));
                    }
                    out.insert("start_slot".into(), json!(start_slot));
                    out.insert("buffer_count".into(), json!(buffer_count));
                    if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                        && shader_stage == 2
                        && stage_ex != 0
                    {
                        out.insert("stage_ex".into(), json!(stage_ex));
                        out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
                    } else if stage_ex != 0 {
                        out.insert("reserved0".into(), json!(stage_ex));
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
                    if let Some(name) = shader_stage_name(shader_stage) {
                        out.insert("shader_stage_name".into(), Value::String(name));
                    }
                    out.insert("start_slot".into(), json!(start_slot));
                    out.insert("uav_count".into(), json!(uav_count));
                    if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                        && shader_stage == 2
                        && stage_ex != 0
                    {
                        out.insert("stage_ex".into(), json!(stage_ex));
                        out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
                    } else if stage_ex != 0 {
                        out.insert("reserved0".into(), json!(stage_ex));
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
            // `DISPATCH.reserved0` is repurposed as a `stage_ex` selector for extended-stage
            // compute execution (GS/HS/DS compute emulation). Gate interpretation by ABI minor to
            // avoid misinterpreting garbage in older streams.
            if let Some(stage_ex) = read_u32_le(pkt.payload, 12) {
                if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR && stage_ex != 0 {
                    out.insert("stage_ex".into(), json!(stage_ex));
                    out.insert("stage_ex_name".into(), json!(stage_ex_name(stage_ex)));
                } else if stage_ex != 0 {
                    out.insert("reserved0".into(), json!(stage_ex));
                }
            }
        }
        AerogpuCmdOpcode::Clear => {
            if pkt.payload.len() < 28 {
                out.insert("decode_error".into(), json!("truncated payload"));
                return out;
            }
            out.insert("flags".into(), json!(read_u32_le(pkt.payload, 0).unwrap()));
            // color_rgba_f32[4] start at payload offset 4.
            let mut rgba = Vec::new();
            for i in 0..4 {
                rgba.push(Value::from(read_f32_le(pkt.payload, 4 + i * 4).unwrap()));
            }
            out.insert("color_rgba".into(), Value::Array(rgba));
            out.insert("depth".into(), json!(read_f32_le(pkt.payload, 20).unwrap()));
            out.insert(
                "stencil".into(),
                json!(read_u32_le(pkt.payload, 24).unwrap()),
            );
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
        AerogpuCmdOpcode::DebugMarker => {
            let marker = String::from_utf8_lossy(pkt.payload)
                .trim_end_matches('\0')
                .replace('\n', "\\n");
            let marker = marker.chars().take(80).collect::<String>();
            out.insert("marker".into(), Value::String(marker));
        }
        // Everything else: no additional decode for now.
        _ => {}
    }

    out
}

fn decode_topology_name(topology: u32) -> Option<String> {
    AerogpuPrimitiveTopology::from_u32(topology).map(|t| format!("{t:?}"))
}

fn decode_index_format_name(format: u32) -> Option<String> {
    AerogpuIndexFormat::from_u32(format).map(|f| format!("{f:?}"))
}

fn decode_blend_factor_name(factor: u32) -> Option<String> {
    AerogpuBlendFactor::from_u32(factor).map(|f| format!("{f:?}"))
}

fn decode_blend_op_name(op: u32) -> Option<String> {
    AerogpuBlendOp::from_u32(op).map(|o| format!("{o:?}"))
}

fn decode_compare_func_name(func: u32) -> Option<String> {
    AerogpuCompareFunc::from_u32(func).map(|f| format!("{f:?}"))
}

fn decode_fill_mode_name(mode: u32) -> Option<String> {
    AerogpuFillMode::from_u32(mode).map(|m| format!("{m:?}"))
}

fn decode_cull_mode_name(mode: u32) -> Option<String> {
    AerogpuCullMode::from_u32(mode).map(|m| format!("{m:?}"))
}

fn decode_format_name(format: u32) -> Option<String> {
    AerogpuFormat::from_u32(format).map(|f| format!("{f:?}"))
}

fn decode_sampler_filter_name(filter: u32) -> Option<String> {
    AerogpuSamplerFilter::from_u32(filter).map(|f| format!("{f:?}"))
}

fn decode_sampler_address_mode_name(mode: u32) -> Option<String> {
    AerogpuSamplerAddressMode::from_u32(mode).map(|m| format!("{m:?}"))
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

fn hex_prefix(bytes: &[u8], max_len: usize) -> String {
    let mut out = String::new();
    let take = bytes.len().min(max_len);
    for b in &bytes[..take] {
        let _ = write!(out, "{b:02x}");
    }
    if bytes.len() > max_len {
        out.push_str("..");
    }
    out
}

fn shader_stage_name(shader_stage: u32) -> Option<String> {
    AerogpuShaderStage::from_u32(shader_stage).map(|s| format!("{s:?}"))
}

fn stage_ex_name(stage_ex: u32) -> &'static str {
    // Human-readable names for `stage_ex` discriminators (DXBC program type IDs).
    //
    // Note: `stage_ex=1` (Vertex DXBC program type) is intentionally invalid in AeroGPU; vertex
    // shaders must be encoded via the legacy `shader_stage = VERTEX` value for clarity.
    match stage_ex {
        1 => "InvalidVertex",
        2 => "Geometry",
        3 => "Hull",
        4 => "Domain",
        5 => "Compute",
        _ => "Unknown",
    }
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
