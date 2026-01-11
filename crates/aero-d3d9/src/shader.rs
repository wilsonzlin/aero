//! Shader parsing and translation (DXBC/D3D9 bytecode → IR → WGSL).

use std::collections::{BTreeSet, HashMap};

use blake3::Hash;
use thiserror::Error;

use crate::dxbc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Vertex,
    Pixel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderModel {
    pub major: u8,
    pub minor: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderVersion {
    pub stage: ShaderStage,
    pub model: ShaderModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegisterFile {
    Temp,        // r#
    Input,       // v#
    Const,       // c#
    Addr,        // a#
    Texture,     // t#
    Sampler,     // s#
    RastOut,     // oPos
    AttrOut,     // oD#
    TexCoordOut, // oT#
    ColorOut,    // oC#
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Register {
    pub file: RegisterFile,
    pub index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Swizzle(pub [u8; 4]);

impl Swizzle {
    pub const XYZW: Swizzle = Swizzle([0, 1, 2, 3]);

    pub fn from_d3d_byte(swz: u8) -> Self {
        // 2 bits per component, x in bits 0..1, y in 2..3, z in 4..5, w in 6..7.
        let comp = |shift: u32| ((swz >> shift) & 0b11u8) as u8;
        Self([comp(0), comp(2), comp(4), comp(6)])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WriteMask(pub u8);

impl WriteMask {
    pub const XYZW: WriteMask = WriteMask(0b1111);

    pub fn write_x(self) -> bool {
        self.0 & 0b0001 != 0
    }
    pub fn write_y(self) -> bool {
        self.0 & 0b0010 != 0
    }
    pub fn write_z(self) -> bool {
        self.0 & 0b0100 != 0
    }
    pub fn write_w(self) -> bool {
        self.0 & 0b1000 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Src {
    pub reg: Register,
    pub swizzle: Swizzle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Dst {
    pub reg: Register,
    pub mask: WriteMask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    Nop,
    Mov,
    Add,
    Mul,
    Mad,
    Dp3,
    Dp4,
    Rcp,
    Rsq,
    Min,
    Max,
    Cmp,
    Slt,
    Sge,
    Frc,
    Texld,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub op: Op,
    pub dst: Option<Dst>,
    pub src: Vec<Src>,
    /// Sampler register for `texld` (s#).
    pub sampler: Option<u16>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderProgram {
    pub version: ShaderVersion,
    pub instructions: Vec<Instruction>,
    pub used_samplers: BTreeSet<u16>,
    pub used_consts: BTreeSet<u16>,
    pub used_inputs: BTreeSet<u16>,
    pub used_outputs: BTreeSet<Register>,
    pub temp_count: u16,
}

#[derive(Debug, Error)]
pub enum ShaderError {
    #[error("dxbc error: {0}")]
    Dxbc(#[from] dxbc::DxbcError),
    #[error("shader token stream too small")]
    TokenStreamTooSmall,
    #[error("unsupported shader version token 0x{0:08x}")]
    UnsupportedVersion(u32),
    #[error("unexpected end of instruction stream")]
    UnexpectedEof,
    #[error("unknown or unsupported opcode 0x{0:04x}")]
    UnsupportedOpcode(u16),
    #[error("unsupported register type {0}")]
    UnsupportedRegisterType(u8),
}

fn read_u32(words: &[u32], idx: &mut usize) -> Result<u32, ShaderError> {
    if *idx >= words.len() {
        return Err(ShaderError::UnexpectedEof);
    }
    let v = words[*idx];
    *idx += 1;
    Ok(v)
}

fn decode_reg_type(token: u32) -> u8 {
    // D3D9 encodes register type in 5 bits split across two fields:
    // - 3 bits at 28..30
    // - 2 bits at 11..12 (shifted down by 8 to become bits 3..4)
    let low = ((token >> 28) & 0x7) as u8;
    let high = ((token >> 8) & 0x18) as u8;
    low | high
}

fn decode_reg_num(token: u32) -> u16 {
    (token & 0x7FF) as u16
}

fn decode_src(token: u32) -> Result<Src, ShaderError> {
    let reg_type = decode_reg_type(token);
    let reg_num = decode_reg_num(token);
    let swizzle_byte = ((token >> 16) & 0xFF) as u8;

    let file = match reg_type {
        0 => RegisterFile::Temp,
        1 => RegisterFile::Input,
        2 => RegisterFile::Const,
        3 => RegisterFile::Texture, // also Addr in vs; we treat as texture for pixel shader inputs.
        10 => RegisterFile::Sampler,
        other => return Err(ShaderError::UnsupportedRegisterType(other)),
    };

    Ok(Src {
        reg: Register {
            file,
            index: reg_num,
        },
        swizzle: Swizzle::from_d3d_byte(swizzle_byte),
    })
}

fn decode_dst(token: u32) -> Result<Dst, ShaderError> {
    let reg_type = decode_reg_type(token);
    let reg_num = decode_reg_num(token);
    let mask = ((token >> 16) & 0xF) as u8;

    let file = match reg_type {
        0 => RegisterFile::Temp,
        4 => RegisterFile::RastOut,
        5 => RegisterFile::AttrOut,
        6 => RegisterFile::TexCoordOut,
        8 => RegisterFile::ColorOut,
        other => return Err(ShaderError::UnsupportedRegisterType(other)),
    };

    Ok(Dst {
        reg: Register {
            file,
            index: reg_num,
        },
        mask: WriteMask(mask),
    })
}

fn opcode_to_op(opcode: u16) -> Option<Op> {
    // Based on D3DSHADER_INSTRUCTION_OPCODE_TYPE values.
    match opcode {
        0x0000 => Some(Op::Nop),
        0x0001 => Some(Op::Mov),
        0x0002 => Some(Op::Add),
        0x0004 => Some(Op::Mad),
        0x0005 => Some(Op::Mul),
        0x0006 => Some(Op::Rcp),
        0x0007 => Some(Op::Rsq),
        0x0008 => Some(Op::Dp3),
        0x0009 => Some(Op::Dp4),
        0x000A => Some(Op::Min),
        0x000B => Some(Op::Max),
        0x000C => Some(Op::Slt),
        0x000D => Some(Op::Sge),
        0x0013 => Some(Op::Frc),
        0x0042 => Some(Op::Texld), // D3DSIO_TEX
        0x0058 => Some(Op::Cmp),
        0xFFFF => Some(Op::End),
        _ => None,
    }
}

fn parse_token_stream(token_bytes: &[u8]) -> Result<ShaderProgram, ShaderError> {
    if token_bytes.len() < 4 {
        return Err(ShaderError::TokenStreamTooSmall);
    }
    if token_bytes.len() % 4 != 0 {
        return Err(ShaderError::TokenStreamTooSmall);
    }
    let words: Vec<u32> = token_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();

    let version_token = words[0];
    let shader_type = (version_token >> 16) as u16;
    let major = ((version_token >> 8) & 0xFF) as u8;
    let minor = (version_token & 0xFF) as u8;
    let stage = match shader_type {
        0xFFFE => ShaderStage::Vertex,
        0xFFFF => ShaderStage::Pixel,
        _ => return Err(ShaderError::UnsupportedVersion(version_token)),
    };
    if !(major == 2 || major == 3) {
        return Err(ShaderError::UnsupportedVersion(version_token));
    }
    let version = ShaderVersion {
        stage,
        model: ShaderModel { major, minor },
    };

    let mut idx = 1usize;
    let mut instructions = Vec::new();
    let mut used_samplers = BTreeSet::new();
    let mut used_consts = BTreeSet::new();
    let mut used_inputs = BTreeSet::new();
    let mut used_outputs = BTreeSet::new();
    let mut temp_max = 0u16;

    while idx < words.len() {
        let token = read_u32(&words, &mut idx)?;
        let opcode = (token & 0xFFFF) as u16;
        // D3D9 instruction length is encoded in bits 24..27 (4 bits). Higher bits in the same
        // byte are flags (predication/co-issue) and must not affect operand count.
        let param_count = ((token >> 24) & 0x0F) as usize;

        // Comments are variable-length data blocks that should be skipped.
        // Layout: opcode=0xFFFE, length in DWORDs in bits 16..30.
        if opcode == 0xFFFE {
            let comment_len = ((token >> 16) & 0x7FFF) as usize;
            if idx + comment_len > words.len() {
                return Err(ShaderError::UnexpectedEof);
            }
            idx += comment_len;
            continue;
        }

        // Declarations (DCL) are not currently used by the minimal WGSL backend; skip them.
        // The operand count is still encoded in bits 24..27.
        if opcode == 0x001F {
            if idx + param_count > words.len() {
                return Err(ShaderError::UnexpectedEof);
            }
            idx += param_count;
            continue;
        }

        let op = opcode_to_op(opcode).ok_or(ShaderError::UnsupportedOpcode(opcode))?;
        if op == Op::End {
            instructions.push(Instruction {
                op,
                dst: None,
                src: Vec::new(),
                sampler: None,
            });
            break;
        }

        let mut params = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            params.push(read_u32(&words, &mut idx)?);
        }

        let inst = match op {
            Op::Nop => Instruction {
                op,
                dst: None,
                src: Vec::new(),
                sampler: None,
            },
            Op::Mov
            | Op::Add
            | Op::Mul
            | Op::Mad
            | Op::Dp3
            | Op::Dp4
            | Op::Rcp
            | Op::Rsq
            | Op::Min
            | Op::Max
            | Op::Cmp
            | Op::Slt
            | Op::Sge
            | Op::Frc => {
                if params.len() < 2 {
                    return Err(ShaderError::UnexpectedEof);
                }
                let dst = decode_dst(params[0])?;
                let src = params[1..]
                    .iter()
                    .map(|t| decode_src(*t))
                    .collect::<Result<Vec<_>, _>>()?;
                Instruction {
                    op,
                    dst: Some(dst),
                    src,
                    sampler: None,
                }
            }
            Op::Texld => {
                // texld dst, coord, sampler
                if params.len() != 3 {
                    return Err(ShaderError::UnexpectedEof);
                }
                let dst = decode_dst(params[0])?;
                let coord = decode_src(params[1])?;
                let sampler_src = decode_src(params[2])?;
                let sampler_index = if sampler_src.reg.file == RegisterFile::Sampler {
                    sampler_src.reg.index
                } else {
                    return Err(ShaderError::UnsupportedRegisterType(99));
                };
                used_samplers.insert(sampler_index);
                Instruction {
                    op,
                    dst: Some(dst),
                    src: vec![coord],
                    sampler: Some(sampler_index),
                }
            }
            Op::End => unreachable!(),
        };

        // Track usage.
        if let Some(dst) = inst.dst {
            match dst.reg.file {
                RegisterFile::Temp => temp_max = temp_max.max(dst.reg.index + 1),
                RegisterFile::RastOut
                | RegisterFile::AttrOut
                | RegisterFile::TexCoordOut
                | RegisterFile::ColorOut => {
                    used_outputs.insert(dst.reg);
                }
                _ => {}
            }
        }
        for src in &inst.src {
            match src.reg.file {
                RegisterFile::Const => {
                    used_consts.insert(src.reg.index);
                }
                RegisterFile::Input | RegisterFile::Texture => {
                    used_inputs.insert(src.reg.index);
                }
                RegisterFile::Temp => temp_max = temp_max.max(src.reg.index + 1),
                RegisterFile::Sampler => {
                    used_samplers.insert(src.reg.index);
                }
                _ => {}
            }
        }

        instructions.push(inst);
    }

    Ok(ShaderProgram {
        version,
        instructions,
        used_samplers,
        used_consts,
        used_inputs,
        used_outputs,
        temp_count: temp_max.max(1),
    })
}

/// Parse DXBC or raw D3D9 shader bytecode into a [`ShaderProgram`].
pub fn parse(bytes: &[u8]) -> Result<ShaderProgram, ShaderError> {
    let token_stream = dxbc::extract_shader_bytecode(bytes)?;
    parse_token_stream(token_stream)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderIr {
    pub version: ShaderVersion,
    pub temp_count: u16,
    pub ops: Vec<Instruction>,
    pub used_samplers: BTreeSet<u16>,
    pub used_consts: BTreeSet<u16>,
    pub used_inputs: BTreeSet<u16>,
    pub used_outputs: BTreeSet<Register>,
}

pub fn to_ir(program: &ShaderProgram) -> ShaderIr {
    ShaderIr {
        version: program.version,
        temp_count: program.temp_count,
        ops: program.instructions.clone(),
        used_samplers: program.used_samplers.clone(),
        used_consts: program.used_consts.clone(),
        used_inputs: program.used_inputs.clone(),
        used_outputs: program.used_outputs.clone(),
    }
}

fn varying_location(reg: Register) -> Option<u32> {
    match reg.file {
        RegisterFile::AttrOut => Some(reg.index as u32),
        RegisterFile::TexCoordOut => Some(4 + reg.index as u32),
        _ => None,
    }
}

fn ps_input_location(reg: Register) -> Option<u32> {
    match reg.file {
        RegisterFile::Input => Some(reg.index as u32),
        RegisterFile::Texture => Some(4 + reg.index as u32),
        _ => None,
    }
}

fn reg_var_name(reg: Register) -> String {
    match reg.file {
        RegisterFile::Temp => format!("r{}", reg.index),
        RegisterFile::Input => format!("v{}", reg.index),
        RegisterFile::Const => format!("c{}", reg.index),
        RegisterFile::Addr => format!("a{}", reg.index),
        RegisterFile::Texture => format!("t{}", reg.index),
        RegisterFile::Sampler => format!("s{}", reg.index),
        RegisterFile::RastOut => "oPos".to_string(),
        RegisterFile::AttrOut => format!("oD{}", reg.index),
        RegisterFile::TexCoordOut => format!("oT{}", reg.index),
        RegisterFile::ColorOut => format!("oC{}", reg.index),
    }
}

fn swizzle_suffix(swz: Swizzle) -> String {
    let comp = |c| match c {
        0 => 'x',
        1 => 'y',
        2 => 'z',
        3 => 'w',
        _ => 'x',
    };
    let chars: String = swz.0.into_iter().map(comp).collect();
    format!(".{}", chars)
}

fn mask_suffix(mask: WriteMask) -> Option<String> {
    if mask == WriteMask::XYZW {
        return None;
    }
    let mut s = String::new();
    if mask.write_x() {
        s.push('x');
    }
    if mask.write_y() {
        s.push('y');
    }
    if mask.write_z() {
        s.push('z');
    }
    if mask.write_w() {
        s.push('w');
    }
    Some(format!(".{}", s))
}

#[derive(Debug, Clone)]
pub struct WgslOutput {
    pub wgsl: String,
    pub entry_point: &'static str,
    pub bind_group_layout: BindGroupLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindGroupLayout {
    /// Binding 0 is always the constants buffer.
    pub sampler_bindings: HashMap<u16, (u32, u32)>, // sampler_index -> (texture_binding, sampler_binding)
}

pub fn generate_wgsl(ir: &ShaderIr) -> WgslOutput {
    let mut wgsl = String::new();

    // Constants: fixed sized array to keep indexing simple.
    wgsl.push_str("struct Constants { c: array<vec4<f32>, 256>, };\n");
    wgsl.push_str("@group(0) @binding(0) var<uniform> constants: Constants;\n\n");

    let mut sampler_bindings = HashMap::new();
    // Allocate bindings: (texture, sampler) pairs.
    //
    // IMPORTANT: Binding numbers must be stable across shader stages. Using a sequential allocator
    // based on "samplers used by this stage" can produce mismatched binding locations when (for
    // example) the vertex shader samples from `s1` while the pixel shader samples from `s0`.
    //
    // Derive binding numbers from the D3D9 sampler register index instead:
    //   texture binding = 1 + 2*s
    //   sampler binding = 2 + 2*s
    for &s in &ir.used_samplers {
        let tex_binding = 1u32 + u32::from(s) * 2;
        let samp_binding = tex_binding + 1;
        sampler_bindings.insert(s, (tex_binding, samp_binding));
        wgsl.push_str(&format!(
            "@group(0) @binding({}) var tex{}: texture_2d<f32>;\n",
            tex_binding, s
        ));
        wgsl.push_str(&format!(
            "@group(0) @binding({}) var samp{}: sampler;\n",
            samp_binding, s
        ));
    }
    if !ir.used_samplers.is_empty() {
        wgsl.push('\n');
    }

    match ir.version.stage {
        ShaderStage::Vertex => {
            let has_inputs = !ir.used_inputs.is_empty();
            if has_inputs {
                // Input struct.
                wgsl.push_str("struct VsInput {\n");
                for &v in &ir.used_inputs {
                    wgsl.push_str(&format!("  @location({}) v{}: vec4<f32>,\n", v, v));
                }
                wgsl.push_str("};\n");
            }

            // Output struct.
            wgsl.push_str("struct VsOutput {\n  @builtin(position) pos: vec4<f32>,\n");
            for &reg in &ir.used_outputs {
                if reg.file == RegisterFile::RastOut {
                    continue;
                }
                if let Some(loc) = varying_location(reg) {
                    wgsl.push_str(&format!(
                        "  @location({}) {}: vec4<f32>,\n",
                        loc,
                        reg_var_name(reg)
                    ));
                }
            }
            wgsl.push_str("};\n\n");

            if has_inputs {
                wgsl.push_str("@vertex\nfn vs_main(input: VsInput) -> VsOutput {\n");
            } else {
                wgsl.push_str("@vertex\nfn vs_main() -> VsOutput {\n");
            }
            // Declare registers.
            for i in 0..ir.temp_count {
                wgsl.push_str(&format!("  var r{}: vec4<f32> = vec4<f32>(0.0);\n", i));
            }
            for &v in &ir.used_inputs {
                wgsl.push_str(&format!("  let v{}: vec4<f32> = input.v{};\n", v, v));
            }
            // Outputs.
            wgsl.push_str("  var oPos: vec4<f32> = vec4<f32>(0.0);\n");
            for &reg in &ir.used_outputs {
                match reg.file {
                    RegisterFile::AttrOut | RegisterFile::TexCoordOut => {
                        wgsl.push_str(&format!(
                            "  var {}: vec4<f32> = vec4<f32>(0.0);\n",
                            reg_var_name(reg)
                        ));
                    }
                    _ => {}
                }
            }
            wgsl.push('\n');

            // Instruction emission.
            for inst in &ir.ops {
                emit_inst(&mut wgsl, inst);
            }

            wgsl.push_str("  var out: VsOutput;\n  out.pos = oPos;\n");
            for &reg in &ir.used_outputs {
                if reg.file == RegisterFile::RastOut {
                    continue;
                }
                if varying_location(reg).is_some() {
                    wgsl.push_str(&format!(
                        "  out.{} = {};\n",
                        reg_var_name(reg),
                        reg_var_name(reg)
                    ));
                }
            }
            wgsl.push_str("  return out;\n}\n");

            WgslOutput {
                wgsl,
                entry_point: "vs_main",
                bind_group_layout: BindGroupLayout { sampler_bindings },
            }
        }
        ShaderStage::Pixel => {
            let mut inputs_by_reg = BTreeSet::new();
            for inst in &ir.ops {
                for src in &inst.src {
                    match src.reg.file {
                        RegisterFile::Input | RegisterFile::Texture => {
                            inputs_by_reg.insert(src.reg);
                        }
                        _ => {}
                    }
                }
            }
            let has_inputs = !inputs_by_reg.is_empty();
            if has_inputs {
                // Inputs are driven by varying mapping. We just emit for any used input regs.
                // For simplicity we emit `v#` as @location(#) and `t#` as @location(4+#).
                wgsl.push_str("struct PsInput {\n");
                for reg in &inputs_by_reg {
                    if let Some(loc) = ps_input_location(*reg) {
                        wgsl.push_str(&format!(
                            "  @location({}) {}: vec4<f32>,\n",
                            loc,
                            reg_var_name(*reg)
                        ));
                    }
                }
                wgsl.push_str("};\n\n");
                wgsl.push_str("@fragment\nfn fs_main(input: PsInput) -> @location(0) vec4<f32> {\n");
            } else {
                wgsl.push_str("@fragment\nfn fs_main() -> @location(0) vec4<f32> {\n");
            }
            for i in 0..ir.temp_count {
                wgsl.push_str(&format!("  var r{}: vec4<f32> = vec4<f32>(0.0);\n", i));
            }
            // Load inputs.
            if has_inputs {
                for reg in &inputs_by_reg {
                    wgsl.push_str(&format!(
                        "  let {}: vec4<f32> = input.{};\n",
                        reg_var_name(*reg),
                        reg_var_name(*reg)
                    ));
                }
            }
            wgsl.push_str("  var oC0: vec4<f32> = vec4<f32>(0.0);\n\n");

            for inst in &ir.ops {
                emit_inst(&mut wgsl, inst);
            }
            wgsl.push_str("  return oC0;\n}\n");

            WgslOutput {
                wgsl,
                entry_point: "fs_main",
                bind_group_layout: BindGroupLayout { sampler_bindings },
            }
        }
    }
}

fn emit_inst(wgsl: &mut String, inst: &Instruction) {
    match inst.op {
        Op::Nop => {}
        Op::End => {}
        Op::Mov => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let dst_name = reg_var_name(dst.reg);
            let src_expr = src_expr(&src0);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, src_expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, src_expr));
            }
        }
        Op::Add | Op::Mul => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let src1 = inst.src[1];
            let op = if inst.op == Op::Add { "+" } else { "*" };
            let dst_name = reg_var_name(dst.reg);
            let expr = format!("({} {} {})", src_expr(&src0), op, src_expr(&src1));
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
        Op::Min | Op::Max => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let src1 = inst.src[1];
            let func = if inst.op == Op::Min { "min" } else { "max" };
            let dst_name = reg_var_name(dst.reg);
            let expr = format!("{}({}, {})", func, src_expr(&src0), src_expr(&src1));
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
        Op::Mad => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0]);
            let b = src_expr(&inst.src[1]);
            let c = src_expr(&inst.src[2]);
            let expr = format!("fma({}, {}, {})", a, b, c);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
        Op::Cmp => {
            let dst = inst.dst.unwrap();
            let cond = src_expr(&inst.src[0]);
            let a = src_expr(&inst.src[1]);
            let b = src_expr(&inst.src[2]);
            // Per-component compare: if cond >= 0 then a else b.
            let expr = format!(
                "select({}, {}, ({} >= vec4<f32>(0.0)))",
                b, a, cond
            );
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
        Op::Slt | Op::Sge => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0]);
            let b = src_expr(&inst.src[1]);
            let op = if inst.op == Op::Slt { "<" } else { ">=" };
            let expr = format!(
                "select(vec4<f32>(0.0), vec4<f32>(1.0), ({} {} {}))",
                a, op, b
            );
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
        Op::Dp3 | Op::Dp4 => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0]);
            let b = src_expr(&inst.src[1]);
            let expr = if inst.op == Op::Dp3 {
                format!("vec4<f32>(dot(({}).xyz, ({}).xyz))", a, b)
            } else {
                format!("vec4<f32>(dot({}, {}))", a, b)
            };
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
        Op::Texld => {
            let dst = inst.dst.unwrap();
            let coord = inst.src[0];
            let s = inst.sampler.unwrap();
            let dst_name = reg_var_name(dst.reg);
            let coord_expr = src_expr(&coord);
            let sample = format!("textureSample(tex{}, samp{}, ({}).xy)", s, s, coord_expr);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, sample, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, sample));
            }
        }
        Op::Rcp => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0]);
            let expr = format!("(vec4<f32>(1.0) / {})", src0);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
        Op::Rsq => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0]);
            let expr = format!("inverseSqrt({})", src0);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
        Op::Frc => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0]);
            let expr = format!("fract({})", src0);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                wgsl.push_str(&format!("  {}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                wgsl.push_str(&format!("  {} = {};\n", dst_name, expr));
            }
        }
    }
}

fn src_expr(src: &Src) -> String {
    match src.reg.file {
        RegisterFile::Const => format!(
            "constants.c[{}u]{}",
            src.reg.index,
            swizzle_suffix(src.swizzle)
        ),
        _ => format!("{}{}", reg_var_name(src.reg), swizzle_suffix(src.swizzle)),
    }
}

#[derive(Debug, Clone)]
pub struct CachedShader {
    pub hash: Hash,
    pub ir: ShaderIr,
    pub wgsl: WgslOutput,
}

#[derive(Default)]
pub struct ShaderCache {
    map: HashMap<Hash, CachedShader>,
}

impl ShaderCache {
    pub fn get_or_translate(&mut self, bytes: &[u8]) -> Result<&CachedShader, ShaderError> {
        let hash = blake3::hash(bytes);
        if !self.map.contains_key(&hash) {
            let program = parse(bytes)?;
            let ir = to_ir(&program);
            let wgsl = generate_wgsl(&ir);
            self.map.insert(hash, CachedShader { hash, ir, wgsl });
        }
        Ok(self.map.get(&hash).unwrap())
    }
}
