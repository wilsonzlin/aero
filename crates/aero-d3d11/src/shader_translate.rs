use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use crate::binding_model::{
    BINDING_BASE_CBUFFER, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, BINDING_BASE_UAV,
    D3D11_MAX_CONSTANT_BUFFER_SLOTS, MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS, MAX_UAV_SLOTS,
};
use crate::signature::{DxbcSignature, DxbcSignatureParameter, ShaderSignatures};
use crate::sm4::opcode::opcode_name;
use crate::sm4::ShaderStage;
use crate::sm4_ir::{
    BufferKind, CmpOp, CmpType, OperandModifier, RegFile, RegisterRef, Sm4Decl, Sm4Inst, Sm4Module,
    SrcKind, Swizzle, WriteMask,
};
use crate::DxbcFile;
use aero_dxbc::RdefChunk;

#[derive(Debug, Clone)]
pub struct ShaderTranslation {
    pub wgsl: String,
    pub stage: ShaderStage,
    pub reflection: ShaderReflection,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderReflection {
    pub inputs: Vec<IoParam>,
    pub outputs: Vec<IoParam>,
    pub bindings: Vec<Binding>,
    /// Parsed `RDEF` reflection, if present in the DXBC container.
    pub rdef: Option<RdefChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoParam {
    pub semantic_name: String,
    pub semantic_index: u32,
    pub register: u32,
    pub location: Option<u32>,
    pub builtin: Option<Builtin>,
    /// Bitmask of components (x=1, y=2, z=4, w=8).
    pub mask: u8,
    /// Signature stream index (used by geometry shaders / stream-out).
    ///
    /// For VS/PS this is typically 0.
    pub stream: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Position,
    VertexIndex,
    InstanceIndex,
    PrimitiveIndex,
    GsInstanceIndex,
    FrontFacing,
    GlobalInvocationId,
    LocalInvocationId,
    WorkgroupId,
    LocalInvocationIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub group: u32,
    pub binding: u32,
    pub visibility: wgpu::ShaderStages,
    pub kind: BindingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    ConstantBuffer {
        slot: u32,
        reg_count: u32,
    },
    Texture2D {
        slot: u32,
    },
    /// A `t#` SRV that is backed by a buffer (e.g. `ByteAddressBuffer`, `StructuredBuffer`).
    ///
    /// In WGSL this maps to a `var<storage, read>` binding.
    SrvBuffer {
        slot: u32,
    },
    Sampler {
        slot: u32,
    },
    /// A `u#` UAV that is backed by a buffer (e.g. `RWByteAddressBuffer`, `RWStructuredBuffer`).
    ///
    /// In WGSL this maps to a `var<storage, read_write>` binding.
    UavBuffer {
        slot: u32,
    },
}

#[derive(Debug)]
pub enum ShaderTranslateError {
    UnsupportedStage(ShaderStage),
    MissingSignature(&'static str),
    SignatureMissingRegister {
        io: &'static str,
        register: u32,
    },
    ConflictingSignatureRegister {
        io: &'static str,
        register: u32,
        first: String,
        second: String,
    },
    ConflictingVertexInputPacking {
        register: u32,
        component: char,
        first: String,
        second: String,
    },
    InvalidSignatureMask {
        io: &'static str,
        semantic: String,
        register: u32,
        mask: u8,
    },
    UnsupportedInstruction {
        inst_index: usize,
        opcode: String,
    },
    UnsupportedWriteMask {
        inst_index: usize,
        opcode: &'static str,
        mask: WriteMask,
    },
    MalformedControlFlow {
        inst_index: usize,
        expected: String,
        found: String,
    },
    ResourceSlotOutOfRange {
        kind: &'static str,
        slot: u32,
        max: u32,
    },
    UnsupportedSystemValue {
        stage: ShaderStage,
        semantic: String,
        reason: &'static str,
    },
    PixelShaderMissingColorOutputs,
    MissingThreadGroupSize,
    InvalidThreadGroupSize {
        x: u32,
        y: u32,
        z: u32,
    },
}

impl fmt::Display for ShaderTranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShaderTranslateError::UnsupportedStage(stage) => write!(f, "unsupported shader stage {stage:?}"),
            ShaderTranslateError::MissingSignature(name) => write!(f, "DXBC missing required signature chunk {name}"),
            ShaderTranslateError::MissingThreadGroupSize => write!(f, "compute shader is missing required thread-group size declaration"),
            ShaderTranslateError::SignatureMissingRegister { io, register } => {
                write!(f, "{io} signature does not declare register {register}")
            }
            ShaderTranslateError::ConflictingSignatureRegister {
                io,
                register,
                first,
                second,
            } => write!(
                f,
                "{io} signature declares multiple parameters for register {register} with incompatible system-value mappings ({first} vs {second})"
            ),
            ShaderTranslateError::ConflictingVertexInputPacking {
                register,
                component,
                first,
                second,
            } => write!(
                f,
                "vertex shader input signature packs multiple semantics into v{register}.{component} ({first} vs {second})"
            ),
            ShaderTranslateError::InvalidSignatureMask {
                io,
                semantic,
                register,
                mask,
            } => write!(
                f,
                "{io} signature parameter {semantic} (r{register}) has unsupported component mask 0x{mask:02x}"
            ),
            ShaderTranslateError::UnsupportedInstruction { inst_index, opcode } => {
                write!(f, "unsupported SM4/5 instruction {opcode} at index {inst_index}")
            }
            ShaderTranslateError::UnsupportedWriteMask {
                inst_index,
                opcode,
                mask,
            } => write!(
                f,
                "unsupported write mask {mask:?} for {opcode} at instruction index {inst_index}"
            ),
            ShaderTranslateError::MalformedControlFlow {
                inst_index,
                expected,
                found,
            } => write!(
                f,
                "malformed control flow at instruction index {inst_index} (expected {expected}, found {found})"
            ),
            ShaderTranslateError::ResourceSlotOutOfRange { kind, slot, max } => {
                match *kind {
                    "cbuffer" => write!(
                        f,
                        "cbuffer slot {slot} is out of range for D3D11 (max {max}); D3D11 exposes constant buffers b0..b{max} ({} slots per stage) which map to @binding({BINDING_BASE_CBUFFER} + slot)",
                        max.saturating_add(1),
                    ),
                    "texture" => write!(
                        f,
                        "t# SRV slot {slot} is out of range (max {max}); t# slots map to @binding({BINDING_BASE_TEXTURE} + slot) and must stay below the sampler base @binding({BINDING_BASE_SAMPLER})"
                    ),
                    "srv_buffer" => write!(
                        f,
                        "t# SRV buffer slot {slot} is out of range (max {max}); t# slots map to @binding({BINDING_BASE_TEXTURE} + slot) and must stay below the sampler base @binding({BINDING_BASE_SAMPLER})"
                    ),
                    "uav" => write!(
                        f,
                        "uav slot {slot} is out of range (max {max}); u# slots map to @binding({BINDING_BASE_UAV} + slot)"
                    ),
                    "uav_buffer" => write!(
                        f,
                        "u# UAV buffer slot {slot} is out of range (max {max}); u# slots map to @binding({BINDING_BASE_UAV} + slot)"
                    ),
                    "sampler" => write!(
                        f,
                        "sampler slot {slot} is out of range (max {max}); s# slots map to @binding({BINDING_BASE_SAMPLER} + slot)"
                    ),
                    _ => write!(f, "{kind} slot {slot} is out of range (max {max})"),
                }
            }
            ShaderTranslateError::UnsupportedSystemValue {
                stage,
                semantic,
                reason,
            } => write!(
                f,
                "unsupported system-value input {semantic} in {stage:?} shader: {reason}"
            ),
            ShaderTranslateError::PixelShaderMissingColorOutputs => {
                write!(
                    f,
                    "pixel shader output signature declares no render-target outputs (SV_Target0..7 or legacy COLOR0..7)"
                )
            }
            ShaderTranslateError::InvalidThreadGroupSize { x, y, z } => write!(
                f,
                "compute shader has invalid thread group size ({x}, {y}, {z})"
            ),
        }
    }
}

impl std::error::Error for ShaderTranslateError {}

/// Translates a decoded SM4/SM5 module into WGSL.
///
/// The `dxbc` input is currently used only for diagnostics / future expansion
/// (e.g. `RDEF`-driven sizing). The translation is driven by `module` and
/// `signatures`.
pub fn translate_sm4_module_to_wgsl(
    dxbc: &DxbcFile<'_>,
    module: &Sm4Module,
    signatures: &ShaderSignatures,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    let rdef = dxbc.get_rdef().and_then(|res| res.ok());
    match (module.stage, rdef) {
        (ShaderStage::Vertex, rdef) => {
            let isgn = signatures
                .isgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("ISGN"))?;
            let osgn = signatures
                .osgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("OSGN"))?;
            translate_vs(module, isgn, osgn, rdef)
        }
        (ShaderStage::Pixel, rdef) => {
            let isgn = signatures
                .isgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("ISGN"))?;
            let osgn = signatures
                .osgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("OSGN"))?;
            translate_ps(module, isgn, osgn, rdef)
        }
        (ShaderStage::Compute, rdef) => translate_cs(module, rdef),
        (other, _rdef) => Err(ShaderTranslateError::UnsupportedStage(other)),
    }
}

/// Scans a decoded SM4/SM5 module and produces bind group layout entries for the
/// module's declared shader stage.
///
/// Note: The binding model reserves:
/// - `@group(2)` for compute resources
/// - `@group(3)` for D3D11 extended stage resources (GS/HS/DS; executed via compute emulation)
pub fn reflect_resource_bindings(module: &Sm4Module) -> Result<Vec<Binding>, ShaderTranslateError> {
    Ok(scan_resources(module, None)?.bindings(module.stage))
}

fn translate_cs(
    module: &Sm4Module,
    rdef: Option<RdefChunk>,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    let io = build_cs_io_maps(module);
    let resources = scan_resources(module, rdef.as_ref())?;

    let mut thread_group_size: Option<(u32, u32, u32)> = None;
    for decl in &module.decls {
        if let Sm4Decl::ThreadGroupSize { x, y, z } = decl {
            let next = (*x, *y, *z);
            if let Some(prev) = thread_group_size {
                if prev != next {
                    return Err(ShaderTranslateError::InvalidThreadGroupSize {
                        x: next.0,
                        y: next.1,
                        z: next.2,
                    });
                }
            } else {
                thread_group_size = Some(next);
            }
        }
    }
    let (x, y, z) = thread_group_size.ok_or(ShaderTranslateError::MissingThreadGroupSize)?;
    if x == 0 || y == 0 || z == 0 {
        return Err(ShaderTranslateError::InvalidThreadGroupSize { x, y, z });
    }

    let used_regs = scan_used_input_registers(module);
    let mut reflected_inputs = Vec::<IoParam>::new();
    for reg in &used_regs {
        let Some(siv) = io.cs_inputs.get(reg) else {
            continue;
        };

        let mask = module
            .decls
            .iter()
            .find_map(|decl| match decl {
                Sm4Decl::InputSiv {
                    reg: decl_reg,
                    mask,
                    sys_value,
                } if decl_reg == reg && compute_sys_value_from_d3d_name(*sys_value) == Some(*siv) => {
                    Some(mask.0)
                }
                _ => None,
            })
            .unwrap_or_else(|| match siv {
                ComputeSysValue::GroupIndex => 0b0001,
                _ => 0b0111,
            });

        reflected_inputs.push(IoParam {
            semantic_name: siv.d3d_semantic_name().to_owned(),
            semantic_index: 0,
            register: *reg,
            location: None,
            builtin: Some(siv.builtin()),
            mask,
            stream: 0,
        });
    }

    let reflection = ShaderReflection {
        inputs: reflected_inputs,
        outputs: Vec::new(),
        bindings: resources.bindings(ShaderStage::Compute),
        rdef,
    };

    let used_sivs = scan_used_compute_sivs(module, &io);

    let mut w = WgslWriter::new();
    // The Aero D3D11 binding model uses stage-scoped bind groups, so compute-stage resources live
    // in `@group(2)`.
    resources.emit_decls(&mut w, ShaderStage::Compute)?;

    if !used_sivs.is_empty() {
        w.line("struct CsIn {");
        w.indent();
        for siv in &used_sivs {
            w.line(&format!(
                "@builtin({}) {}: {},",
                siv.wgsl_builtin(),
                siv.wgsl_field_name(),
                siv.wgsl_ty()
            ));
        }
        w.dedent();
        w.line("};");
        w.line("");
    }

    w.line(&format!("@compute @workgroup_size({x}, {y}, {z})"));
    if used_sivs.is_empty() {
        w.line("fn cs_main() {");
    } else {
        w.line("fn cs_main(input: CsIn) {");
    }
    w.indent();
    w.line("");
    emit_temp_and_output_decls(&mut w, module, &io)?;

    let ctx = EmitCtx {
        stage: ShaderStage::Compute,
        io: &io,
        resources: &resources,
    };
    emit_instructions(&mut w, module, &ctx)?;

    w.dedent();
    w.line("}");

    Ok(ShaderTranslation {
        wgsl: w.finish(),
        stage: ShaderStage::Compute,
        reflection,
    })
}

fn translate_vs(
    module: &Sm4Module,
    isgn: &DxbcSignature,
    osgn: &DxbcSignature,
    rdef: Option<RdefChunk>,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    let io = build_io_maps(module, isgn, osgn)?;
    let resources = scan_resources(module, rdef.as_ref())?;

    let reflection = ShaderReflection {
        inputs: io.inputs_reflection(),
        outputs: io.outputs_reflection_vertex(),
        bindings: resources.bindings(ShaderStage::Vertex),
        rdef,
    };

    let mut w = WgslWriter::new();

    resources.emit_decls(&mut w, ShaderStage::Vertex)?;
    io.emit_vs_structs(&mut w)?;

    w.line("@vertex");
    let vs_has_inputs = !io.inputs.is_empty();
    if vs_has_inputs {
        w.line("fn vs_main(input: VsIn) -> VsOut {");
    } else {
        w.line("fn vs_main() -> VsOut {");
    }
    w.indent();
    w.line("var out: VsOut;");
    w.line("");
    emit_temp_and_output_decls(&mut w, module, &io)?;

    let ctx = EmitCtx {
        stage: ShaderStage::Vertex,
        io: &io,
        resources: &resources,
    };
    emit_instructions(&mut w, module, &ctx)?;

    w.line("");
    io.emit_vs_return(&mut w)?;
    w.dedent();
    w.line("}");

    Ok(ShaderTranslation {
        wgsl: w.finish(),
        stage: ShaderStage::Vertex,
        reflection,
    })
}

fn translate_ps(
    module: &Sm4Module,
    isgn: &DxbcSignature,
    osgn: &DxbcSignature,
    rdef: Option<RdefChunk>,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    let io = build_io_maps(module, isgn, osgn)?;
    let resources = scan_resources(module, rdef.as_ref())?;

    let mut ps_targets: Vec<&ParamInfo> = io
        .outputs
        .values()
        .filter(|p| p.sys_value == Some(D3D_NAME_TARGET))
        .collect();
    ps_targets.sort_by_key(|p| p.param.semantic_index);
    let ps_has_depth_output = io.ps_sv_depth_register.is_some();
    if ps_targets.is_empty() && !ps_has_depth_output {
        return Err(ShaderTranslateError::PixelShaderMissingColorOutputs);
    }

    let reflection = ShaderReflection {
        inputs: io.inputs_reflection(),
        outputs: io.outputs_reflection_pixel(),
        bindings: resources.bindings(ShaderStage::Pixel),
        rdef,
    };

    let mut w = WgslWriter::new();

    resources.emit_decls(&mut w, ShaderStage::Pixel)?;
    let ps_has_inputs = !io.inputs.is_empty() || io.ps_position_register.is_some();
    if ps_has_inputs {
        io.emit_ps_structs(&mut w)?;
    }

    w.line("struct PsOut {");
    w.indent();
    for p in &ps_targets {
        let location = p.param.semantic_index;
        w.line(&format!("@location({location}) target{location}: vec4<f32>,"));
    }
    if ps_has_depth_output {
        w.line("@builtin(frag_depth) depth: f32,");
    }
    w.dedent();
    w.line("};");
    w.line("");

    w.line("@fragment");
    if ps_has_inputs {
        w.line("fn fs_main(input: PsIn) -> PsOut {");
    } else {
        w.line("fn fs_main() -> PsOut {");
    }
    w.indent();
    w.line("");
    emit_temp_and_output_decls(&mut w, module, &io)?;

    let ctx = EmitCtx {
        stage: ShaderStage::Pixel,
        io: &io,
        resources: &resources,
    };
    emit_instructions(&mut w, module, &ctx)?;

    w.line("");
    io.emit_ps_return(&mut w)?;
    w.dedent();
    w.line("}");

    Ok(ShaderTranslation {
        wgsl: w.finish(),
        stage: ShaderStage::Pixel,
        reflection,
    })
}

fn build_io_maps(
    module: &Sm4Module,
    isgn: &DxbcSignature,
    osgn: &DxbcSignature,
) -> Result<IoMaps, ShaderTranslateError> {
    let mut input_sivs = BTreeMap::<u32, u32>::new();
    let mut output_sivs = BTreeMap::<u32, u32>::new();
    for decl in &module.decls {
        match decl {
            Sm4Decl::InputSiv { reg, sys_value, .. } => {
                input_sivs.insert(*reg, *sys_value);
            }
            Sm4Decl::OutputSiv { reg, sys_value, .. } => {
                output_sivs.insert(*reg, *sys_value);
            }
            _ => {}
        }
    }

    let mut inputs: BTreeMap<u32, ParamInfo> = BTreeMap::new();
    let mut vs_input_fields = Vec::<VsInputField>::new();
    let mut vs_input_fields_by_register = BTreeMap::<u32, Vec<usize>>::new();
    let mut next_vs_location = 0u32;
    for p in &isgn.parameters {
        let sys_value = resolve_sys_value_type(p, &input_sivs);
        let info = ParamInfo::from_sig_param("input", p, sys_value)?;
        if module.stage == ShaderStage::Vertex && info.builtin.is_none() {
            let location = next_vs_location;
            next_vs_location += 1;
            let idx = vs_input_fields.len();
            vs_input_fields.push(VsInputField {
                location,
                info: info.clone(),
            });
            vs_input_fields_by_register
                .entry(p.register)
                .or_default()
                .push(idx);
        }
        match inputs.get_mut(&p.register) {
            Some(existing) => {
                if existing.sys_value != info.sys_value || existing.builtin != info.builtin {
                    return Err(ShaderTranslateError::ConflictingSignatureRegister {
                        io: "input",
                        register: p.register,
                        first: format!(
                            "{}{}",
                            existing.param.semantic_name, existing.param.semantic_index
                        ),
                        second: format!(
                            "{}{}",
                            info.param.semantic_name, info.param.semantic_index
                        ),
                    });
                }

                let mut merged_param = existing.param.clone();
                merged_param.mask |= info.param.mask;
                merged_param.read_write_mask |= info.param.read_write_mask;
                *existing = ParamInfo::from_sig_param("input", &merged_param, existing.sys_value)?;
            }
            None => {
                inputs.insert(p.register, info);
            }
        }
    }

    let mut outputs: BTreeMap<u32, ParamInfo> = BTreeMap::new();
    for p in &osgn.parameters {
        let sys_value = resolve_sys_value_type(p, &output_sivs);
        let info = ParamInfo::from_sig_param("output", p, sys_value)?;
        match outputs.get_mut(&p.register) {
            Some(existing) => {
                if existing.sys_value != info.sys_value || existing.builtin != info.builtin {
                    return Err(ShaderTranslateError::ConflictingSignatureRegister {
                        io: "output",
                        register: p.register,
                        first: format!(
                            "{}{}",
                            existing.param.semantic_name, existing.param.semantic_index
                        ),
                        second: format!(
                            "{}{}",
                            info.param.semantic_name, info.param.semantic_index
                        ),
                    });
                }

                let mut merged_param = existing.param.clone();
                merged_param.mask |= info.param.mask;
                merged_param.read_write_mask |= info.param.read_write_mask;
                *existing = ParamInfo::from_sig_param("output", &merged_param, existing.sys_value)?;
            }
            None => {
                outputs.insert(p.register, info);
            }
        }
    }

    let mut vs_position_reg = outputs
        .values()
        .find(|p| p.sys_value == Some(D3D_NAME_POSITION))
        .map(|p| p.param.register);
    if vs_position_reg.is_none() && module.stage == ShaderStage::Vertex {
        // Some compilers still emit legacy `POSITION` semantics for the vertex shader's position
        // output even when the signature's `system_value_type` is unset.
        vs_position_reg = osgn
            .parameters
            .iter()
            .find(|p| p.semantic_index == 0 && p.semantic_name.eq_ignore_ascii_case("POSITION"))
            .map(|p| p.register);
        if let Some(reg) = vs_position_reg {
            if let Some(p) = outputs.get_mut(&reg) {
                p.sys_value = Some(D3D_NAME_POSITION);
                p.builtin = Some(Builtin::Position);
            }
        }
    }

    let mut ps_position_reg = None;
    if module.stage == ShaderStage::Pixel {
        ps_position_reg = inputs
            .values()
            .find(|p| p.sys_value == Some(D3D_NAME_POSITION))
            .map(|p| p.param.register);
        if ps_position_reg.is_none() {
            // Legacy `POSITION` can also be used for pixel shader `SV_Position` inputs.
            ps_position_reg = isgn
                .parameters
                .iter()
                .find(|p| p.semantic_index == 0 && p.semantic_name.eq_ignore_ascii_case("POSITION"))
                .map(|p| p.register);
            if let Some(reg) = ps_position_reg {
                if let Some(p) = inputs.get_mut(&reg) {
                    p.sys_value = Some(D3D_NAME_POSITION);
                    p.builtin = Some(Builtin::Position);
                }
            }
        }
    }

    if module.stage == ShaderStage::Pixel {
        // Legacy `COLOR` semantics (SM4-era) can stand in for `SV_Target` on pixel shader
        // outputs. Treat `COLORn` as `SV_Targetn` by updating the resolved system value type.
        for p in &osgn.parameters {
            if !p.semantic_name.eq_ignore_ascii_case("COLOR") {
                continue;
            }
            if let Some(info) = outputs.get_mut(&p.register) {
                if info.sys_value.is_none() {
                    info.sys_value = Some(D3D_NAME_TARGET);
                }
            }
        }
    }

    let mut ps_sv_depth_reg = None;
    if module.stage == ShaderStage::Pixel {
        ps_sv_depth_reg = outputs
            .values()
            .find(|p| {
                matches!(
                    p.sys_value,
                    Some(D3D_NAME_DEPTH)
                        | Some(D3D_NAME_DEPTH_GREATER_EQUAL)
                        | Some(D3D_NAME_DEPTH_LESS_EQUAL)
                )
            })
            .map(|p| p.param.register);
        if ps_sv_depth_reg.is_none() {
            // Some toolchains emit the legacy `DEPTH` semantic with `system_value_type` unset.
            ps_sv_depth_reg = osgn
                .parameters
                .iter()
                .find(|p| p.semantic_index == 0 && p.semantic_name.eq_ignore_ascii_case("DEPTH"))
                .map(|p| p.register);
            if let Some(reg) = ps_sv_depth_reg {
                if let Some(p) = outputs.get_mut(&reg) {
                    p.sys_value = Some(D3D_NAME_DEPTH);
                }
            }
        }
    }

    let vs_vertex_id_reg = (module.stage == ShaderStage::Vertex)
        .then(|| {
            inputs
                .values()
                .find(|p| p.sys_value == Some(D3D_NAME_VERTEX_ID))
                .map(|p| p.param.register)
        })
        .flatten();
    let vs_instance_id_reg = (module.stage == ShaderStage::Vertex)
        .then(|| {
            inputs
                .values()
                .find(|p| p.sys_value == Some(D3D_NAME_INSTANCE_ID))
                .map(|p| p.param.register)
        })
        .flatten();
    let ps_primitive_id_reg = (module.stage == ShaderStage::Pixel)
        .then(|| {
            inputs
                .values()
                .find(|p| p.sys_value == Some(D3D_NAME_PRIMITIVE_ID))
                .map(|p| p.param.register)
        })
        .flatten();
    let ps_front_facing_reg = (module.stage == ShaderStage::Pixel)
        .then(|| {
            inputs
                .values()
                .find(|p| p.sys_value == Some(D3D_NAME_IS_FRONT_FACE))
                .map(|p| p.param.register)
        })
        .flatten();

    Ok(IoMaps {
        inputs,
        outputs,
        vs_input_fields,
        vs_input_fields_by_register,
        vs_position_register: vs_position_reg,
        ps_position_register: ps_position_reg,
        ps_primitive_id_register: ps_primitive_id_reg,
        ps_sv_depth_register: ps_sv_depth_reg,
        vs_vertex_id_register: vs_vertex_id_reg,
        vs_instance_id_register: vs_instance_id_reg,
        ps_front_facing_register: ps_front_facing_reg,
        cs_inputs: BTreeMap::new(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ComputeSysValue {
    DispatchThreadId,
    GroupThreadId,
    GroupId,
    GroupIndex,
}

impl ComputeSysValue {
    fn wgsl_builtin(self) -> &'static str {
        match self {
            ComputeSysValue::DispatchThreadId => "global_invocation_id",
            ComputeSysValue::GroupThreadId => "local_invocation_id",
            ComputeSysValue::GroupId => "workgroup_id",
            ComputeSysValue::GroupIndex => "local_invocation_index",
        }
    }

    fn d3d_semantic_name(self) -> &'static str {
        match self {
            ComputeSysValue::DispatchThreadId => "SV_DispatchThreadID",
            ComputeSysValue::GroupThreadId => "SV_GroupThreadID",
            ComputeSysValue::GroupId => "SV_GroupID",
            ComputeSysValue::GroupIndex => "SV_GroupIndex",
        }
    }

    fn builtin(self) -> Builtin {
        match self {
            ComputeSysValue::DispatchThreadId => Builtin::GlobalInvocationId,
            ComputeSysValue::GroupThreadId => Builtin::LocalInvocationId,
            ComputeSysValue::GroupId => Builtin::WorkgroupId,
            ComputeSysValue::GroupIndex => Builtin::LocalInvocationIndex,
        }
    }

    fn wgsl_field_name(self) -> &'static str {
        // Use the WGSL builtin name as the field name for easy/greppable lowering.
        self.wgsl_builtin()
    }

    fn wgsl_ty(self) -> &'static str {
        match self {
            ComputeSysValue::DispatchThreadId
            | ComputeSysValue::GroupThreadId
            | ComputeSysValue::GroupId => "vec3<u32>",
            ComputeSysValue::GroupIndex => "u32",
        }
    }

    fn expand_to_vec4(self) -> String {
        match self {
            ComputeSysValue::DispatchThreadId
            | ComputeSysValue::GroupThreadId
            | ComputeSysValue::GroupId => {
                let field = format!("input.{}", self.wgsl_field_name());
                format!(
                    "vec4<f32>(bitcast<f32>({field}.x), bitcast<f32>({field}.y), bitcast<f32>({field}.z), 1.0)"
                )
            }
            ComputeSysValue::GroupIndex => {
                let field = format!("input.{}", self.wgsl_field_name());
                format!("vec4<f32>(bitcast<f32>({field}), 0.0, 0.0, 1.0)")
            }
        }
    }
}

fn compute_sys_value_from_d3d_name(name: u32) -> Option<ComputeSysValue> {
    match name {
        D3D_NAME_DISPATCH_THREAD_ID => Some(ComputeSysValue::DispatchThreadId),
        D3D_NAME_GROUP_THREAD_ID => Some(ComputeSysValue::GroupThreadId),
        D3D_NAME_GROUP_ID => Some(ComputeSysValue::GroupId),
        D3D_NAME_GROUP_INDEX => Some(ComputeSysValue::GroupIndex),
        _ => None,
    }
}

fn build_cs_io_maps(module: &Sm4Module) -> IoMaps {
    let mut cs_inputs = BTreeMap::<u32, ComputeSysValue>::new();
    for decl in &module.decls {
        if let Sm4Decl::InputSiv { reg, sys_value, .. } = decl {
            if let Some(siv) = compute_sys_value_from_d3d_name(*sys_value) {
                cs_inputs.insert(*reg, siv);
            }
        }
    }

    IoMaps {
        inputs: BTreeMap::new(),
        outputs: BTreeMap::new(),
        vs_input_fields: Vec::new(),
        vs_input_fields_by_register: BTreeMap::new(),
        vs_position_register: None,
        ps_position_register: None,
        ps_primitive_id_register: None,
        ps_sv_depth_register: None,
        vs_vertex_id_register: None,
        vs_instance_id_register: None,
        ps_front_facing_register: None,
        cs_inputs,
    }
}

fn scan_used_input_registers(module: &Sm4Module) -> BTreeSet<u32> {
    let mut inputs = BTreeSet::<u32>::new();
    for inst in &module.instructions {
        let mut scan_reg = |reg: RegisterRef| {
            if reg.file == RegFile::Input {
                inputs.insert(reg.index);
            }
        };
        match inst {
            Sm4Inst::If { cond, .. } => scan_src_regs(cond, &mut scan_reg),
            Sm4Inst::Else | Sm4Inst::EndIf | Sm4Inst::Loop | Sm4Inst::EndLoop => {}
            Sm4Inst::Mov { dst: _, src } | Sm4Inst::Utof { dst: _, src } => {
                scan_src_regs(src, &mut scan_reg)
            }
            Sm4Inst::Movc { dst: _, cond, a, b } => {
                scan_src_regs(cond, &mut scan_reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::And { dst: _, a, b }
            | Sm4Inst::Add { dst: _, a, b }
            | Sm4Inst::IAddC {
                dst_sum: _,
                dst_carry: _,
                a,
                b,
            }
            | Sm4Inst::UAddC {
                dst_sum: _,
                dst_carry: _,
                a,
                b,
            }
            | Sm4Inst::ISubC {
                dst_diff: _,
                dst_borrow: _,
                a,
                b,
            }
            | Sm4Inst::USubB {
                dst_diff: _,
                dst_borrow: _,
                a,
                b,
            }
            | Sm4Inst::Mul { dst: _, a, b }
            | Sm4Inst::Dp3 { dst: _, a, b }
            | Sm4Inst::Dp4 { dst: _, a, b }
            | Sm4Inst::Min { dst: _, a, b }
            | Sm4Inst::Max { dst: _, a, b }
            | Sm4Inst::IMin { dst: _, a, b }
            | Sm4Inst::IMax { dst: _, a, b }
            | Sm4Inst::UMin { dst: _, a, b }
            | Sm4Inst::UMax { dst: _, a, b }
            | Sm4Inst::UDiv {
                dst_quot: _,
                dst_rem: _,
                a,
                b,
            }
            | Sm4Inst::IDiv {
                dst_quot: _,
                dst_rem: _,
                a,
                b,
            } => {
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::Mad { dst: _, a, b, c } => {
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
                scan_src_regs(c, &mut scan_reg);
            }
            Sm4Inst::Bfi {
                width,
                offset,
                insert,
                base,
                ..
            } => {
                scan_src_regs(width, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
                scan_src_regs(insert, &mut scan_reg);
                scan_src_regs(base, &mut scan_reg);
            }
            Sm4Inst::Ubfe {
                width,
                offset,
                src,
                ..
            }
            | Sm4Inst::Ibfe {
                width,
                offset,
                src,
                ..
            } => {
                scan_src_regs(width, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
                scan_src_regs(src, &mut scan_reg);
            }
            Sm4Inst::Rcp { dst: _, src }
            | Sm4Inst::Rsq { dst: _, src }
            | Sm4Inst::IAbs { dst: _, src }
            | Sm4Inst::Bfrev { dst: _, src }
            | Sm4Inst::CountBits { dst: _, src }
            | Sm4Inst::FirstbitHi { dst: _, src }
            | Sm4Inst::FirstbitLo { dst: _, src }
            | Sm4Inst::FirstbitShi { dst: _, src }
            | Sm4Inst::INeg { dst: _, src } => {
                scan_src_regs(src, &mut scan_reg)
            }
            Sm4Inst::Cmp { a, b, .. } => {
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::Sample {
                dst: _,
                coord,
                texture: _,
                sampler: _,
            } => scan_src_regs(coord, &mut scan_reg),
            Sm4Inst::SampleL {
                dst: _,
                coord,
                texture: _,
                sampler: _,
                lod,
            } => {
                scan_src_regs(coord, &mut scan_reg);
                scan_src_regs(lod, &mut scan_reg);
            }
            Sm4Inst::Ld {
                dst: _,
                coord,
                texture: _,
                lod,
            } => {
                scan_src_regs(coord, &mut scan_reg);
                scan_src_regs(lod, &mut scan_reg);
            }
            Sm4Inst::LdRaw { addr, .. } => scan_src_regs(addr, &mut scan_reg),
            Sm4Inst::StoreRaw { addr, value, .. } => {
                scan_src_regs(addr, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::LdStructured { index, offset, .. } => {
                scan_src_regs(index, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
            }
            Sm4Inst::StoreStructured {
                index,
                offset,
                value,
                ..
            } => {
                scan_src_regs(index, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::WorkgroupBarrier => {}
            Sm4Inst::Switch { selector } => scan_src_regs(selector, &mut scan_reg),
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch | Sm4Inst::Break => {}
            Sm4Inst::Emit { .. }
            | Sm4Inst::Cut { .. }
            | Sm4Inst::BufInfoRaw { .. }
            | Sm4Inst::BufInfoStructured { .. }
            | Sm4Inst::Unknown { .. }
            | Sm4Inst::Ret => {}
        }
    }
    inputs
}

fn scan_used_compute_sivs(module: &Sm4Module, io: &IoMaps) -> BTreeSet<ComputeSysValue> {
    let used_regs = scan_used_input_registers(module);
    let mut out = BTreeSet::<ComputeSysValue>::new();
    for reg in used_regs {
        if let Some(siv) = io.cs_inputs.get(&reg) {
            out.insert(*siv);
        }
    }
    out
}

#[derive(Debug, Clone)]
struct ParamInfo {
    param: DxbcSignatureParameter,
    sys_value: Option<u32>,
    builtin: Option<Builtin>,
    wgsl_ty: &'static str,
    component_count: usize,
    components: [u8; 4],
}

impl ParamInfo {
    fn from_sig_param(
        io: &'static str,
        param: &DxbcSignatureParameter,
        sys_value: Option<u32>,
    ) -> Result<Self, ShaderTranslateError> {
        let builtin = sys_value.and_then(builtin_from_d3d_name);

        let mask = param.mask & 0xF;
        let mut comps = [0u8; 4];
        let mut count = 0usize;
        for (idx, bit) in [1u8, 2, 4, 8].into_iter().enumerate() {
            if (mask & bit) != 0 {
                comps[count] = idx as u8;
                count += 1;
            }
        }
        if count == 0 || count > 4 {
            return Err(ShaderTranslateError::InvalidSignatureMask {
                io,
                semantic: format!("{}{}", param.semantic_name, param.semantic_index),
                register: param.register,
                mask,
            });
        }

        let wgsl_ty = match count {
            1 => "f32",
            2 => "vec2<f32>",
            3 => "vec3<f32>",
            4 => "vec4<f32>",
            _ => unreachable!(),
        };

        // WGSL builtin inputs have fixed types that do not match the signature component count.
        // We still keep the vec4<f32> internal register model and expand scalar builtins into x
        // with D3D default fill (0,0,0,1).
        let (wgsl_ty, component_count, components) = match builtin {
            Some(Builtin::VertexIndex)
            | Some(Builtin::InstanceIndex)
            | Some(Builtin::PrimitiveIndex)
            | Some(Builtin::GsInstanceIndex)
            | Some(Builtin::LocalInvocationIndex) => ("u32", 1, [0, 0, 0, 0]),
            Some(Builtin::FrontFacing) => ("bool", 1, [0, 0, 0, 0]),
            Some(Builtin::GlobalInvocationId)
            | Some(Builtin::LocalInvocationId)
            | Some(Builtin::WorkgroupId) => ("vec3<u32>", 3, [0, 1, 2, 0]),
            _ => (wgsl_ty, count, comps),
        };

        Ok(Self {
            param: param.clone(),
            wgsl_ty,
            component_count,
            components,
            sys_value,
            builtin,
        })
    }

    fn field_name(&self, prefix: char) -> String {
        format!("{prefix}{}", self.param.register)
    }
}

#[derive(Debug, Clone)]
struct VsInputField {
    location: u32,
    info: ParamInfo,
}

impl VsInputField {
    fn field_name(&self) -> String {
        format!("a{}", self.location)
    }
}

#[derive(Debug, Clone, Default)]
struct IoMaps {
    inputs: BTreeMap<u32, ParamInfo>,
    outputs: BTreeMap<u32, ParamInfo>,
    /// Vertex-stage input fields (one per signature parameter, with unique WGSL locations).
    ///
    /// This differs from `inputs`, which is keyed by D3D register index; `ISGN` signatures can pack
    /// multiple semantics into a single register, but WGSL vertex attributes must have unique
    /// `@location`s.
    vs_input_fields: Vec<VsInputField>,
    /// Mapping from D3D input register index (v#) to the corresponding entries in `vs_input_fields`.
    ///
    /// Only populated for vertex shaders.
    vs_input_fields_by_register: BTreeMap<u32, Vec<usize>>,
    vs_position_register: Option<u32>,
    ps_position_register: Option<u32>,
    ps_primitive_id_register: Option<u32>,
    ps_sv_depth_register: Option<u32>,
    vs_vertex_id_register: Option<u32>,
    vs_instance_id_register: Option<u32>,
    ps_front_facing_register: Option<u32>,
    cs_inputs: BTreeMap<u32, ComputeSysValue>,
}

impl IoMaps {
    fn inputs_reflection(&self) -> Vec<IoParam> {
        self.inputs
            .values()
            .map(|p| IoParam {
                semantic_name: p.param.semantic_name.clone(),
                semantic_index: p.param.semantic_index,
                register: p.param.register,
                location: p.builtin.is_none().then_some(p.param.register),
                builtin: p.builtin,
                mask: p.param.mask,
                stream: p.param.stream,
            })
            .collect()
    }

    fn outputs_reflection_vertex(&self) -> Vec<IoParam> {
        self.outputs
            .values()
            .map(|p| IoParam {
                semantic_name: p.param.semantic_name.clone(),
                semantic_index: p.param.semantic_index,
                register: p.param.register,
                location: p.builtin.is_none().then_some(p.param.register),
                builtin: p.builtin,
                mask: p.param.mask,
                stream: p.param.stream,
            })
            .collect()
    }

    fn outputs_reflection_pixel(&self) -> Vec<IoParam> {
        self.outputs
            .values()
            .map(|p| {
                let is_target = p.sys_value == Some(D3D_NAME_TARGET);
                IoParam {
                    semantic_name: p.param.semantic_name.clone(),
                    semantic_index: p.param.semantic_index,
                    register: p.param.register,
                    location: is_target.then_some(p.param.semantic_index),
                    builtin: None,
                    mask: p.param.mask,
                    stream: p.param.stream,
                }
            })
            .collect()
    }

    fn emit_vs_structs(&self, w: &mut WgslWriter) -> Result<(), ShaderTranslateError> {
        let has_inputs = self.vs_vertex_id_register.is_some()
            || self.vs_instance_id_register.is_some()
            || !self.vs_input_fields.is_empty();
        if has_inputs {
            w.line("struct VsIn {");
            w.indent();

            if self.vs_vertex_id_register.is_some() {
                w.line("@builtin(vertex_index) vertex_id: u32,");
            }
            if self.vs_instance_id_register.is_some() {
                w.line("@builtin(instance_index) instance_id: u32,");
            }

            for f in &self.vs_input_fields {
                w.line(&format!(
                    "@location({}) {}: {},",
                    f.location,
                    f.field_name(),
                    f.info.wgsl_ty
                ));
            }
            w.dedent();
            w.line("};");
            w.line("");
        }

        let pos_reg = self
            .vs_position_register
            .ok_or(ShaderTranslateError::MissingSignature(
                "vertex output SV_Position",
            ))?;

        w.line("struct VsOut {");
        w.indent();
        w.line("@builtin(position) pos: vec4<f32>,");
        for p in self.outputs.values() {
            if p.param.register == pos_reg {
                continue;
            }
            w.line(&format!(
                "@location({}) {}: {},",
                p.param.register,
                p.field_name('o'),
                // WGSL requires exact stage-interface type matching. D3D signatures can legally
                // differ in their component masks between stages (e.g. VS exports float3 while PS
                // declares float4). To avoid vec2/vec3/vec4 mismatches at the same @location, we
                // normalize all user varyings to `vec4<f32>` in both VS outputs and PS inputs.
                "vec4<f32>"
            ));
        }
        w.dedent();
        w.line("};");
        w.line("");

        Ok(())
    }

    fn emit_ps_structs(&self, w: &mut WgslWriter) -> Result<(), ShaderTranslateError> {
        w.line("struct PsIn {");
        w.indent();
        if let Some(_pos_reg) = self.ps_position_register {
            w.line("@builtin(position) pos: vec4<f32>,");
        }
        if self.ps_primitive_id_register.is_some() {
            w.line("@builtin(primitive_index) primitive_id: u32,");
        }
        if self.ps_front_facing_register.is_some() {
            w.line("@builtin(front_facing) front_facing: bool,");
        }
        for p in self.inputs.values() {
            if Some(p.param.register) == self.ps_position_register
                || Some(p.param.register) == self.ps_primitive_id_register
                || Some(p.param.register) == self.ps_front_facing_register
            {
                continue;
            }
            w.line(&format!(
                "@location({}) {}: {},",
                p.param.register,
                p.field_name('v'),
                // See `emit_vs_structs` for why we use vec4 here.
                "vec4<f32>"
            ));
        }
        w.dedent();
        w.line("};");
        w.line("");
        Ok(())
    }

    fn emit_vs_return(&self, w: &mut WgslWriter) -> Result<(), ShaderTranslateError> {
        let pos_reg = self
            .vs_position_register
            .ok_or(ShaderTranslateError::MissingSignature(
                "vertex output SV_Position",
            ))?;
        let pos_param =
            self.outputs
                .get(&pos_reg)
                .ok_or(ShaderTranslateError::SignatureMissingRegister {
                    io: "output",
                    register: pos_reg,
                })?;
        let pos_expr = apply_sig_mask_to_vec4(&format!("o{pos_reg}"), pos_param.param.mask);
        w.line(&format!("out.pos = {pos_expr};"));
        for p in self.outputs.values() {
            if p.param.register == pos_reg {
                continue;
            }
            // `VsOut` always declares varyings as vec4, so apply the signature mask to fill
            // missing components with D3D defaults.
            let src = apply_sig_mask_to_vec4(&format!("o{}", p.param.register), p.param.mask);
            w.line(&format!("out.{} = {src};", p.field_name('o')));
        }
        w.line("return out;");
        Ok(())
    }

    fn emit_ps_return(&self, w: &mut WgslWriter) -> Result<(), ShaderTranslateError> {
        let mut targets: Vec<&ParamInfo> = self
            .outputs
            .values()
            .filter(|p| p.sys_value == Some(D3D_NAME_TARGET))
            .collect();
        targets.sort_by_key(|p| p.param.semantic_index);

        let has_depth = self.ps_sv_depth_register.is_some();
        if targets.is_empty() && !has_depth {
            return Err(ShaderTranslateError::PixelShaderMissingColorOutputs);
        }

        w.line("var out: PsOut;");
        for p in targets {
            let location = p.param.semantic_index;
            let reg = p.param.register;
            let expr = apply_sig_mask_to_vec4(&format!("o{reg}"), p.param.mask);
            w.line(&format!("out.target{location} = {expr};"));
        }
        if let Some(depth_reg) = self.ps_sv_depth_register {
            let depth_param = self.outputs.get(&depth_reg).ok_or(
                ShaderTranslateError::SignatureMissingRegister {
                    io: "output",
                    register: depth_reg,
                },
            )?;
            let depth_expr = apply_sig_mask_to_scalar(&format!("o{depth_reg}"), depth_param.param.mask);
            w.line(&format!("out.depth = {depth_expr};"));
        }
        w.line("return out;");
        Ok(())
    }

    fn read_input_vec4(
        &self,
        stage: ShaderStage,
        reg: u32,
    ) -> Result<String, ShaderTranslateError> {
        match stage {
            ShaderStage::Vertex => {
                if Some(reg) == self.vs_vertex_id_register {
                    let p = self.inputs.get(&reg).ok_or(
                        ShaderTranslateError::SignatureMissingRegister {
                            io: "input",
                            register: reg,
                        },
                    )?;
                    // `SV_VertexID` / `@builtin(vertex_index)` is an integer system value in D3D.
                    //
                    // Our internal register model is `vec4<f32>` (untyped 32-bit lanes). Integer
                    // operations therefore expect the *raw integer bit pattern* to be carried in a
                    // float lane, not the float numeric value of the integer.
                    //
                    // Converting via `f32(input.vertex_id)` would lose the original bits (e.g.
                    // breaking `and`/`xor`/shifts once those ops are implemented). Preserve the bits
                    // with a bitcast instead.
                    return Ok(expand_to_vec4("bitcast<f32>(input.vertex_id)", p));
                }
                if Some(reg) == self.vs_instance_id_register {
                    let p = self.inputs.get(&reg).ok_or(
                        ShaderTranslateError::SignatureMissingRegister {
                            io: "input",
                            register: reg,
                        },
                    )?;
                    // See `vs_vertex_id_register` above for why this is a `bitcast<f32>` rather
                    // than a numeric `f32(...)` conversion.
                    return Ok(expand_to_vec4("bitcast<f32>(input.instance_id)", p));
                }
                let p = self.inputs.get(&reg).ok_or(ShaderTranslateError::SignatureMissingRegister {
                    io: "input",
                    register: reg,
                })?;
                if p.sys_value.is_some() {
                    return Err(ShaderTranslateError::UnsupportedSystemValue {
                        stage,
                        semantic: format!("{}{}", p.param.semantic_name, p.param.semantic_index),
                        reason: "emulation not implemented",
                    });
                }
                let field_indices = self.vs_input_fields_by_register.get(&reg).ok_or(
                    ShaderTranslateError::SignatureMissingRegister {
                        io: "input",
                        register: reg,
                    },
                )?;

                if field_indices.len() == 1 {
                    let f = &self.vs_input_fields[field_indices[0]];
                    return Ok(expand_to_vec4(
                        &format!("input.{}", f.field_name()),
                        &f.info,
                    ));
                }

                // D3D signatures can pack multiple semantics into the same input register. WebGPU
                // vertex attributes can't express this directly, so we assign each signature
                // parameter a unique `@location` and reconstruct the packed register here.
                let mut comps: [Option<(String, String)>; 4] = [None, None, None, None];
                for &idx in field_indices {
                    let f = &self.vs_input_fields[idx];
                    let semantic = format!(
                        "{}{}",
                        f.info.param.semantic_name, f.info.param.semantic_index
                    );
                    let base = format!("input.{}", f.field_name());

                    for (dst_comp, bit) in [(0usize, 1u8), (1, 2), (2, 4), (3, 8)] {
                        if (f.info.param.mask & bit) == 0 {
                            continue;
                        }

                        let lane = f
                            .info
                            .components
                            .iter()
                            .take(f.info.component_count)
                            .position(|&c| c as usize == dst_comp)
                            .expect("signature component should exist");
                        let lane_char = ['x', 'y', 'z', 'w'][lane];
                        let expr = if f.info.component_count == 1 {
                            base.clone()
                        } else {
                            format!("({base}).{lane_char}")
                        };

                        if let Some((_, prev_semantic)) = &comps[dst_comp] {
                            return Err(ShaderTranslateError::ConflictingVertexInputPacking {
                                register: reg,
                                component: ['x', 'y', 'z', 'w'][dst_comp],
                                first: prev_semantic.clone(),
                                second: semantic,
                            });
                        }
                        comps[dst_comp] = Some((expr, semantic.clone()));
                    }
                }

                let mut out = [String::new(), String::new(), String::new(), String::new()];
                for i in 0..4 {
                    out[i] = comps[i]
                        .take()
                        .map(|(expr, _semantic)| expr)
                        .unwrap_or_else(|| {
                            if i == 3 {
                                "1.0".to_owned()
                            } else {
                                "0.0".to_owned()
                            }
                        });
                }
                Ok(format!(
                    "vec4<f32>({}, {}, {}, {})",
                    out[0], out[1], out[2], out[3]
                ))
            }
            ShaderStage::Pixel => {
                if Some(reg) == self.ps_position_register {
                    let p = self.inputs.get(&reg).ok_or(
                        ShaderTranslateError::SignatureMissingRegister {
                            io: "input",
                            register: reg,
                        },
                    )?;
                    return Ok(apply_sig_mask_to_vec4("input.pos", p.param.mask));
                }
                if Some(reg) == self.ps_primitive_id_register {
                    let p = self.inputs.get(&reg).ok_or(
                        ShaderTranslateError::SignatureMissingRegister {
                            io: "input",
                            register: reg,
                        },
                    )?;
                    // `SV_PrimitiveID` / `@builtin(primitive_index)` is an integer system value.
                    //
                    // As with `SV_VertexID`/`SV_InstanceID`, preserve raw integer bits in the
                    // internal untyped `vec4<f32>` register model so integer/bitwise ops can be
                    // translated correctly. Numeric conversion (e.g. to write it to a float render
                    // target) should be expressed via an explicit `utof` instruction.
                    return Ok(expand_to_vec4("bitcast<f32>(input.primitive_id)", p));
                }
                if Some(reg) == self.ps_front_facing_register {
                    let _p = self.inputs.get(&reg).ok_or(
                        ShaderTranslateError::SignatureMissingRegister {
                            io: "input",
                            register: reg,
                        },
                    )?;
                    // `SV_IsFrontFace` is a bool in HLSL, but in SM4/5 the register file is untyped
                    // and most compilers represent boolean values as an all-bits-set mask (0xffffffff)
                    // for true and 0 for false. Integer/bitwise code expects this representation.
                    //
                    // Store the mask bits in our internal `vec4<f32>` register model by
                    // bitcasting the u32 mask into an f32 and splatting across lanes.
                    return Ok(
                        "vec4<f32>(bitcast<f32>(select(0u, 0xffffffffu, input.front_facing)))"
                            .to_owned(),
                    );
                }
                let p = self.inputs.get(&reg).ok_or(
                    ShaderTranslateError::SignatureMissingRegister {
                        io: "input",
                        register: reg,
                    },
                )?;
                if p.sys_value.is_some() {
                    return Err(ShaderTranslateError::UnsupportedSystemValue {
                        stage,
                        semantic: format!("{}{}", p.param.semantic_name, p.param.semantic_index),
                        reason: "emulation not implemented",
                    });
                }
                // `PsIn` always declares varyings as vec4, so apply the signature mask to fill
                // missing components with D3D defaults.
                Ok(apply_sig_mask_to_vec4(
                    &format!("input.{}", p.field_name('v')),
                    p.param.mask,
                ))
            }
            ShaderStage::Compute => {
                let siv = self.cs_inputs.get(&reg).ok_or(
                    ShaderTranslateError::SignatureMissingRegister {
                        io: "input",
                        register: reg,
                    },
                )?;
                Ok(siv.expand_to_vec4())
            }
            _ => Err(ShaderTranslateError::UnsupportedStage(stage)),
        }
    }
}

const D3D_NAME_POSITION: u32 = 1;
const D3D_NAME_VERTEX_ID: u32 = 6;
const D3D_NAME_PRIMITIVE_ID: u32 = 7;
const D3D_NAME_INSTANCE_ID: u32 = 8;
const D3D_NAME_IS_FRONT_FACE: u32 = 9;
// D3D10+ geometry-shader instancing builtin (`SV_GSInstanceID`). This value is shared between
// signature `system_value_type` and `dcl_input_siv` declaration encodings.
const D3D_NAME_GS_INSTANCE_ID: u32 = 11;
const D3D_NAME_DISPATCH_THREAD_ID: u32 = 20;
const D3D_NAME_GROUP_ID: u32 = 21;
const D3D_NAME_GROUP_INDEX: u32 = 22;
const D3D_NAME_GROUP_THREAD_ID: u32 = 23;
const D3D_NAME_TARGET: u32 = 64;
const D3D_NAME_DEPTH: u32 = 65;
const D3D_NAME_DEPTH_GREATER_EQUAL: u32 = 67;
const D3D_NAME_DEPTH_LESS_EQUAL: u32 = 68;

fn builtin_from_d3d_name(name: u32) -> Option<Builtin> {
    match name {
        D3D_NAME_POSITION => Some(Builtin::Position),
        D3D_NAME_VERTEX_ID => Some(Builtin::VertexIndex),
        D3D_NAME_PRIMITIVE_ID => Some(Builtin::PrimitiveIndex),
        D3D_NAME_INSTANCE_ID => Some(Builtin::InstanceIndex),
        D3D_NAME_GS_INSTANCE_ID => Some(Builtin::GsInstanceIndex),
        D3D_NAME_IS_FRONT_FACE => Some(Builtin::FrontFacing),
        D3D_NAME_DISPATCH_THREAD_ID => Some(Builtin::GlobalInvocationId),
        D3D_NAME_GROUP_THREAD_ID => Some(Builtin::LocalInvocationId),
        D3D_NAME_GROUP_ID => Some(Builtin::WorkgroupId),
        D3D_NAME_GROUP_INDEX => Some(Builtin::LocalInvocationIndex),
        _ => None,
    }
}

fn resolve_sys_value_type(
    param: &DxbcSignatureParameter,
    decl_sivs: &BTreeMap<u32, u32>,
) -> Option<u32> {
    if let Some(&sys_value) = decl_sivs.get(&param.register) {
        return Some(sys_value);
    }
    if param.system_value_type != 0 {
        return Some(param.system_value_type);
    }
    semantic_to_d3d_name(&param.semantic_name)
}

fn semantic_to_d3d_name(name: &str) -> Option<u32> {
    if is_sv_position(name) {
        return Some(D3D_NAME_POSITION);
    }
    if is_sv_primitive_id(name) {
        return Some(D3D_NAME_PRIMITIVE_ID);
    }
    if is_sv_vertex_id(name) {
        return Some(D3D_NAME_VERTEX_ID);
    }
    if is_sv_instance_id(name) {
        return Some(D3D_NAME_INSTANCE_ID);
    }
    if is_sv_gs_instance_id(name) {
        return Some(D3D_NAME_GS_INSTANCE_ID);
    }
    if is_sv_is_front_face(name) {
        return Some(D3D_NAME_IS_FRONT_FACE);
    }
    if is_sv_target(name) {
        return Some(D3D_NAME_TARGET);
    }
    if is_sv_dispatch_thread_id(name) {
        return Some(D3D_NAME_DISPATCH_THREAD_ID);
    }
    if is_sv_group_thread_id(name) {
        return Some(D3D_NAME_GROUP_THREAD_ID);
    }
    if is_sv_group_id(name) {
        return Some(D3D_NAME_GROUP_ID);
    }
    if is_sv_group_index(name) {
        return Some(D3D_NAME_GROUP_INDEX);
    }
    if is_sv_depth(name) {
        return Some(D3D_NAME_DEPTH);
    }
    if is_sv_depth_greater_equal(name) {
        return Some(D3D_NAME_DEPTH_GREATER_EQUAL);
    }
    if is_sv_depth_less_equal(name) {
        return Some(D3D_NAME_DEPTH_LESS_EQUAL);
    }
    None
}

fn is_sv_position(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_Position") || name.eq_ignore_ascii_case("SV_POSITION")
}

fn is_sv_vertex_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_VertexID") || name.eq_ignore_ascii_case("SV_VERTEXID")
}

fn is_sv_primitive_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_PrimitiveID") || name.eq_ignore_ascii_case("SV_PRIMITIVEID")
}

fn is_sv_instance_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_InstanceID") || name.eq_ignore_ascii_case("SV_INSTANCEID")
}

fn is_sv_gs_instance_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_GSInstanceID") || name.eq_ignore_ascii_case("SV_GSINSTANCEID")
}

fn is_sv_is_front_face(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_IsFrontFace") || name.eq_ignore_ascii_case("SV_ISFRONTFACE")
}

fn is_sv_target(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_Target") || name.eq_ignore_ascii_case("SV_TARGET")
}

fn is_sv_dispatch_thread_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_DispatchThreadID")
        || name.eq_ignore_ascii_case("SV_DISPATCHTHREADID")
}

fn is_sv_group_thread_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_GroupThreadID") || name.eq_ignore_ascii_case("SV_GROUPTHREADID")
}

fn is_sv_group_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_GroupID") || name.eq_ignore_ascii_case("SV_GROUPID")
}

fn is_sv_group_index(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_GroupIndex") || name.eq_ignore_ascii_case("SV_GROUPINDEX")
}

fn is_sv_depth(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_Depth")
        || name.eq_ignore_ascii_case("SV_DEPTH")
        || name.eq_ignore_ascii_case("DEPTH")
}

fn is_sv_depth_greater_equal(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_DepthGreaterEqual") || name.eq_ignore_ascii_case("SV_DEPTHGREATEREQUAL")
}

fn is_sv_depth_less_equal(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_DepthLessEqual") || name.eq_ignore_ascii_case("SV_DEPTHLESSEQUAL")
}

fn expand_to_vec4(expr: &str, p: &ParamInfo) -> String {
    // D3D input assembler fills missing components with (0,0,0,1). We apply the
    // same rule when expanding signature-typed values into internal vec4
    // registers.
    let mut src = Vec::<String>::with_capacity(p.component_count);
    match p.component_count {
        1 => src.push(expr.to_owned()),
        2 => {
            src.push(format!("{expr}.x"));
            src.push(format!("{expr}.y"));
        }
        3 => {
            src.push(format!("{expr}.x"));
            src.push(format!("{expr}.y"));
            src.push(format!("{expr}.z"));
        }
        4 => {
            return expr.to_owned();
        }
        _ => {}
    }

    let mut out = [String::new(), String::new(), String::new(), String::new()];
    let mut next = 0usize;
    for (dst_comp, out_comp) in out.iter_mut().enumerate() {
        let want = p
            .components
            .iter()
            .take(p.component_count)
            .any(|&c| c as usize == dst_comp);
        if want {
            *out_comp = src[next].clone();
            next += 1;
        } else if dst_comp == 3 {
            *out_comp = "1.0".to_owned();
        } else {
            *out_comp = "0.0".to_owned();
        }
    }

    format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3])
}

fn apply_sig_mask_to_vec4(expr: &str, mask: u8) -> String {
    let mask = mask & 0xF;
    if mask == 0xF {
        return expr.to_owned();
    }

    let mut out = [String::new(), String::new(), String::new(), String::new()];
    for (dst_comp, bit) in [1u8, 2, 4, 8].into_iter().enumerate() {
        if (mask & bit) != 0 {
            out[dst_comp] = format!("{expr}.{}", component_char(dst_comp as u8));
        } else if dst_comp == 3 {
            out[dst_comp] = "1.0".to_owned();
        } else {
            out[dst_comp] = "0.0".to_owned();
        }
    }

    format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3])
}

fn apply_sig_mask_to_scalar(expr: &str, mask: u8) -> String {
    let mask = mask & 0xF;
    let comp = if (mask & 1) != 0 {
        'x'
    } else if (mask & 2) != 0 {
        'y'
    } else if (mask & 4) != 0 {
        'z'
    } else if (mask & 8) != 0 {
        'w'
    } else {
        // `ParamInfo::from_sig_param` rejects mask==0; fall back defensively anyway.
        'x'
    };
    format!("({expr}).{comp}")
}

fn component_char(c: u8) -> char {
    match c {
        0 => 'x',
        1 => 'y',
        2 => 'z',
        3 => 'w',
        _ => 'x',
    }
}

fn swizzle_suffix(swizzle: Swizzle) -> String {
    let mut s = String::with_capacity(4);
    for &c in &swizzle.0 {
        s.push(component_char(c));
    }
    s
}

fn apply_modifier(expr: String, modifier: OperandModifier) -> String {
    match modifier {
        OperandModifier::None => expr,
        OperandModifier::Neg => format!("-({expr})"),
        OperandModifier::Abs => format!("abs({expr})"),
        OperandModifier::AbsNeg => format!("-abs({expr})"),
    }
}

fn apply_modifier_u32(expr: String, modifier: OperandModifier) -> String {
    match modifier {
        OperandModifier::None => expr,
        // WGSL does not support unary negation on `u32`. DXBC operand modifiers are defined over
        // raw 32-bit values, so model `-x` as wrapping subtraction from 0.
        //
        // Note: This helper is currently used for `vec4<u32>` operands.
        OperandModifier::Neg | OperandModifier::AbsNeg => format!("vec4<u32>(0u) - ({expr})"),
        // `abs` is a no-op for unsigned integers.
        OperandModifier::Abs => expr,
    }
}
#[derive(Debug, Clone)]
struct ResourceUsage {
    cbuffers: BTreeMap<u32, u32>,
    textures: BTreeSet<u32>,
    srv_buffers: BTreeSet<u32>,
    samplers: BTreeSet<u32>,
    uav_buffers: BTreeSet<u32>,
}

impl ResourceUsage {
    fn stage_bind_group(stage: ShaderStage) -> u32 {
        match stage {
            ShaderStage::Vertex => 0,
            ShaderStage::Pixel => 1,
            ShaderStage::Compute => 2,
            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => 3,
            _ => 0,
        }
    }

    fn bindings(&self, stage: ShaderStage) -> Vec<Binding> {
        let visibility = match stage {
            ShaderStage::Vertex => wgpu::ShaderStages::VERTEX,
            ShaderStage::Pixel => wgpu::ShaderStages::FRAGMENT,
            ShaderStage::Compute => wgpu::ShaderStages::COMPUTE,
            // Geometry shaders are executed via a compute emulation path, but still use their own
            // stage-scoped bind group (`@group(3)`) so they don't collide with true compute shader
            // resources (`@group(2)`).
            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => wgpu::ShaderStages::COMPUTE,
            _ => wgpu::ShaderStages::empty(),
        };
        let group = Self::stage_bind_group(stage);

        let mut out = Vec::new();
        for (&slot, &reg_count) in &self.cbuffers {
            out.push(Binding {
                group,
                binding: BINDING_BASE_CBUFFER + slot,
                visibility,
                kind: BindingKind::ConstantBuffer { slot, reg_count },
            });
        }
        for &slot in &self.textures {
            out.push(Binding {
                group,
                binding: BINDING_BASE_TEXTURE + slot,
                visibility,
                kind: BindingKind::Texture2D { slot },
            });
        }
        for &slot in &self.srv_buffers {
            out.push(Binding {
                group,
                binding: BINDING_BASE_TEXTURE + slot,
                visibility,
                kind: BindingKind::SrvBuffer { slot },
            });
        }
        for &slot in &self.samplers {
            out.push(Binding {
                group,
                binding: BINDING_BASE_SAMPLER + slot,
                visibility,
                kind: BindingKind::Sampler { slot },
            });
        }
        for &slot in &self.uav_buffers {
            out.push(Binding {
                group,
                binding: BINDING_BASE_UAV + slot,
                visibility,
                kind: BindingKind::UavBuffer { slot },
            });
        }
        out
    }

    fn emit_decls(
        &self,
        w: &mut WgslWriter,
        stage: ShaderStage,
    ) -> Result<(), ShaderTranslateError> {
        // Bindings are stage-scoped; these declarations are emitted inside the per-stage WGSL
        // module, so `group` is derived from the shader stage.
        let group = Self::stage_bind_group(stage);
        for (&slot, &reg_count) in &self.cbuffers {
            w.line(&format!(
                "struct Cb{slot} {{ regs: array<vec4<u32>, {reg_count}> }};"
            ));
            w.line(&format!(
                "@group({group}) @binding({}) var<uniform> cb{slot}: Cb{slot};",
                BINDING_BASE_CBUFFER + slot
            ));
            w.line("");
        }
        for &slot in &self.textures {
            w.line(&format!(
                "@group({group}) @binding({}) var t{slot}: texture_2d<f32>;",
                BINDING_BASE_TEXTURE + slot
            ));
        }
        if !self.textures.is_empty() || !self.srv_buffers.is_empty() {
            w.line("");
        }
        if !self.srv_buffers.is_empty() || !self.uav_buffers.is_empty() {
            // WGSL requires storage buffers to have a `struct` as the top-level type; arrays
            // cannot be declared directly as a `var<storage>`.
            w.line("struct AeroStorageBufferU32 { data: array<u32> };");
            w.line("");
        }
        for &slot in &self.srv_buffers {
            w.line(&format!(
                "@group({group}) @binding({}) var<storage, read> t{slot}: AeroStorageBufferU32;",
                BINDING_BASE_TEXTURE + slot
            ));
        }
        if !self.srv_buffers.is_empty() {
            w.line("");
        }
        for &slot in &self.samplers {
            w.line(&format!(
                "@group({group}) @binding({}) var s{slot}: sampler;",
                BINDING_BASE_SAMPLER + slot
            ));
        }
        if !self.samplers.is_empty() {
            w.line("");
        }
        for &slot in &self.uav_buffers {
            w.line(&format!(
                "@group({group}) @binding({}) var<storage, read_write> u{slot}: AeroStorageBufferU32;",
                BINDING_BASE_UAV + slot
            ));
        }
        if !self.uav_buffers.is_empty() {
            w.line("");
        }
        Ok(())
    }
}

fn scan_resources(
    module: &Sm4Module,
    rdef: Option<&RdefChunk>,
) -> Result<ResourceUsage, ShaderTranslateError> {
    fn validate_slot(
        kind: &'static str,
        slot: u32,
        max_slots: u32,
    ) -> Result<(), ShaderTranslateError> {
        if slot >= max_slots {
            return Err(ShaderTranslateError::ResourceSlotOutOfRange {
                kind,
                slot,
                max: max_slots.saturating_sub(1),
            });
        }
        Ok(())
    }
    let mut cbuffers: BTreeMap<u32, u32> = BTreeMap::new();
    let mut textures = BTreeSet::new();
    let mut srv_buffers = BTreeSet::new();
    let mut samplers = BTreeSet::new();
    let mut uav_buffers = BTreeSet::new();
    let mut declared_cbuffer_sizes: BTreeMap<u32, u32> = BTreeMap::new();

    for decl in &module.decls {
        match decl {
            Sm4Decl::ConstantBuffer { slot, reg_count } => {
                let entry = declared_cbuffer_sizes.entry(*slot).or_insert(0);
                *entry = (*entry).max(*reg_count);
            }
            Sm4Decl::ResourceBuffer { slot, .. } => {
                validate_slot("srv_buffer", *slot, MAX_TEXTURE_SLOTS)?;
                srv_buffers.insert(*slot);
            }
            Sm4Decl::UavBuffer { slot, .. } => {
                validate_slot("uav_buffer", *slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(*slot);
            }
            _ => {}
        }
    }

    for (_inst_index, inst) in module.instructions.iter().enumerate() {
        let mut scan_src = |src: &crate::sm4_ir::SrcOperand| -> Result<(), ShaderTranslateError> {
            if let SrcKind::ConstantBuffer { slot, reg } = src.kind {
                validate_slot("cbuffer", slot, D3D11_MAX_CONSTANT_BUFFER_SLOTS)?;
                let entry = cbuffers.entry(slot).or_insert(0);
                *entry = (*entry).max(reg + 1);
            }
            Ok(())
        };
        match inst {
            Sm4Inst::If { cond, .. } => scan_src(cond)?,
            Sm4Inst::Else | Sm4Inst::EndIf | Sm4Inst::Loop | Sm4Inst::EndLoop => {}
            Sm4Inst::Mov { dst: _, src } => scan_src(src)?,
            Sm4Inst::Utof { dst: _, src } => scan_src(src)?,
            Sm4Inst::Movc { dst: _, cond, a, b } => {
                scan_src(cond)?;
                scan_src(a)?;
                scan_src(b)?;
            }
            Sm4Inst::And { dst: _, a, b }
            | Sm4Inst::Add { dst: _, a, b }
            | Sm4Inst::IAddC {
                dst_sum: _,
                dst_carry: _,
                a,
                b,
            }
            | Sm4Inst::UAddC {
                dst_sum: _,
                dst_carry: _,
                a,
                b,
            }
            | Sm4Inst::ISubC {
                dst_diff: _,
                dst_borrow: _,
                a,
                b,
            }
            | Sm4Inst::USubB {
                dst_diff: _,
                dst_borrow: _,
                a,
                b,
            }
            | Sm4Inst::Mul { dst: _, a, b }
            | Sm4Inst::Dp3 { dst: _, a, b }
            | Sm4Inst::Dp4 { dst: _, a, b }
            | Sm4Inst::Min { dst: _, a, b }
            | Sm4Inst::Max { dst: _, a, b }
            | Sm4Inst::IMin { dst: _, a, b }
            | Sm4Inst::IMax { dst: _, a, b }
            | Sm4Inst::UMin { dst: _, a, b }
            | Sm4Inst::UMax { dst: _, a, b }
            | Sm4Inst::UDiv {
                dst_quot: _,
                dst_rem: _,
                a,
                b,
            }
            | Sm4Inst::IDiv {
                dst_quot: _,
                dst_rem: _,
                a,
                b,
            }
            | Sm4Inst::Cmp { dst: _, a, b, .. } => {
                scan_src(a)?;
                scan_src(b)?;
            }
            Sm4Inst::Mad { dst: _, a, b, c } => {
                scan_src(a)?;
                scan_src(b)?;
                scan_src(c)?;
            }
            Sm4Inst::Rcp { dst: _, src }
            | Sm4Inst::Rsq { dst: _, src }
            | Sm4Inst::IAbs { dst: _, src }
            | Sm4Inst::Bfrev { dst: _, src }
            | Sm4Inst::CountBits { dst: _, src }
            | Sm4Inst::FirstbitHi { dst: _, src }
            | Sm4Inst::FirstbitLo { dst: _, src }
            | Sm4Inst::FirstbitShi { dst: _, src }
            | Sm4Inst::INeg { dst: _, src } => scan_src(src)?,
            Sm4Inst::Bfi {
                dst: _,
                width,
                offset,
                insert,
                base,
            } => {
                scan_src(width)?;
                scan_src(offset)?;
                scan_src(insert)?;
                scan_src(base)?;
            }
            Sm4Inst::Ubfe {
                dst: _,
                width,
                offset,
                src,
            }
            | Sm4Inst::Ibfe {
                dst: _,
                width,
                offset,
                src,
            } => {
                scan_src(width)?;
                scan_src(offset)?;
                scan_src(src)?;
            }
            Sm4Inst::Sample {
                dst: _,
                coord,
                texture,
                sampler,
            } => {
                scan_src(coord)?;
                validate_slot("texture", texture.slot, MAX_TEXTURE_SLOTS)?;
                validate_slot("sampler", sampler.slot, MAX_SAMPLER_SLOTS)?;
                textures.insert(texture.slot);
                samplers.insert(sampler.slot);
            }
            Sm4Inst::SampleL {
                dst: _,
                coord,
                texture,
                sampler,
                lod,
            } => {
                scan_src(coord)?;
                scan_src(lod)?;
                validate_slot("texture", texture.slot, MAX_TEXTURE_SLOTS)?;
                validate_slot("sampler", sampler.slot, MAX_SAMPLER_SLOTS)?;
                textures.insert(texture.slot);
                samplers.insert(sampler.slot);
            }
            Sm4Inst::Ld {
                dst: _,
                coord,
                texture,
                lod,
            } => {
                scan_src(coord)?;
                scan_src(lod)?;
                validate_slot("texture", texture.slot, MAX_TEXTURE_SLOTS)?;
                textures.insert(texture.slot);
            }
            Sm4Inst::LdRaw {
                dst: _,
                addr,
                buffer,
            } => {
                scan_src(addr)?;
                validate_slot("srv_buffer", buffer.slot, MAX_TEXTURE_SLOTS)?;
                srv_buffers.insert(buffer.slot);
            }
            Sm4Inst::StoreRaw {
                uav, addr, value, ..
            } => {
                scan_src(addr)?;
                scan_src(value)?;
                validate_slot("uav_buffer", uav.slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(uav.slot);
            }
            Sm4Inst::LdStructured {
                dst: _,
                index,
                offset,
                buffer,
            } => {
                scan_src(index)?;
                scan_src(offset)?;
                validate_slot("srv_buffer", buffer.slot, MAX_TEXTURE_SLOTS)?;
                srv_buffers.insert(buffer.slot);
            }
            Sm4Inst::StoreStructured {
                uav,
                index,
                offset,
                value,
                mask: _,
            } => {
                scan_src(index)?;
                scan_src(offset)?;
                scan_src(value)?;
                validate_slot("uav_buffer", uav.slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(uav.slot);
            }
            Sm4Inst::WorkgroupBarrier => {}
            Sm4Inst::Switch { selector } => {
                scan_src(selector)?;
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch | Sm4Inst::Break => {}
            Sm4Inst::BufInfoRaw { dst: _, buffer }
            | Sm4Inst::BufInfoStructured { dst: _, buffer, .. } => {
                validate_slot("srv_buffer", buffer.slot, MAX_TEXTURE_SLOTS)?;
                srv_buffers.insert(buffer.slot);
            }
            Sm4Inst::Unknown { .. } => {}
            Sm4Inst::Emit { .. } | Sm4Inst::Cut { .. } => {}
            Sm4Inst::Ret => {}
        }
    }

    // If we know the declared register count for a constant buffer and it is referenced by the
    // instruction stream, ensure the emitted WGSL struct is large enough.
    for (slot, reg_count) in declared_cbuffer_sizes {
        if let Some(entry) = cbuffers.get_mut(&slot) {
            *entry = (*entry).max(reg_count);
        }
    }

    if let Some(rdef) = rdef {
        for cb in &rdef.constant_buffers {
            let Some(slot) = cb.bind_point else {
                continue;
            };
            let reg_count_u64 = (u64::from(cb.size) + 15) / 16;
            let reg_count = u32::try_from(reg_count_u64).unwrap_or(u32::MAX).max(1);
            let bind_count = cb.bind_count.unwrap_or(1);
            if bind_count <= 1 {
                if let Some(entry) = cbuffers.get_mut(&slot) {
                    *entry = (*entry).max(reg_count);
                }
                continue;
            }

            // Expand constant buffer arrays (e.g. `ConstantBuffer<T> cb[4] : register(b0)`).
            //
            // As with other resource arrays, we only expand when the shader already uses at least
            // one slot within the declared binding range.
            let end = slot.saturating_add(bind_count);
            let intersects = cbuffers.range(slot..end).next().is_some();
            if !intersects {
                continue;
            }
            let end = end.min(D3D11_MAX_CONSTANT_BUFFER_SLOTS);
            for s in slot..end {
                let entry = cbuffers.entry(s).or_insert(0);
                *entry = (*entry).max(reg_count);
            }
        }

        // Expand resource binding ranges (arrays) using the RDEF binding table.
        //
        // This is important for shaders that declare resource arrays and use dynamic indexing
        // (e.g. `Texture2D tex[4] : register(t0); tex[i].Sample(...)`). Instruction scanning alone
        // may only see a subset of the slots.
        //
        // We only expand ranges that intersect the resources we've already discovered via
        // declarations/instructions, to avoid needlessly bloating pipeline layouts for unused
        // declarations.
        fn set_intersects_range(set: &BTreeSet<u32>, start: u32, count: u32) -> bool {
            if count == 0 {
                return false;
            }
            let end = start.saturating_add(count);
            set.range(start..end).next().is_some()
        }
        fn expand_set_range(set: &mut BTreeSet<u32>, start: u32, count: u32, max_slots: u32) {
            if count == 0 {
                return;
            }
            let end = start.saturating_add(count).min(max_slots);
            for slot in start..end {
                set.insert(slot);
            }
        }

        for res in &rdef.bound_resources {
            match res.input_type {
                // D3D_SIT_TEXTURE
                2 => {
                    if set_intersects_range(&textures, res.bind_point, res.bind_count) {
                        expand_set_range(
                            &mut textures,
                            res.bind_point,
                            res.bind_count,
                            MAX_TEXTURE_SLOTS,
                        );
                    }
                }
                // D3D_SIT_SAMPLER
                3 => {
                    if set_intersects_range(&samplers, res.bind_point, res.bind_count) {
                        expand_set_range(
                            &mut samplers,
                            res.bind_point,
                            res.bind_count,
                            MAX_SAMPLER_SLOTS,
                        );
                    }
                }
                // D3D_SIT_STRUCTURED / D3D_SIT_BYTEADDRESS
                5 | 7 => {
                    if set_intersects_range(&srv_buffers, res.bind_point, res.bind_count) {
                        expand_set_range(
                            &mut srv_buffers,
                            res.bind_point,
                            res.bind_count,
                            MAX_TEXTURE_SLOTS,
                        );
                    }
                }
                // UAV buffer types (SM5):
                // - D3D_SIT_UAV_RWTYPED
                // - D3D_SIT_UAV_RWSTRUCTURED
                // - D3D_SIT_UAV_RWBYTEADDRESS
                // - D3D_SIT_UAV_APPEND_STRUCTURED
                // - D3D_SIT_UAV_CONSUME_STRUCTURED
                // - D3D_SIT_UAV_RWSTRUCTURED_WITH_COUNTER
                4 | 6 | 8 | 9 | 10 | 11 => {
                    if set_intersects_range(&uav_buffers, res.bind_point, res.bind_count) {
                        expand_set_range(
                            &mut uav_buffers,
                            res.bind_point,
                            res.bind_count,
                            MAX_UAV_SLOTS,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    Ok(ResourceUsage {
        cbuffers,
        textures,
        srv_buffers,
        samplers,
        uav_buffers,
    })
}

fn emit_temp_and_output_decls(
    w: &mut WgslWriter,
    module: &Sm4Module,
    io: &IoMaps,
) -> Result<(), ShaderTranslateError> {
    let mut temps = BTreeSet::<u32>::new();
    let mut outputs = BTreeSet::<u32>::new();

    for inst in &module.instructions {
        let mut scan_reg = |reg: RegisterRef| match reg.file {
            RegFile::Temp => {
                temps.insert(reg.index);
            }
            RegFile::Output => {
                outputs.insert(reg.index);
            }
            RegFile::OutputDepth => {
                // Depth output registers are mapped to a concrete `o#` register by the output
                // signature. Ensure the mapped register is declared if present.
                if let Some(depth_reg) = io.ps_sv_depth_register {
                    outputs.insert(depth_reg);
                } else {
                    outputs.insert(reg.index);
                }
            }
            RegFile::Input => {}
        };

        match inst {
            Sm4Inst::If { cond, .. } => {
                scan_src_regs(cond, &mut scan_reg);
            }
            Sm4Inst::Else | Sm4Inst::EndIf | Sm4Inst::Loop | Sm4Inst::EndLoop => {}
            Sm4Inst::Mov { dst, src } => {
                scan_reg(dst.reg);
                scan_src_regs(src, &mut scan_reg);
            }
            Sm4Inst::Utof { dst, src } => {
                scan_reg(dst.reg);
                scan_src_regs(src, &mut scan_reg);
            }
            Sm4Inst::Movc { dst, cond, a, b } => {
                scan_reg(dst.reg);
                scan_src_regs(cond, &mut scan_reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::IAddC {
                dst_sum,
                dst_carry,
                a,
                b,
            }
            | Sm4Inst::UAddC {
                dst_sum,
                dst_carry,
                a,
                b,
            } => {
                scan_reg(dst_sum.reg);
                scan_reg(dst_carry.reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::ISubC {
                dst_diff,
                dst_borrow,
                a,
                b,
            }
            | Sm4Inst::USubB {
                dst_diff,
                dst_borrow,
                a,
                b,
            } => {
                scan_reg(dst_diff.reg);
                scan_reg(dst_borrow.reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::And { dst, a, b }
            | Sm4Inst::Add { dst, a, b }
            | Sm4Inst::Mul { dst, a, b }
            | Sm4Inst::Dp3 { dst, a, b }
            | Sm4Inst::Dp4 { dst, a, b }
            | Sm4Inst::Min { dst, a, b }
            | Sm4Inst::Max { dst, a, b }
            | Sm4Inst::IMin { dst, a, b }
            | Sm4Inst::IMax { dst, a, b }
            | Sm4Inst::UMin { dst, a, b }
            | Sm4Inst::UMax { dst, a, b }
            | Sm4Inst::Cmp { dst, a, b, .. } => {
                scan_reg(dst.reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::UDiv {
                dst_quot,
                dst_rem,
                a,
                b,
            }
            | Sm4Inst::IDiv {
                dst_quot,
                dst_rem,
                a,
                b,
            } => {
                scan_reg(dst_quot.reg);
                scan_reg(dst_rem.reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::Mad { dst, a, b, c } => {
                scan_reg(dst.reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
                scan_src_regs(c, &mut scan_reg);
            }
            Sm4Inst::Rcp { dst, src }
            | Sm4Inst::Rsq { dst, src }
            | Sm4Inst::IAbs { dst, src }
            | Sm4Inst::Bfrev { dst, src }
            | Sm4Inst::CountBits { dst, src }
            | Sm4Inst::FirstbitHi { dst, src }
            | Sm4Inst::FirstbitLo { dst, src }
            | Sm4Inst::FirstbitShi { dst, src }
            | Sm4Inst::INeg { dst, src } => {
                scan_reg(dst.reg);
                scan_src_regs(src, &mut scan_reg);
            }
            Sm4Inst::Bfi {
                dst,
                width,
                offset,
                insert,
                base,
            } => {
                scan_reg(dst.reg);
                scan_src_regs(width, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
                scan_src_regs(insert, &mut scan_reg);
                scan_src_regs(base, &mut scan_reg);
            }
            Sm4Inst::Ubfe {
                dst,
                width,
                offset,
                src,
            }
            | Sm4Inst::Ibfe {
                dst,
                width,
                offset,
                src,
            } => {
                scan_reg(dst.reg);
                scan_src_regs(width, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
                scan_src_regs(src, &mut scan_reg);
            }
            Sm4Inst::Sample { dst, coord, .. } => {
                scan_reg(dst.reg);
                scan_src_regs(coord, &mut scan_reg);
            }
            Sm4Inst::SampleL {
                dst, coord, lod, ..
            } => {
                scan_reg(dst.reg);
                scan_src_regs(coord, &mut scan_reg);
                scan_src_regs(lod, &mut scan_reg);
            }
            Sm4Inst::Ld {
                dst, coord, lod, ..
            } => {
                scan_reg(dst.reg);
                scan_src_regs(coord, &mut scan_reg);
                scan_src_regs(lod, &mut scan_reg);
            }
            Sm4Inst::LdRaw { dst, addr, .. } => {
                scan_reg(dst.reg);
                scan_src_regs(addr, &mut scan_reg);
            }
            Sm4Inst::StoreRaw { addr, value, .. } => {
                scan_src_regs(addr, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::LdStructured {
                dst, index, offset, ..
            } => {
                scan_reg(dst.reg);
                scan_src_regs(index, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
            }
            Sm4Inst::StoreStructured {
                index,
                offset,
                value,
                ..
            } => {
                scan_src_regs(index, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::WorkgroupBarrier => {}
            Sm4Inst::Switch { selector } => {
                scan_src_regs(selector, &mut scan_reg);
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch | Sm4Inst::Break => {}
            Sm4Inst::BufInfoRaw { dst, .. } | Sm4Inst::BufInfoStructured { dst, .. } => {
                scan_reg(dst.reg);
            }
            Sm4Inst::Unknown { .. } => {}
            Sm4Inst::Emit { .. } | Sm4Inst::Cut { .. } => {}
            Sm4Inst::Ret => {}
        }
    }

    // Ensure we have internal output regs for any signature-declared outputs that are
    // not written by the shader body (common for unused varyings).
    for &reg in io.outputs.keys() {
        outputs.insert(reg);
    }
    if let Some(pos_reg) = io.vs_position_register {
        outputs.insert(pos_reg);
    }

    let has_temps = !temps.is_empty();
    for &idx in &temps {
        w.line(&format!("var r{idx}: vec4<f32> = vec4<f32>(0.0);"));
    }
    if has_temps {
        w.line("");
    }
    let has_outputs = !outputs.is_empty();
    for &idx in &outputs {
        w.line(&format!("var o{idx}: vec4<f32> = vec4<f32>(0.0);"));
    }
    if has_outputs {
        w.line("");
    }

    Ok(())
}

fn scan_src_regs(src: &crate::sm4_ir::SrcOperand, f: &mut impl FnMut(RegisterRef)) {
    if let SrcKind::Register(r) = src.kind {
        f(r);
    }
}

struct EmitCtx<'a> {
    stage: ShaderStage,
    io: &'a IoMaps,
    resources: &'a ResourceUsage,
}

fn emit_instructions(
    w: &mut WgslWriter,
    module: &Sm4Module,
    ctx: &EmitCtx<'_>,
) -> Result<(), ShaderTranslateError> {
    #[derive(Debug, Clone, Copy)]
    enum BlockKind {
        If { has_else: bool },
        Loop,
    }

    #[derive(Debug, Clone, Copy)]
    enum SwitchLabel {
        Case(i32),
        Default,
    }

    #[derive(Debug, Default)]
    struct SwitchFrame {
        pending_labels: Vec<SwitchLabel>,
        saw_default: bool,
    }

    #[derive(Debug, Default)]
    struct CaseFrame {
        last_was_break: bool,
    }

    #[derive(Debug)]
    enum CfFrame {
        Switch(SwitchFrame),
        Case(CaseFrame),
    }

    let mut blocks: Vec<BlockKind> = Vec::new();
    impl BlockKind {
        fn describe(self) -> String {
            match self {
                BlockKind::If { has_else: false } => "if".to_owned(),
                BlockKind::If { has_else: true } => "if (else already seen)".to_owned(),
                BlockKind::Loop => "loop".to_owned(),
            }
        }

        fn expected_end_token(self) -> &'static str {
            match self {
                BlockKind::If { .. } => "EndIf",
                BlockKind::Loop => "EndLoop",
            }
        }
    }

    let mut cf_stack: Vec<CfFrame> = Vec::new();

    let emit_src_scalar_i32 = |src: &crate::sm4_ir::SrcOperand,
                               inst_index: usize,
                               opcode: &'static str|
     -> Result<String, ShaderTranslateError> {
        // Register values are modeled as `vec4<f32>`. Reconstruct an `i32` selector by choosing
        // between numeric conversion and raw bitcast depending on whether the lane looks like an
        // exact integer.
        let vec_f = emit_src_vec4(src, inst_index, opcode, ctx)?;
        let vec_i = emit_src_vec4_i32(src, inst_index, opcode, ctx)?;
        let f = format!("({vec_f}).x");
        let i = format!("({vec_i}).x");
        Ok(format!("select({i}, i32({f}), ({f}) == floor({f}))"))
    };

    let fmt_case_values = |values: &[i32]| -> String {
        values
            .iter()
            .map(|v| format!("{v}i"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let close_case_body = |w: &mut WgslWriter,
                           cf_stack: &mut Vec<CfFrame>,
                           fallthrough_to_next: bool|
     -> Result<(), ShaderTranslateError> {
        let Some(CfFrame::Case(case_frame)) = cf_stack.last() else {
            return Ok(());
        };
        let last_was_break = case_frame.last_was_break;

        if fallthrough_to_next && !last_was_break {
            w.line("fallthrough;");
        }

        // Close the WGSL case block.
        w.dedent();
        w.line("}");
        cf_stack.pop();
        Ok(())
    };

    let flush_pending_labels =
        |w: &mut WgslWriter, cf_stack: &mut Vec<CfFrame>, inst_index: usize| -> Result<(), ShaderTranslateError> {
            let pending_labels = match cf_stack.last_mut() {
                Some(CfFrame::Switch(sw)) => {
                    if sw.pending_labels.is_empty() {
                        // Inside a switch but not in a case block. Non-label instructions here are
                        // invalid.
                        return Err(ShaderTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "switch_body_without_case".to_owned(),
                        });
                    }
                    std::mem::take(&mut sw.pending_labels)
                }
                _ => return Ok(()),
            };

            let mut case_values = Vec::<i32>::new();
            let mut has_default = false;
            for lbl in &pending_labels {
                match *lbl {
                    SwitchLabel::Case(v) => case_values.push(v),
                    SwitchLabel::Default => has_default = true,
                }
            }

            let last_label = *pending_labels.last().expect("pending_labels non-empty");

            // If the label set contains a default label, we may need an extra fallthrough stub, since
            // WGSL can't combine `default` with `case` selectors in a single clause.
            match (has_default, last_label) {
                (false, _) => {
                    let selectors = fmt_case_values(&case_values);
                    w.line(&format!("case {selectors}: {{"));
                    w.indent();
                    cf_stack.push(CfFrame::Case(CaseFrame::default()));
                }
                (true, SwitchLabel::Default) => {
                    if !case_values.is_empty() {
                        let selectors = fmt_case_values(&case_values);
                        w.line(&format!("case {selectors}: {{"));
                        w.indent();
                        w.line("fallthrough;");
                        w.dedent();
                        w.line("}");
                    }
                    w.line("default: {");
                    w.indent();
                    cf_stack.push(CfFrame::Case(CaseFrame::default()));
                }
                (true, SwitchLabel::Case(_)) => {
                    // Emit the default fallthrough stub first so it can reach the case body.
                    w.line("default: {");
                    w.indent();
                    w.line("fallthrough;");
                    w.dedent();
                    w.line("}");
                    let selectors = fmt_case_values(&case_values);
                    w.line(&format!("case {selectors}: {{"));
                    w.indent();
                    cf_stack.push(CfFrame::Case(CaseFrame::default()));
                }
            }

            Ok(())
        };

    // Structured buffer access (`*_structured`) requires the element stride in bytes, which is
    // provided via `dcl_resource_structured` / `dcl_uav_structured`. Collect those declarations so
    // we can lower address calculations when emitting WGSL.
    let mut srv_buffer_decls = BTreeMap::<u32, (BufferKind, u32)>::new();
    let mut uav_buffer_decls = BTreeMap::<u32, (BufferKind, u32)>::new();
    for decl in &module.decls {
        match decl {
            Sm4Decl::ResourceBuffer { slot, stride, kind } => {
                srv_buffer_decls.insert(*slot, (*kind, *stride));
            }
            Sm4Decl::UavBuffer { slot, stride, kind } => {
                uav_buffer_decls.insert(*slot, (*kind, *stride));
            }
            _ => {}
        }
    }

    for (inst_index, inst) in module.instructions.iter().enumerate() {
        match inst {
            Sm4Inst::Case { value } => {
                close_case_body(w, &mut cf_stack, true)?;

                let Some(CfFrame::Switch(sw)) = cf_stack.last_mut() else {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "case".to_owned(),
                    });
                };
                sw.pending_labels.push(SwitchLabel::Case(*value as i32));
                continue;
            }
            Sm4Inst::Default => {
                close_case_body(w, &mut cf_stack, true)?;

                let Some(CfFrame::Switch(sw)) = cf_stack.last_mut() else {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "default".to_owned(),
                    });
                };
                sw.saw_default = true;
                sw.pending_labels.push(SwitchLabel::Default);
                continue;
            }
            Sm4Inst::EndSwitch => {
                // Close any open case body without an implicit fallthrough: reaching the end of a
                // switch clause breaks out of the switch.
                close_case_body(w, &mut cf_stack, false)?;

                let Some(CfFrame::Switch(_)) = cf_stack.last() else {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "endswitch".to_owned(),
                    });
                };

                let (pending_labels_nonempty, saw_default) = match cf_stack.last() {
                    Some(CfFrame::Switch(sw)) => (!sw.pending_labels.is_empty(), sw.saw_default),
                    _ => unreachable!("checked switch exists above"),
                };

                // If there are pending labels but no body, emit an empty clause.
                if pending_labels_nonempty {
                    flush_pending_labels(w, &mut cf_stack, inst_index)?;
                    close_case_body(w, &mut cf_stack, false)?;
                }

                // WGSL `switch` allows omitting `default`, but we always emit one so that
                // switch-without-default shaders stay structurally valid and match the HLSL
                // semantics where a missing default is equivalent to an empty one.
                if !saw_default {
                    w.line("default: {");
                    w.indent();
                    w.dedent();
                    w.line("}");
                }

                // Close the switch.
                w.dedent();
                w.line("}");
                cf_stack.pop();
                continue;
            }
            _ => {}
        }

        // Ensure any pending case labels are emitted before the first instruction of the clause
        // body.
        if matches!(cf_stack.last(), Some(CfFrame::Switch(_))) {
            flush_pending_labels(w, &mut cf_stack, inst_index)?;
        }

        // Any regular statement resets the fallthrough detector.
        if let Some(CfFrame::Case(case_frame)) = cf_stack.last_mut() {
            case_frame.last_was_break = false;
        }

        let maybe_saturate = |dst: &crate::sm4_ir::DstOperand, expr: String| {
            if dst.saturate {
                format!("clamp(({expr}), vec4<f32>(0.0), vec4<f32>(1.0))")
            } else {
                expr
            }
        };

        match inst {
            Sm4Inst::Switch { selector } => {
                let selector = emit_src_scalar_i32(selector, inst_index, "switch")?;
                w.line(&format!("switch({selector}) {{"));
                w.indent();
                cf_stack.push(CfFrame::Switch(SwitchFrame::default()));
            }
            Sm4Inst::Break => {
                let Some(CfFrame::Case(case_frame)) = cf_stack.last_mut() else {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "break".to_owned(),
                    });
                };
                w.line("break;");
                case_frame.last_was_break = true;
            }
            Sm4Inst::If { cond, test } => {
                let cond_vec = emit_src_vec4(cond, inst_index, "if", ctx)?;
                let cond_scalar = format!("({cond_vec}).x");
                let cond_bits = format!("bitcast<u32>({cond_scalar})");
                let expr = match test {
                    crate::sm4_ir::Sm4TestBool::Zero => format!("{cond_bits} == 0u"),
                    crate::sm4_ir::Sm4TestBool::NonZero => format!("{cond_bits} != 0u"),
                };
                w.line(&format!("if ({expr}) {{"));
                w.indent();
                blocks.push(BlockKind::If { has_else: false });
            }
            Sm4Inst::Else => {
                match blocks.last_mut() {
                    Some(BlockKind::If { has_else }) => {
                        if *has_else {
                            return Err(ShaderTranslateError::MalformedControlFlow {
                                inst_index,
                                expected: "if (without an else)".to_owned(),
                                found: BlockKind::If { has_else: true }.describe(),
                            });
                        }
                        *has_else = true;
                    }
                    Some(other) => {
                        return Err(ShaderTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "if".to_owned(),
                            found: other.describe(),
                        });
                    }
                    None => {
                        return Err(ShaderTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "if".to_owned(),
                            found: "none".to_owned(),
                        });
                    }
                }

                // Close the `if` block and open the `else` block.
                w.dedent();
                w.line("} else {");
                w.indent();
            }
            Sm4Inst::EndIf => {
                match blocks.last() {
                    Some(BlockKind::If { .. }) => {
                        blocks.pop();
                        w.dedent();
                        w.line("}");
                    }
                    Some(other) => {
                        return Err(ShaderTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "if".to_owned(),
                            found: other.describe(),
                        });
                    }
                    None => {
                        return Err(ShaderTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "if".to_owned(),
                            found: "none".to_owned(),
                        });
                    }
                }
            }
            Sm4Inst::Loop => {
                w.line("loop {");
                w.indent();
                blocks.push(BlockKind::Loop);
            }
            Sm4Inst::EndLoop => match blocks.last() {
                Some(BlockKind::Loop) => {
                    blocks.pop();
                    w.dedent();
                    w.line("}");
                }
                Some(other) => {
                    return Err(ShaderTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: other.describe(),
                    });
                }
                None => {
                    return Err(ShaderTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: "none".to_owned(),
                    });
                }
            },
            Sm4Inst::Mov { dst, src } => {
                let rhs = emit_src_vec4(src, inst_index, "mov", ctx)?;
                let rhs = maybe_saturate(dst, rhs);
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "mov", ctx)?;
            }
            Sm4Inst::Movc { dst, cond, a, b } => {
                let cond_vec = emit_src_vec4(cond, inst_index, "movc", ctx)?;
                let a_vec = emit_src_vec4(a, inst_index, "movc", ctx)?;
                let b_vec = emit_src_vec4(b, inst_index, "movc", ctx)?;

                let cond_bits = format!("movc_cond_bits_{inst_index}");
                let cond_bool = format!("movc_cond_bool_{inst_index}");
                w.line(&format!(
                    "let {cond_bits} = bitcast<vec4<u32>>({cond_vec});"
                ));
                w.line(&format!("let {cond_bool} = {cond_bits} != vec4<u32>(0u);"));

                let expr =
                    maybe_saturate(dst, format!("select(({b_vec}), ({a_vec}), {cond_bool})"));
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "movc", ctx)?;
            }
            Sm4Inst::Utof { dst, src } => {
                // Unsigned int -> float conversion.
                //
                // The source operand is carried in our untyped `vec4<f32>` register model; we
                // reinterpret each lane as `u32`, then apply a numeric conversion to `f32`.
                let src_bits = emit_src_vec4(src, inst_index, "utof", ctx)?;
                let rhs = format!("vec4<f32>(bitcast<vec4<u32>>({src_bits}))");
                let rhs = maybe_saturate(dst, rhs);
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "utof", ctx)?;
            }
            Sm4Inst::And { dst, a, b } => {
                let a_vec = emit_src_vec4(a, inst_index, "and", ctx)?;
                let b_vec = emit_src_vec4(b, inst_index, "and", ctx)?;
                let rhs = format!(
                    "bitcast<vec4<f32>>(bitcast<vec4<u32>>({a_vec}) & bitcast<vec4<u32>>({b_vec}))"
                );
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "and", ctx)?;
            }
            Sm4Inst::Add { dst, a, b } => {
                let a = emit_src_vec4(a, inst_index, "add", ctx)?;
                let b = emit_src_vec4(b, inst_index, "add", ctx)?;
                let rhs = maybe_saturate(dst, format!("({a}) + ({b})"));
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "add", ctx)?;
            }
            Sm4Inst::IAddC {
                dst_sum,
                dst_carry,
                a,
                b,
            } => {
                emit_add_with_carry(w, "iaddc", inst_index, dst_sum, dst_carry, a, b, ctx)?;
            }
            Sm4Inst::UAddC {
                dst_sum,
                dst_carry,
                a,
                b,
            } => {
                emit_add_with_carry(w, "uaddc", inst_index, dst_sum, dst_carry, a, b, ctx)?;
            }
            Sm4Inst::ISubC {
                dst_diff,
                dst_borrow,
                a,
                b,
            } => {
                emit_sub_with_borrow(w, "isubc", inst_index, dst_diff, dst_borrow, a, b, ctx)?;
            }
            Sm4Inst::USubB {
                dst_diff,
                dst_borrow,
                a,
                b,
            } => {
                emit_sub_with_borrow(w, "usubb", inst_index, dst_diff, dst_borrow, a, b, ctx)?;
            }
            Sm4Inst::Mul { dst, a, b } => {
                let a = emit_src_vec4(a, inst_index, "mul", ctx)?;
                let b = emit_src_vec4(b, inst_index, "mul", ctx)?;
                let rhs = maybe_saturate(dst, format!("({a}) * ({b})"));
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "mul", ctx)?;
            }
            Sm4Inst::Mad { dst, a, b, c } => {
                let a = emit_src_vec4(a, inst_index, "mad", ctx)?;
                let b = emit_src_vec4(b, inst_index, "mad", ctx)?;
                let c = emit_src_vec4(c, inst_index, "mad", ctx)?;
                let rhs = maybe_saturate(dst, format!("({a}) * ({b}) + ({c})"));
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "mad", ctx)?;
            }
            Sm4Inst::Dp3 { dst, a, b } => {
                let a = emit_src_vec4(a, inst_index, "dp3", ctx)?;
                let b = emit_src_vec4(b, inst_index, "dp3", ctx)?;
                let expr = format!("vec4<f32>(dot(({a}).xyz, ({b}).xyz))");
                let expr = maybe_saturate(dst, expr);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "dp3", ctx)?;
            }
            Sm4Inst::Dp4 { dst, a, b } => {
                let a = emit_src_vec4(a, inst_index, "dp4", ctx)?;
                let b = emit_src_vec4(b, inst_index, "dp4", ctx)?;
                let expr = format!("vec4<f32>(dot(({a}), ({b})))");
                let expr = maybe_saturate(dst, expr);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "dp4", ctx)?;
            }
            Sm4Inst::Min { dst, a, b } => {
                let a = emit_src_vec4(a, inst_index, "min", ctx)?;
                let b = emit_src_vec4(b, inst_index, "min", ctx)?;
                let expr = maybe_saturate(dst, format!("min(({a}), ({b}))"));
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "min", ctx)?;
            }
            Sm4Inst::Max { dst, a, b } => {
                let a = emit_src_vec4(a, inst_index, "max", ctx)?;
                let b = emit_src_vec4(b, inst_index, "max", ctx)?;
                let expr = maybe_saturate(dst, format!("max(({a}), ({b}))"));
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "max", ctx)?;
            }
            Sm4Inst::UDiv {
                dst_quot,
                dst_rem,
                a,
                b,
            } => {
                // DXBC integer ops write raw bits into the untyped register file. Model that by
                // bitcasting through `u32`, performing the arithmetic, then bitcasting back to
                // `f32` before writing.
                let a_u = emit_src_vec4_u32_int(a, inst_index, "udiv", ctx)?;
                let b_u = emit_src_vec4_u32_int(b, inst_index, "udiv", ctx)?;
                let a_name = format!("udiv_a{inst_index}");
                let b_name = format!("udiv_b{inst_index}");
                let q_name = format!("udiv_q{inst_index}");
                let r_name = format!("udiv_r{inst_index}");
                let q_f_name = format!("udiv_qf{inst_index}");
                let r_f_name = format!("udiv_rf{inst_index}");
                w.line(&format!("let {a_name}: vec4<u32> = {a_u};"));
                w.line(&format!("let {b_name}: vec4<u32> = {b_u};"));
                w.line(&format!(
                    "let {q_name}: vec4<u32> = ({a_name}) / ({b_name});"
                ));
                w.line(&format!(
                    "let {r_name}: vec4<u32> = ({a_name}) % ({b_name});"
                ));
                w.line(&format!(
                    "let {q_f_name}: vec4<f32> = bitcast<vec4<f32>>({q_name});"
                ));
                w.line(&format!(
                    "let {r_f_name}: vec4<f32> = bitcast<vec4<f32>>({r_name});"
                ));
                emit_write_masked(
                    w,
                    dst_quot.reg,
                    dst_quot.mask,
                    q_f_name,
                    inst_index,
                    "udiv",
                    ctx,
                )?;
                emit_write_masked(
                    w,
                    dst_rem.reg,
                    dst_rem.mask,
                    r_f_name,
                    inst_index,
                    "udiv",
                    ctx,
                )?;
            }
            Sm4Inst::IDiv {
                dst_quot,
                dst_rem,
                a,
                b,
            } => {
                // Same idea as `udiv`, but operate on signed integers.
                let a_i = emit_src_vec4_i32_int(a, inst_index, "idiv", ctx)?;
                let b_i = emit_src_vec4_i32_int(b, inst_index, "idiv", ctx)?;
                let a_name = format!("idiv_a{inst_index}");
                let b_name = format!("idiv_b{inst_index}");
                let q_name = format!("idiv_q{inst_index}");
                let r_name = format!("idiv_r{inst_index}");
                let q_f_name = format!("idiv_qf{inst_index}");
                let r_f_name = format!("idiv_rf{inst_index}");
                w.line(&format!("let {a_name}: vec4<i32> = {a_i};"));
                w.line(&format!("let {b_name}: vec4<i32> = {b_i};"));
                w.line(&format!(
                    "let {q_name}: vec4<i32> = ({a_name}) / ({b_name});"
                ));
                w.line(&format!(
                    "let {r_name}: vec4<i32> = ({a_name}) % ({b_name});"
                ));
                w.line(&format!(
                    "let {q_f_name}: vec4<f32> = bitcast<vec4<f32>>({q_name});"
                ));
                w.line(&format!(
                    "let {r_f_name}: vec4<f32> = bitcast<vec4<f32>>({r_name});"
                ));
                emit_write_masked(
                    w,
                    dst_quot.reg,
                    dst_quot.mask,
                    q_f_name,
                    inst_index,
                    "idiv",
                    ctx,
                )?;
                emit_write_masked(
                    w,
                    dst_rem.reg,
                    dst_rem.mask,
                    r_f_name,
                    inst_index,
                    "idiv",
                    ctx,
                )?;
            }
            Sm4Inst::IMin { dst, a, b } => {
                let a = emit_src_vec4_i32(a, inst_index, "imin", ctx)?;
                let b = emit_src_vec4_i32(b, inst_index, "imin", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(vec4<u32>(min(({a}), ({b}))))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "imin", ctx)?;
            }
            Sm4Inst::IMax { dst, a, b } => {
                let a = emit_src_vec4_i32(a, inst_index, "imax", ctx)?;
                let b = emit_src_vec4_i32(b, inst_index, "imax", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(vec4<u32>(max(({a}), ({b}))))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "imax", ctx)?;
            }
            Sm4Inst::UMin { dst, a, b } => {
                let a = emit_src_vec4_u32(a, inst_index, "umin", ctx)?;
                let b = emit_src_vec4_u32(b, inst_index, "umin", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(min(({a}), ({b})))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "umin", ctx)?;
            }
            Sm4Inst::UMax { dst, a, b } => {
                let a = emit_src_vec4_u32(a, inst_index, "umax", ctx)?;
                let b = emit_src_vec4_u32(b, inst_index, "umax", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(max(({a}), ({b})))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "umax", ctx)?;
            }
            Sm4Inst::IAbs { dst, src } => {
                let src = emit_src_vec4_i32(src, inst_index, "iabs", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(vec4<u32>(abs({src})))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "iabs", ctx)?;
            }
            Sm4Inst::INeg { dst, src } => {
                let src = emit_src_vec4_i32(src, inst_index, "ineg", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(vec4<u32>(-({src})))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ineg", ctx)?;
            }
            Sm4Inst::Rcp { dst, src } => {
                let src = emit_src_vec4(src, inst_index, "rcp", ctx)?;
                let expr = maybe_saturate(dst, format!("1.0 / ({src})"));
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "rcp", ctx)?;
            }
            Sm4Inst::Rsq { dst, src } => {
                let src = emit_src_vec4(src, inst_index, "rsq", ctx)?;
                let expr = maybe_saturate(dst, format!("inverseSqrt({src})"));
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "rsq", ctx)?;
            }
            Sm4Inst::Bfi {
                dst,
                width,
                offset,
                insert,
                base,
            } => {
                let width_i = emit_src_vec4_i32(width, inst_index, "bfi", ctx)?;
                let offset_i = emit_src_vec4_i32(offset, inst_index, "bfi", ctx)?;
                let insert_i = emit_src_vec4_i32(insert, inst_index, "bfi", ctx)?;
                let base_i = emit_src_vec4_i32(base, inst_index, "bfi", ctx)?;

                // WGSL `insertBits` takes scalar `offset`/`count`, but DXBC operands are vectors.
                // Emit per-lane inserts so swizzles (common in pack/unpack patterns) behave like
                // DXBC.
                let lanes = ['x', 'y', 'z', 'w'];
                let mut out = Vec::with_capacity(4);
                for lane in lanes {
                    let offset_u = format!("u32(({offset_i}).{lane})");
                    let count_u = format!("u32(({width_i}).{lane})");
                    let insert_u = format!("bitcast<u32>(({insert_i}).{lane})");
                    let base_u = format!("bitcast<u32>(({base_i}).{lane})");
                    out.push(format!(
                        "bitcast<f32>(insertBits({base_u}, {insert_u}, {offset_u}, {count_u}))"
                    ));
                }
                let expr = format!(
                    "vec4<f32>({}, {}, {}, {})",
                    out[0], out[1], out[2], out[3]
                );
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "bfi", ctx)?;
            }
            Sm4Inst::Ubfe {
                dst,
                width,
                offset,
                src,
            } => {
                let width_i = emit_src_vec4_i32(width, inst_index, "ubfe", ctx)?;
                let offset_i = emit_src_vec4_i32(offset, inst_index, "ubfe", ctx)?;
                let src_i = emit_src_vec4_i32(src, inst_index, "ubfe", ctx)?;

                let lanes = ['x', 'y', 'z', 'w'];
                let mut out = Vec::with_capacity(4);
                for lane in lanes {
                    let offset_u = format!("u32(({offset_i}).{lane})");
                    let count_u = format!("u32(({width_i}).{lane})");
                    let src_u = format!("bitcast<u32>(({src_i}).{lane})");
                    out.push(format!(
                        "bitcast<f32>(extractBits({src_u}, {offset_u}, {count_u}))"
                    ));
                }
                let expr = format!(
                    "vec4<f32>({}, {}, {}, {})",
                    out[0], out[1], out[2], out[3]
                );
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ubfe", ctx)?;
            }
            Sm4Inst::Ibfe {
                dst,
                width,
                offset,
                src,
            } => {
                let width_i = emit_src_vec4_i32(width, inst_index, "ibfe", ctx)?;
                let offset_i = emit_src_vec4_i32(offset, inst_index, "ibfe", ctx)?;
                let src_i = emit_src_vec4_i32(src, inst_index, "ibfe", ctx)?;

                // `extractBits(i32, ...)` sign-extends in WGSL, matching D3D's `ibfe`.
                let lanes = ['x', 'y', 'z', 'w'];
                let mut out = Vec::with_capacity(4);
                for lane in lanes {
                    let offset_u = format!("u32(({offset_i}).{lane})");
                    let count_u = format!("u32(({width_i}).{lane})");
                    let src_s = format!("({src_i}).{lane}");
                    out.push(format!(
                        "bitcast<f32>(extractBits({src_s}, {offset_u}, {count_u}))"
                    ));
                }
                let expr = format!(
                    "vec4<f32>({}, {}, {}, {})",
                    out[0], out[1], out[2], out[3]
                );
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ibfe", ctx)?;
            }
            Sm4Inst::Cmp { dst, a, b, op, ty } => {
                let (a, b) = match ty {
                    CmpType::I32 => (
                        emit_src_vec4_i32(a, inst_index, "cmp", ctx)?,
                        emit_src_vec4_i32(b, inst_index, "cmp", ctx)?,
                    ),
                    CmpType::U32 => (
                        emit_src_vec4_u32(a, inst_index, "cmp", ctx)?,
                        emit_src_vec4_u32(b, inst_index, "cmp", ctx)?,
                    ),
                };

                let cmp = match op {
                    CmpOp::Eq => format!("({a}) == ({b})"),
                    CmpOp::Ne => format!("({a}) != ({b})"),
                    CmpOp::Lt => format!("({a}) < ({b})"),
                    CmpOp::Le => format!("({a}) <= ({b})"),
                    CmpOp::Gt => format!("({a}) > ({b})"),
                    CmpOp::Ge => format!("({a}) >= ({b})"),
                };

                // Convert the bool vector result into D3D-style predicate mask bits.
                let mask = format!("select(vec4<u32>(0u), vec4<u32>(0xffffffffu), {cmp})");
                let expr = format!("bitcast<vec4<f32>>({mask})");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "cmp", ctx)?;
            }
            Sm4Inst::Bfrev { dst, src } => {
                let src_u = emit_src_vec4_u32(src, inst_index, "bfrev", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(reverseBits({src_u}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "bfrev", ctx)?;
            }
            Sm4Inst::CountBits { dst, src } => {
                let src_u = emit_src_vec4_u32(src, inst_index, "countbits", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(countOneBits({src_u}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "countbits", ctx)?;
            }
            Sm4Inst::FirstbitHi { dst, src } => {
                let src_u = emit_src_vec4_u32(src, inst_index, "firstbit_hi", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(firstLeadingBit({src_u}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "firstbit_hi", ctx)?;
            }
            Sm4Inst::FirstbitLo { dst, src } => {
                let src_u = emit_src_vec4_u32(src, inst_index, "firstbit_lo", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(firstTrailingBit({src_u}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "firstbit_lo", ctx)?;
            }
            Sm4Inst::FirstbitShi { dst, src } => {
                let src_i = emit_src_vec4_i32(src, inst_index, "firstbit_shi", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(firstLeadingBit({src_i}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "firstbit_shi", ctx)?;
            }
            Sm4Inst::Sample {
                dst,
                coord,
                texture,
                sampler,
            } => {
                let coord = emit_src_vec4(coord, inst_index, "sample", ctx)?;
                // WGSL forbids implicit-derivative sampling (`textureSample`) outside the fragment
                // stage, so map D3D-style `sample` to `textureSampleLevel(..., 0.0)` when
                // translating vertex/compute shaders.
                //
                // Note: On real D3D hardware, non-fragment `sample` uses an implementation-defined
                // LOD selection (typically base LOD). Using LOD 0 is a reasonable approximation and
                // keeps the generated WGSL valid.
                let expr = match ctx.stage {
                    ShaderStage::Pixel => format!(
                        "textureSample(t{}, s{}, ({coord}).xy)",
                        texture.slot, sampler.slot
                    ),
                    _ => format!(
                        "textureSampleLevel(t{}, s{}, ({coord}).xy, 0.0)",
                        texture.slot, sampler.slot
                    ),
                };
                let expr = maybe_saturate(dst, expr);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "sample", ctx)?;
            }
            Sm4Inst::SampleL {
                dst,
                coord,
                texture,
                sampler,
                lod,
            } => {
                let coord = emit_src_vec4(coord, inst_index, "sample_l", ctx)?;
                let lod_vec = emit_src_vec4(lod, inst_index, "sample_l", ctx)?;
                let expr = format!(
                    "textureSampleLevel(t{}, s{}, ({coord}).xy, ({lod_vec}).x)",
                    texture.slot, sampler.slot
                );
                let expr = maybe_saturate(dst, expr);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "sample_l", ctx)?;
            }
            Sm4Inst::Ld {
                dst,
                coord,
                texture,
                lod,
            } => {
                // SM4/SM5 `ld` (e.g. `Texture2D.Load`) consumes integer texel coordinates and an
                // integer mip level.
                //
                // DXBC register files are untyped; integer-typed values are stored as raw 32-bit
                // patterns in the same lanes that the rest of the translator models as
                // `vec4<f32>`. For `textureLoad`, interpret the source lanes strictly as integer
                // bits (i.e. bitcast `f32` -> `i32`) with no float-to-int heuristics.
                let coord_i = emit_src_vec4_i32(coord, inst_index, "ld", ctx)?;
                let x = format!("({coord_i}).x");
                let y = format!("({coord_i}).y");

                let lod_i = emit_src_vec4_i32(lod, inst_index, "ld", ctx)?;
                let lod_scalar = format!("({lod_i}).x");

                let expr = format!(
                    "textureLoad(t{}, vec2<i32>({x}, {y}), {lod_scalar})",
                    texture.slot
                );
                let expr = maybe_saturate(dst, expr);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ld", ctx)?;
            }
            Sm4Inst::LdRaw { dst, addr, buffer } => {
                // Raw buffer loads operate on byte offsets. Model buffers as a storage
                // `array<u32>` and derive a word index from the byte address.
                let addr_u = emit_src_vec4_u32(addr, inst_index, "ld_raw", ctx)?;
                let base_name = format!("ld_raw_base{inst_index}");
                w.line(&format!("let {base_name}: u32 = (({addr_u}).x) / 4u;"));

                let mask_bits = dst.mask.0 & 0xF;
                let load_lane = |bit: u8, offset: u32| {
                    if (mask_bits & bit) != 0 {
                        format!("t{}.data[{base_name} + {offset}u]", buffer.slot)
                    } else {
                        "0u".to_owned()
                    }
                };

                let u_name = format!("ld_raw_u{inst_index}");
                w.line(&format!(
                    "let {u_name}: vec4<u32> = vec4<u32>({}, {}, {}, {});",
                    load_lane(1, 0),
                    load_lane(2, 1),
                    load_lane(4, 2),
                    load_lane(8, 3),
                ));
                let f_name = format!("ld_raw_f{inst_index}");
                w.line(&format!("let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"));

                let expr = maybe_saturate(dst, f_name);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ld_raw", ctx)?;
            }
            Sm4Inst::StoreRaw {
                uav,
                addr,
                value,
                mask,
            } => {
                let mask_bits = mask.0 & 0xF;
                if mask_bits == 0 {
                    return Err(ShaderTranslateError::UnsupportedWriteMask {
                        inst_index,
                        opcode: "store_raw",
                        mask: *mask,
                    });
                }

                let addr_u = emit_src_vec4_u32(addr, inst_index, "store_raw", ctx)?;
                let base_name = format!("store_raw_base{inst_index}");
                w.line(&format!("let {base_name}: u32 = (({addr_u}).x) / 4u;"));

                let value_u = emit_src_vec4_u32(value, inst_index, "store_raw", ctx)?;
                let value_name = format!("store_raw_val{inst_index}");
                w.line(&format!("let {value_name}: vec4<u32> = {value_u};"));

                let comps = [('x', 1u8, 0u32), ('y', 2u8, 1), ('z', 4u8, 2), ('w', 8u8, 3)];
                for (c, bit, offset) in comps {
                    if (mask_bits & bit) != 0 {
                        w.line(&format!(
                            "u{}.data[{base_name} + {offset}u] = ({value_name}).{c};",
                            uav.slot
                        ));
                    }
                }
            }
            Sm4Inst::LdStructured {
                dst,
                index,
                offset,
                buffer,
            } => {
                let Some((kind, stride)) = srv_buffer_decls.get(&buffer.slot).copied() else {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "ld_structured".to_owned(),
                    });
                };
                if kind != BufferKind::Structured || stride == 0 || (stride % 4) != 0 {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "ld_structured".to_owned(),
                    });
                }

                let index_u = emit_src_vec4_u32(index, inst_index, "ld_structured", ctx)?;
                let offset_u = emit_src_vec4_u32(offset, inst_index, "ld_structured", ctx)?;
                let base_name = format!("ld_struct_base{inst_index}");
                w.line(&format!(
                    "let {base_name}: u32 = ((({index_u}).x) * {stride}u + (({offset_u}).x)) / 4u;"
                ));

                let mask_bits = dst.mask.0 & 0xF;
                let load_lane = |bit: u8, offset: u32| {
                    if (mask_bits & bit) != 0 {
                        format!("t{}.data[{base_name} + {offset}u]", buffer.slot)
                    } else {
                        "0u".to_owned()
                    }
                };

                let u_name = format!("ld_struct_u{inst_index}");
                w.line(&format!(
                    "let {u_name}: vec4<u32> = vec4<u32>({}, {}, {}, {});",
                    load_lane(1, 0),
                    load_lane(2, 1),
                    load_lane(4, 2),
                    load_lane(8, 3),
                ));
                let f_name = format!("ld_struct_f{inst_index}");
                w.line(&format!("let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"));

                let expr = maybe_saturate(dst, f_name);
                emit_write_masked(
                    w,
                    dst.reg,
                    dst.mask,
                    expr,
                    inst_index,
                    "ld_structured",
                    ctx,
                )?;
            }
            Sm4Inst::StoreStructured {
                uav,
                index,
                offset,
                value,
                mask,
            } => {
                let mask_bits = mask.0 & 0xF;
                if mask_bits == 0 {
                    return Err(ShaderTranslateError::UnsupportedWriteMask {
                        inst_index,
                        opcode: "store_structured",
                        mask: *mask,
                    });
                }
                let Some((kind, stride)) = uav_buffer_decls.get(&uav.slot).copied() else {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "store_structured".to_owned(),
                    });
                };
                if kind != BufferKind::Structured || stride == 0 || (stride % 4) != 0 {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "store_structured".to_owned(),
                    });
                }

                let index_u = emit_src_vec4_u32(index, inst_index, "store_structured", ctx)?;
                let offset_u = emit_src_vec4_u32(offset, inst_index, "store_structured", ctx)?;
                let base_name = format!("store_struct_base{inst_index}");
                w.line(&format!(
                    "let {base_name}: u32 = ((({index_u}).x) * {stride}u + (({offset_u}).x)) / 4u;"
                ));

                let value_u = emit_src_vec4_u32(value, inst_index, "store_structured", ctx)?;
                let value_name = format!("store_struct_val{inst_index}");
                w.line(&format!("let {value_name}: vec4<u32> = {value_u};"));

                let comps = [('x', 1u8, 0u32), ('y', 2u8, 1), ('z', 4u8, 2), ('w', 8u8, 3)];
                for (c, bit, offset) in comps {
                    if (mask_bits & bit) != 0 {
                        w.line(&format!(
                            "u{}.data[{base_name} + {offset}u] = ({value_name}).{c};",
                            uav.slot
                        ));
                    }
                }
            }
            Sm4Inst::BufInfoRaw { dst, buffer } => {
                let dwords = format!("arrayLength(&t{}.data)", buffer.slot);
                let bytes = format!("({dwords}) * 4u");
                // `bufinfo` produces integer values. DXBC register files are untyped, so store the
                // raw `u32` bits into our `vec4<f32>` register model by bitcasting.
                //
                // Output packing:
                // - x = total byte size
                // - yzw = 0
                let expr = format!(
                    "bitcast<vec4<f32>>(vec4<u32>(({bytes}), 0u, 0u, 0u))"
                );
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "bufinfo", ctx)?;
            }
            Sm4Inst::BufInfoStructured {
                dst,
                buffer,
                stride_bytes,
            } => {
                let dwords = format!("arrayLength(&t{}.data)", buffer.slot);
                let byte_size = format!("({dwords}) * 4u");
                let stride = format!("{}u", stride_bytes);
                let elem_count = format!("({byte_size}) / ({stride})");
                // Output packing:
                // - x = element count
                // - y = stride (bytes)
                // - zw = 0
                let expr = format!(
                    "bitcast<vec4<f32>>(vec4<u32>(({elem_count}), ({stride}), 0u, 0u))"
                );
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "bufinfo", ctx)?;
            }
            Sm4Inst::WorkgroupBarrier => {
                if ctx.stage != ShaderStage::Compute {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "workgroup_barrier".to_owned(),
                    });
                }

                // SM5 `sync_*_t` instructions are workgroup barriers that optionally include
                // storage/UAV memory ordering semantics.
                //
                // WGSL exposes:
                // - `workgroupBarrier()` for control + workgroup-memory synchronization.
                // - `storageBarrier()` for storage-buffer/texture memory ordering.
                //
                // We conservatively emit both. This preserves the semantics of
                // `DeviceMemoryBarrierWithGroupSync()` / `AllMemoryBarrierWithGroupSync()` in
                // addition to `GroupMemoryBarrierWithGroupSync()`.
                //
                // Order matters if `storageBarrier()` is treated as a memory fence without an
                // execution barrier: we want all invocations to execute it before synchronizing.
                w.line("storageBarrier();");
                w.line("workgroupBarrier();");
            }
            Sm4Inst::Unknown { opcode } => {
                let opcode = opcode_name(*opcode)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("opcode_{opcode}"));
                return Err(ShaderTranslateError::UnsupportedInstruction { inst_index, opcode });
            }
            Sm4Inst::Emit { stream } => {
                let opcode = if *stream == 0 {
                    "emit".to_owned()
                } else {
                    format!("emit_stream({stream})")
                };
                return Err(ShaderTranslateError::UnsupportedInstruction { inst_index, opcode });
            }
            Sm4Inst::Cut { stream } => {
                let opcode = if *stream == 0 {
                    "cut".to_owned()
                } else {
                    format!("cut_stream({stream})")
                };
                return Err(ShaderTranslateError::UnsupportedInstruction { inst_index, opcode });
            }
            Sm4Inst::Ret => {
                // DXBC `ret` returns from the current shader invocation. We only need to emit an
                // explicit WGSL `return` when it appears inside a structured control-flow block;
                // at top-level the translation already emits a stage-appropriate return sequence.
                if blocks.is_empty() && cf_stack.is_empty() {
                    break;
                }

                if let Some(CfFrame::Case(case_frame)) = cf_stack.last_mut() {
                    case_frame.last_was_break = true;
                }

                match ctx.stage {
                    ShaderStage::Vertex => ctx.io.emit_vs_return(w)?,
                    ShaderStage::Pixel => ctx.io.emit_ps_return(w)?,
                    ShaderStage::Compute => {
                        // Compute entry points return `()`.
                        w.line("return;");
                    }
                    other => {
                        return Err(ShaderTranslateError::UnsupportedStage(other));
                    }
                }
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => unreachable!(
                "switch label instructions handled at top of loop"
            ),
        }
    }

    if let Some(&top) = blocks.last() {
        return Err(ShaderTranslateError::MalformedControlFlow {
            inst_index: module.instructions.len(),
            expected: top.expected_end_token().to_owned(),
            found: "end of shader".to_owned(),
        });
    }
    if !cf_stack.is_empty() {
        return Err(ShaderTranslateError::UnsupportedInstruction {
            inst_index: module.instructions.len(),
            opcode: "unbalanced_switch".to_owned(),
        });
    }
    Ok(())
}

fn emit_src_vec4(
    src: &crate::sm4_ir::SrcOperand,
    _inst_index: usize,
    _opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    let base = match &src.kind {
        SrcKind::Register(reg) => {
            match reg.file {
                RegFile::Temp => format!("r{}", reg.index),
                RegFile::Output => format!("o{}", reg.index),
                RegFile::OutputDepth => {
                    let depth_reg = ctx.io.ps_sv_depth_register.ok_or(
                        ShaderTranslateError::MissingSignature("pixel output SV_Depth"),
                    )?;
                    format!("o{depth_reg}")
                }
                RegFile::Input => ctx.io.read_input_vec4(ctx.stage, reg.index)?,
            }
        }
        SrcKind::GsInput { .. } => return Err(ShaderTranslateError::UnsupportedStage(ctx.stage)),
        SrcKind::ConstantBuffer { slot, reg } => {
            // Size is determined by scanning, so the declared array is always
            // large enough.
            let _ = ctx.resources.cbuffers.get(slot);
            format!("bitcast<vec4<f32>>(cb{slot}.regs[{reg}])")
        }
        SrcKind::ImmediateF32(vals) => {
            let lanes: Vec<String> = vals
                .iter()
                .map(|v| format!("bitcast<f32>(0x{v:08x}u)"))
                .collect();
            format!(
                "vec4<f32>({}, {}, {}, {})",
                lanes[0], lanes[1], lanes[2], lanes[3]
            )
        }
    };

    let mut expr = base;
    if !src.swizzle.is_identity() {
        let s = swizzle_suffix(src.swizzle);
        expr = format!("({expr}).{s}");
    }
    expr = apply_modifier(expr, src.modifier);
    Ok(expr)
}

fn emit_src_vec4_u32(
    src: &crate::sm4_ir::SrcOperand,
    _inst_index: usize,
    _opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    let base = match &src.kind {
        SrcKind::Register(reg) => {
            let expr = match reg.file {
                RegFile::Temp => format!("r{}", reg.index),
                RegFile::Output => format!("o{}", reg.index),
                RegFile::OutputDepth => {
                    let depth_reg = ctx.io.ps_sv_depth_register.ok_or(
                        ShaderTranslateError::MissingSignature("pixel output SV_Depth"),
                    )?;
                    format!("o{depth_reg}")
                }
                RegFile::Input => ctx.io.read_input_vec4(ctx.stage, reg.index)?,
            };
            format!("bitcast<vec4<u32>>({expr})")
        }
        SrcKind::GsInput { .. } => return Err(ShaderTranslateError::UnsupportedStage(ctx.stage)),
        SrcKind::ConstantBuffer { slot, reg } => {
            let _ = ctx.resources.cbuffers.get(slot);
            format!("cb{slot}.regs[{reg}]")
        }
        SrcKind::ImmediateF32(vals) => {
            let lanes: Vec<String> = vals.iter().map(|v| format!("0x{v:08x}u")).collect();
            format!(
                "vec4<u32>({}, {}, {}, {})",
                lanes[0], lanes[1], lanes[2], lanes[3]
            )
        }
    };

    let mut expr = base;
    if !src.swizzle.is_identity() {
        let s = swizzle_suffix(src.swizzle);
        expr = format!("({expr}).{s}");
    }
    expr = apply_modifier_u32(expr, src.modifier);
    Ok(expr)
}

/// Emits a `vec4<u32>` source for integer operations.
///
/// SM4/SM5 register operands are untyped; in practice integer values can show up either as:
/// - Raw integer bits written into the register file (common in real DXBC).
/// - Numeric floats (e.g. system-value inputs expanded via `f32(input.vertex_id)`).
///
/// To cover both, this helper derives a `u32` value per lane by selecting between:
/// - `bitcast<u32>(f32)` (raw bits)
/// - `u32(f32)` (numeric conversion)
/// based on whether the float value looks like a non-negative integer.
fn emit_src_vec4_u32_int(
    src: &crate::sm4_ir::SrcOperand,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    let mut no_mod = src.clone();
    no_mod.modifier = OperandModifier::None;
    let f = emit_src_vec4(&no_mod, inst_index, opcode, ctx)?;
    let bits = emit_src_vec4_u32(&no_mod, inst_index, opcode, ctx)?;
    let is_int = format!("(({f}) == floor(({f})))");
    let is_nonneg = format!("(({f}) >= vec4<f32>(0.0))");
    let cond = format!("select(vec4<bool>(false), {is_int}, {is_nonneg})");
    let base = format!("select(({bits}), vec4<u32>({f}), {cond})");
    Ok(apply_modifier_u32(base, src.modifier))
}
fn emit_src_vec4_i32(
    src: &crate::sm4_ir::SrcOperand,
    _inst_index: usize,
    _opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    let base = match &src.kind {
        SrcKind::Register(reg) => {
            let expr = match reg.file {
                RegFile::Temp => format!("r{}", reg.index),
                RegFile::Output => format!("o{}", reg.index),
                RegFile::OutputDepth => {
                    let depth_reg = ctx.io.ps_sv_depth_register.ok_or(
                        ShaderTranslateError::MissingSignature("pixel output SV_Depth"),
                    )?;
                    format!("o{depth_reg}")
                }
                RegFile::Input => ctx.io.read_input_vec4(ctx.stage, reg.index)?,
            };
            format!("bitcast<vec4<i32>>({expr})")
        }
        SrcKind::GsInput { .. } => return Err(ShaderTranslateError::UnsupportedStage(ctx.stage)),
        SrcKind::ConstantBuffer { slot, reg } => {
            let _ = ctx.resources.cbuffers.get(slot);
            format!("bitcast<vec4<i32>>(cb{slot}.regs[{reg}])")
        }
        SrcKind::ImmediateF32(vals) => {
            let lanes: Vec<String> = vals
                .iter()
                .map(|v| format!("bitcast<i32>(0x{v:08x}u)"))
                .collect();
            format!(
                "vec4<i32>({}, {}, {}, {})",
                lanes[0], lanes[1], lanes[2], lanes[3]
            )
        }
    };

    let mut expr = base;
    if !src.swizzle.is_identity() {
        let s = swizzle_suffix(src.swizzle);
        expr = format!("({expr}).{s}");
    }
    expr = apply_modifier(expr, src.modifier);
    Ok(expr)
}

/// Emits a `vec4<i32>` source for integer operations (see [`emit_src_vec4_u32_int`]).
fn emit_src_vec4_i32_int(
    src: &crate::sm4_ir::SrcOperand,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    let mut no_mod = src.clone();
    no_mod.modifier = OperandModifier::None;
    let f = emit_src_vec4(&no_mod, inst_index, opcode, ctx)?;
    let bits = emit_src_vec4_i32(&no_mod, inst_index, opcode, ctx)?;
    let cond = format!("(({f}) == floor(({f})))");
    let base = format!("select(({bits}), vec4<i32>({f}), {cond})");
    Ok(apply_modifier(base, src.modifier))
}

fn emit_add_with_carry(
    w: &mut WgslWriter,
    opcode: &'static str,
    inst_index: usize,
    dst_sum: &crate::sm4_ir::DstOperand,
    dst_carry: &crate::sm4_ir::DstOperand,
    a: &crate::sm4_ir::SrcOperand,
    b: &crate::sm4_ir::SrcOperand,
    ctx: &EmitCtx<'_>,
) -> Result<(), ShaderTranslateError> {
    let a_expr = emit_src_vec4_u32_int(a, inst_index, opcode, ctx)?;
    let b_expr = emit_src_vec4_u32_int(b, inst_index, opcode, ctx)?;

    // DXBC integer ops operate on raw 32-bit lanes. Model them as per-lane `u32` math and then
    // store the raw bits back into the untyped `vec4<f32>` register file.
    let a_var = format!("{opcode}_a_{inst_index}");
    let b_var = format!("{opcode}_b_{inst_index}");
    let sum_var = format!("{opcode}_sum_{inst_index}");
    let carry_var = format!("{opcode}_carry_{inst_index}");

    w.line(&format!("let {a_var} = {a_expr};"));
    w.line(&format!("let {b_var} = {b_expr};"));
    w.line(&format!("let {sum_var} = {a_var} + {b_var};"));
    w.line(&format!(
        "let {carry_var} = select(vec4<u32>(0u), vec4<u32>(1u), {sum_var} < {a_var});"
    ));

    let sum_bits = format!("bitcast<vec4<f32>>({sum_var})");
    emit_write_masked(
        w,
        dst_sum.reg,
        dst_sum.mask,
        sum_bits,
        inst_index,
        opcode,
        ctx,
    )?;

    let carry_bits = format!("bitcast<vec4<f32>>({carry_var})");
    emit_write_masked(
        w,
        dst_carry.reg,
        dst_carry.mask,
        carry_bits,
        inst_index,
        opcode,
        ctx,
    )?;

    Ok(())
}

fn emit_sub_with_borrow(
    w: &mut WgslWriter,
    opcode: &'static str,
    inst_index: usize,
    dst_diff: &crate::sm4_ir::DstOperand,
    dst_borrow: &crate::sm4_ir::DstOperand,
    a: &crate::sm4_ir::SrcOperand,
    b: &crate::sm4_ir::SrcOperand,
    ctx: &EmitCtx<'_>,
) -> Result<(), ShaderTranslateError> {
    let a_expr = emit_src_vec4_u32_int(a, inst_index, opcode, ctx)?;
    let b_expr = emit_src_vec4_u32_int(b, inst_index, opcode, ctx)?;

    let a_var = format!("{opcode}_a_{inst_index}");
    let b_var = format!("{opcode}_b_{inst_index}");
    let diff_var = format!("{opcode}_diff_{inst_index}");
    let borrow_var = format!("{opcode}_borrow_{inst_index}");

    w.line(&format!("let {a_var} = {a_expr};"));
    w.line(&format!("let {b_var} = {b_expr};"));
    w.line(&format!("let {diff_var} = {a_var} - {b_var};"));
    w.line(&format!(
        "let {borrow_var} = select(vec4<u32>(0u), vec4<u32>(1u), {a_var} < {b_var});"
    ));

    let diff_bits = format!("bitcast<vec4<f32>>({diff_var})");
    emit_write_masked(
        w,
        dst_diff.reg,
        dst_diff.mask,
        diff_bits,
        inst_index,
        opcode,
        ctx,
    )?;

    let borrow_bits = format!("bitcast<vec4<f32>>({borrow_var})");
    emit_write_masked(
        w,
        dst_borrow.reg,
        dst_borrow.mask,
        borrow_bits,
        inst_index,
        opcode,
        ctx,
    )?;

    Ok(())
}

fn emit_write_masked(
    w: &mut WgslWriter,
    dst: RegisterRef,
    mask: WriteMask,
    rhs: String,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<(), ShaderTranslateError> {
    let dst_expr = match dst.file {
        RegFile::Temp => format!("r{}", dst.index),
        RegFile::Output => format!("o{}", dst.index),
        RegFile::OutputDepth => {
            let depth_reg =
                ctx.io
                    .ps_sv_depth_register
                    .ok_or(ShaderTranslateError::MissingSignature(
                        "pixel output SV_Depth",
                    ))?;
            format!("o{depth_reg}")
        }
        RegFile::Input => {
            return Err(ShaderTranslateError::UnsupportedInstruction {
                inst_index,
                opcode: opcode.to_owned(),
            });
        }
    };

    // Mask is 4 bits.
    let mask_bits = mask.0 & 0xF;
    if mask_bits == 0 {
        return Err(ShaderTranslateError::UnsupportedWriteMask {
            inst_index,
            opcode,
            mask,
        });
    }

    let comps = [('x', 1u8), ('y', 2u8), ('z', 4u8), ('w', 8u8)];
    for (c, bit) in comps {
        if (mask_bits & bit) != 0 {
            w.line(&format!("{dst_expr}.{c} = ({rhs}).{c};"));
        }
    }
    Ok(())
}

struct WgslWriter {
    out: String,
    indent: usize,
}

impl WgslWriter {
    fn new() -> Self {
        Self {
            out: String::new(),
            indent: 0,
        }
    }

    fn indent(&mut self) {
        self.indent += 2;
    }

    fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(2);
    }

    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push(' ');
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn finish(self) -> String {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_dxbc::test_utils as dxbc_test_utils;

    fn assert_wgsl_validates(wgsl: &str) {
        let module = naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("generated WGSL failed to validate");
    }

    #[test]
    fn compute_system_value_builtins_reflect_and_emit_wgsl() {
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask(0b0111),
                    sys_value: D3D_NAME_DISPATCH_THREAD_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 1,
                    mask: WriteMask(0b0111),
                    sys_value: D3D_NAME_GROUP_THREAD_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 2,
                    mask: WriteMask(0b0111),
                    sys_value: D3D_NAME_GROUP_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 3,
                    mask: WriteMask::X,
                    sys_value: D3D_NAME_GROUP_INDEX,
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 1,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 1,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 2,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 2,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 3,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 3,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let translated = translate_cs(&module, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);

        assert!(translated
            .wgsl
            .contains("@builtin(global_invocation_id) global_invocation_id: vec3<u32>"));
        assert!(translated
            .wgsl
            .contains("@builtin(local_invocation_id) local_invocation_id: vec3<u32>"));
        assert!(translated
            .wgsl
            .contains("@builtin(workgroup_id) workgroup_id: vec3<u32>"));
        assert!(translated
            .wgsl
            .contains("@builtin(local_invocation_index) local_invocation_index: u32"));

        let v0 = translated
            .reflection
            .inputs
            .iter()
            .find(|p| p.register == 0)
            .expect("missing v0 reflection");
        assert_eq!(v0.builtin, Some(Builtin::GlobalInvocationId));
        assert_eq!(v0.location, None);

        let v1 = translated
            .reflection
            .inputs
            .iter()
            .find(|p| p.register == 1)
            .expect("missing v1 reflection");
        assert_eq!(v1.builtin, Some(Builtin::LocalInvocationId));
        assert_eq!(v1.location, None);

        let v2 = translated
            .reflection
            .inputs
            .iter()
            .find(|p| p.register == 2)
            .expect("missing v2 reflection");
        assert_eq!(v2.builtin, Some(Builtin::WorkgroupId));
        assert_eq!(v2.location, None);

        let v3 = translated
            .reflection
            .inputs
            .iter()
            .find(|p| p.register == 3)
            .expect("missing v3 reflection");
        assert_eq!(v3.builtin, Some(Builtin::LocalInvocationIndex));
        assert_eq!(v3.location, None);
    }

    #[test]
    fn resource_usage_bindings_compute_visibility_is_compute() {
        let mut cbuffers = BTreeMap::new();
        cbuffers.insert(0, 1);
        let mut uav_buffers = BTreeSet::new();
        uav_buffers.insert(0);

        let usage = ResourceUsage {
            cbuffers,
            textures: BTreeSet::new(),
            srv_buffers: BTreeSet::new(),
            samplers: BTreeSet::new(),
            uav_buffers,
        };

        let bindings = usage.bindings(ShaderStage::Compute);
        assert_eq!(bindings.len(), 2);
        assert!(
            bindings
                .iter()
                .all(|b| b.visibility == wgpu::ShaderStages::COMPUTE),
            "expected compute-stage bindings to be visible to the compute stage"
        );
    }

    #[test]
    fn uav_buffer_binding_numbers_use_uav_base_offset() {
        let max_slot = MAX_UAV_SLOTS - 1;
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: vec![
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: crate::sm4_ir::BufferKind::Raw,
                },
                Sm4Decl::UavBuffer {
                    slot: max_slot,
                    stride: 0,
                    kind: crate::sm4_ir::BufferKind::Raw,
                },
            ],
            instructions: Vec::new(),
        };

        let bindings = reflect_resource_bindings(&module).unwrap();
        let uav_bindings: BTreeMap<u32, Binding> = bindings
            .into_iter()
            .filter_map(|b| match b.kind {
                BindingKind::UavBuffer { slot } => Some((slot, b)),
                _ => None,
            })
            .collect();

        let b0 = uav_bindings.get(&0).expect("u0 binding");
        assert_eq!(b0.group, 2);
        assert_eq!(b0.visibility, wgpu::ShaderStages::COMPUTE);
        assert_eq!(b0.binding, BINDING_BASE_UAV);

        let bmax = uav_bindings.get(&max_slot).expect("u(max) binding");
        assert_eq!(bmax.binding, BINDING_BASE_UAV + max_slot);
    }

    #[test]
    fn uav_slot_out_of_range_triggers_error() {
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: vec![Sm4Decl::UavBuffer {
                slot: MAX_UAV_SLOTS,
                stride: 0,
                kind: crate::sm4_ir::BufferKind::Raw,
            }],
            instructions: Vec::new(),
        };

        let err = scan_resources(&module, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::ResourceSlotOutOfRange {
                kind: "uav_buffer",
                slot,
                max,
            } if slot == MAX_UAV_SLOTS && max == MAX_UAV_SLOTS - 1
        ));
    }

    #[test]
    fn srv_buffer_binding_numbers_use_texture_base_offset() {
        let max_slot = MAX_TEXTURE_SLOTS - 1;
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ResourceBuffer {
                    slot: 0,
                    stride: 0,
                    kind: crate::sm4_ir::BufferKind::Raw,
                },
                Sm4Decl::ResourceBuffer {
                    slot: max_slot,
                    stride: 0,
                    kind: crate::sm4_ir::BufferKind::Raw,
                },
            ],
            instructions: Vec::new(),
        };

        let bindings = reflect_resource_bindings(&module).unwrap();
        let srv_bindings: BTreeMap<u32, Binding> = bindings
            .into_iter()
            .filter_map(|b| match b.kind {
                BindingKind::SrvBuffer { slot } => Some((slot, b)),
                _ => None,
            })
            .collect();

        let b0 = srv_bindings.get(&0).expect("t0 buffer binding");
        assert_eq!(b0.group, 2);
        assert_eq!(b0.visibility, wgpu::ShaderStages::COMPUTE);
        assert_eq!(b0.binding, BINDING_BASE_TEXTURE);

        let bmax = srv_bindings.get(&max_slot).expect("t(max) buffer binding");
        assert_eq!(bmax.binding, BINDING_BASE_TEXTURE + max_slot);
    }

    fn minimal_module(instructions: Vec<Sm4Inst>) -> Sm4Module {
        Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: Vec::new(),
            instructions,
        }
    }

    fn dummy_dst() -> crate::sm4_ir::DstOperand {
        crate::sm4_ir::DstOperand {
            reg: RegisterRef {
                file: RegFile::Temp,
                index: 0,
            },
            mask: WriteMask::XYZW,
            saturate: false,
        }
    }

    fn dummy_coord() -> crate::sm4_ir::SrcOperand {
        crate::sm4_ir::SrcOperand {
            kind: SrcKind::ImmediateF32([0; 4]),
            swizzle: Swizzle::XYZW,
            modifier: OperandModifier::None,
        }
    }

    fn sig_param(semantic_name: &str, semantic_index: u32, register: u32) -> DxbcSignatureParameter {
        DxbcSignatureParameter {
            semantic_name: semantic_name.to_owned(),
            semantic_index,
            system_value_type: 0,
            component_type: 0,
            register,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: 0,
            min_precision: 0,
        }
    }

    #[test]
    fn texture_slot_128_triggers_error() {
        let module = minimal_module(vec![Sm4Inst::Sample {
            dst: dummy_dst(),
            coord: dummy_coord(),
            texture: crate::sm4_ir::TextureRef { slot: 128 },
            sampler: crate::sm4_ir::SamplerRef { slot: 0 },
        }]);

        let err = scan_resources(&module, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::ResourceSlotOutOfRange {
                kind: "texture",
                slot: 128,
                max: 127
            }
        ));
    }

    #[test]
    fn sampler_slot_16_triggers_error() {
        let module = minimal_module(vec![Sm4Inst::Sample {
            dst: dummy_dst(),
            coord: dummy_coord(),
            texture: crate::sm4_ir::TextureRef { slot: 0 },
            sampler: crate::sm4_ir::SamplerRef { slot: 16 },
        }]);

        let err = scan_resources(&module, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::ResourceSlotOutOfRange {
                kind: "sampler",
                slot: 16,
                max: 15
            }
        ));
    }

    #[test]
    fn cbuffer_slot_14_triggers_error() {
        let module = minimal_module(vec![Sm4Inst::Mov {
            dst: dummy_dst(),
            src: crate::sm4_ir::SrcOperand {
                kind: SrcKind::ConstantBuffer { slot: 14, reg: 0 },
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }]);

        let err = scan_resources(&module, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::ResourceSlotOutOfRange {
                kind: "cbuffer",
                slot: 14,
                max: 13
            }
        ));
    }

    #[test]
    fn compute_dispatch_thread_id_is_mapped_to_global_invocation_id_builtin() {
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_DISPATCH_THREAD_ID,
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        // The compute path doesn't depend on the DXBC container today, but the
        // public API requires one; construct the smallest valid DXBC header.
        let dxbc_bytes = dxbc_test_utils::build_container(&[]);

        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = ShaderSignatures::default();

        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("@compute"));
        assert!(translated.wgsl.contains("@builtin(global_invocation_id)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.global_invocation_id.x)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.global_invocation_id.y)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.global_invocation_id.z)"));
    }

    #[test]
    fn compute_group_thread_id_is_mapped_to_local_invocation_id_builtin() {
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_THREAD_ID,
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dxbc_test_utils::build_container(&[]);

        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = ShaderSignatures::default();
        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("@builtin(local_invocation_id)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.local_invocation_id.x)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.local_invocation_id.y)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.local_invocation_id.z)"));
    }

    #[test]
    fn compute_group_id_is_mapped_to_workgroup_id_builtin_and_unused_sivs_are_omitted() {
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                // Declare an unused builtin to ensure we only include required inputs.
                Sm4Decl::InputSiv {
                    reg: 1,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_DISPATCH_THREAD_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_ID,
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dxbc_test_utils::build_container(&[]);

        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = ShaderSignatures::default();
        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("@builtin(workgroup_id)"));
        assert!(!translated.wgsl.contains("@builtin(global_invocation_id)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.workgroup_id.x)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.workgroup_id.y)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.workgroup_id.z)"));
    }

    #[test]
    fn compute_group_index_is_mapped_to_local_invocation_index_builtin() {
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_INDEX,
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dxbc_test_utils::build_container(&[]);

        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = ShaderSignatures::default();
        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("@builtin(local_invocation_index)"));
        assert!(translated
            .wgsl
            .contains("bitcast<f32>(input.local_invocation_index)"));
    }

    #[test]
    fn compute_uses_declared_thread_group_size_for_workgroup_size_attribute() {
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 64, y: 2, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_DISPATCH_THREAD_ID,
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dxbc_test_utils::build_container(&[]);

        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = ShaderSignatures::default();

        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

        assert_wgsl_validates(&translated.wgsl);
        assert!(translated
            .wgsl
            .contains("@compute @workgroup_size(64, 2, 1)"));
    }

    #[test]
    fn compute_sample_uses_texture_sample_level() {
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
            instructions: vec![
                Sm4Inst::Sample {
                    dst: dummy_dst(),
                    coord: dummy_coord(),
                    texture: crate::sm4_ir::TextureRef { slot: 0 },
                    sampler: crate::sm4_ir::SamplerRef { slot: 0 },
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dxbc_test_utils::build_container(&[]);

        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = ShaderSignatures::default();

        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

        assert_wgsl_validates(&translated.wgsl);
        assert!(
            translated.wgsl.contains("textureSampleLevel("),
            "{}",
            translated.wgsl
        );
        assert!(
            !translated.wgsl.contains("textureSample("),
            "{}",
            translated.wgsl
        );
    }

    #[test]
    fn unknown_opcode_error_uses_friendly_name_when_known() {
        // Force an "unknown opcode" through the emitter path and ensure the resulting error
        // message uses `opcode_name()` instead of a raw `opcode_<n>` placeholder.
        let module = minimal_module(vec![Sm4Inst::Unknown {
            opcode: crate::sm4::opcode::OPCODE_MOVC,
        }]);

        let io = IoMaps::default();
        let resources = ResourceUsage {
            cbuffers: BTreeMap::new(),
            textures: BTreeSet::new(),
            srv_buffers: BTreeSet::new(),
            samplers: BTreeSet::new(),
            uav_buffers: BTreeSet::new(),
        };
        let ctx = EmitCtx {
            stage: ShaderStage::Pixel,
            io: &io,
            resources: &resources,
        };

        let mut w = WgslWriter::new();
        let err = emit_instructions(&mut w, &module, &ctx).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("movc"),
            "expected friendly opcode name in error message, got: {msg}"
        );
    }

    #[test]
    fn semantic_to_d3d_name_recognizes_gs_builtins() {
        assert_eq!(
            semantic_to_d3d_name("SV_PrimitiveID"),
            Some(D3D_NAME_PRIMITIVE_ID)
        );
        assert_eq!(
            semantic_to_d3d_name("SV_PRIMITIVEID"),
            Some(D3D_NAME_PRIMITIVE_ID)
        );
        assert_eq!(
            semantic_to_d3d_name("sv_gsinstanceid"),
            Some(D3D_NAME_GS_INSTANCE_ID)
        );
    }

    #[test]
    fn builtin_from_d3d_name_maps_gs_builtins() {
        assert_eq!(
            builtin_from_d3d_name(D3D_NAME_PRIMITIVE_ID),
            Some(Builtin::PrimitiveIndex)
        );
        assert_eq!(
            builtin_from_d3d_name(D3D_NAME_GS_INSTANCE_ID),
            Some(Builtin::GsInstanceIndex)
        );
    }

    #[test]
    fn pixel_shader_sv_target1_only_emits_location_1() {
        let module = minimal_module(vec![Sm4Inst::Ret]);
        let isgn = DxbcSignature { parameters: vec![] };
        let osgn = DxbcSignature {
            parameters: vec![sig_param("SV_Target", 1, 3)],
        };

        let translated = translate_ps(&module, &isgn, &osgn, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("@location(1)"));
        assert!(translated.wgsl.contains("target1"));
        assert!(translated.wgsl.contains("o3"));

        let reflected = translated
            .reflection
            .outputs
            .iter()
            .find(|o| o.semantic_index == 1)
            .expect("reflected output");
        assert_eq!(reflected.location, Some(1));
    }

    #[test]
    fn pixel_shader_sv_target0_and_1_emit_both_locations() {
        let module = minimal_module(vec![Sm4Inst::Ret]);
        let isgn = DxbcSignature { parameters: vec![] };
        let osgn = DxbcSignature {
            parameters: vec![sig_param("SV_Target", 0, 0), sig_param("SV_Target", 1, 1)],
        };

        let translated = translate_ps(&module, &isgn, &osgn, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("@location(0)"));
        assert!(translated.wgsl.contains("@location(1)"));

        let mut locations: Vec<u32> = translated
            .reflection
            .outputs
            .iter()
            .filter_map(|o| o.location)
            .collect();
        locations.sort_unstable();
        assert_eq!(locations, vec![0, 1]);
    }

    #[test]
    fn pixel_shader_legacy_color_is_treated_as_sv_target() {
        let module = minimal_module(vec![Sm4Inst::Ret]);
        let isgn = DxbcSignature { parameters: vec![] };
        let osgn = DxbcSignature {
            parameters: vec![sig_param("COLOR", 1, 4)],
        };

        let translated = translate_ps(&module, &isgn, &osgn, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("@location(1)"));
        assert!(translated.wgsl.contains("o4"));

        let reflected = translated
            .reflection
            .outputs
            .iter()
            .find(|o| o.semantic_index == 1)
            .expect("reflected output");
        assert_eq!(reflected.location, Some(1));
    }

    #[test]
    fn malformed_control_flow_endif_without_if_triggers_error() {
        let isgn = DxbcSignature { parameters: Vec::new() };
        let osgn = DxbcSignature {
            parameters: vec![DxbcSignatureParameter {
                semantic_name: "SV_Target".to_owned(),
                semantic_index: 0,
                system_value_type: 0,
                component_type: 0,
                register: 0,
                mask: 0b1111,
                read_write_mask: 0b1111,
                stream: 0,
                min_precision: 0,
            }],
        };

        let module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: Vec::new(),
            instructions: vec![Sm4Inst::EndIf, Sm4Inst::Ret],
        };

        let err = translate_ps(&module, &isgn, &osgn, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::MalformedControlFlow { inst_index: 0, .. }
        ));
    }
}
