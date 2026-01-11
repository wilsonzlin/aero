use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use crate::signature::{DxbcSignature, DxbcSignatureParameter, ShaderSignatures};
use crate::sm4::ShaderStage;
use crate::sm4_ir::{
    OperandModifier, RegFile, RegisterRef, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, Swizzle, WriteMask,
};
use crate::DxbcFile;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Position,
    VertexIndex,
    InstanceIndex,
    FrontFacing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub group: u32,
    pub binding: u32,
    pub visibility: wgpu::ShaderStages,
    pub kind: BindingKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingKind {
    ConstantBuffer { slot: u32, reg_count: u32 },
    Texture2D { slot: u32 },
    Sampler { slot: u32 },
}

#[derive(Debug)]
pub enum ShaderTranslateError {
    UnsupportedStage(ShaderStage),
    MissingSignature(&'static str),
    SignatureMissingRegister {
        io: &'static str,
        register: u32,
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
    PixelShaderMissingSvTarget0,
}

impl fmt::Display for ShaderTranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShaderTranslateError::UnsupportedStage(stage) => write!(f, "unsupported shader stage {stage:?}"),
            ShaderTranslateError::MissingSignature(name) => write!(f, "DXBC missing required signature chunk {name}"),
            ShaderTranslateError::SignatureMissingRegister { io, register } => {
                write!(f, "{io} signature does not declare register {register}")
            }
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
            ShaderTranslateError::PixelShaderMissingSvTarget0 => {
                write!(f, "pixel shader output signature is missing SV_Target0")
            }
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
    _dxbc: &DxbcFile<'_>,
    module: &Sm4Module,
    signatures: &ShaderSignatures,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    let isgn = signatures
        .isgn
        .as_ref()
        .ok_or(ShaderTranslateError::MissingSignature("ISGN"))?;
    let osgn = signatures
        .osgn
        .as_ref()
        .ok_or(ShaderTranslateError::MissingSignature("OSGN"))?;

    match module.stage {
        ShaderStage::Vertex => translate_vs(module, isgn, osgn),
        ShaderStage::Pixel => translate_ps(module, isgn, osgn),
        other => Err(ShaderTranslateError::UnsupportedStage(other)),
    }
}

fn translate_vs(
    module: &Sm4Module,
    isgn: &DxbcSignature,
    osgn: &DxbcSignature,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    let io = build_io_maps(module, isgn, osgn)?;
    let resources = scan_resources(module);

    let reflection = ShaderReflection {
        inputs: io.inputs_reflection(),
        outputs: io.outputs_reflection_vertex(),
        bindings: resources.bindings(ShaderStage::Vertex),
    };

    let mut w = WgslWriter::new();

    resources.emit_decls(&mut w)?;
    io.emit_vs_structs(&mut w)?;

    w.line("@vertex");
    w.line("fn vs_main(input: VsIn) -> VsOut {");
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
) -> Result<ShaderTranslation, ShaderTranslateError> {
    let io = build_io_maps(module, isgn, osgn)?;
    let resources = scan_resources(module);

    let reflection = ShaderReflection {
        inputs: io.inputs_reflection(),
        outputs: io.outputs_reflection_pixel(),
        bindings: resources.bindings(ShaderStage::Pixel),
    };

    let mut w = WgslWriter::new();

    resources.emit_decls(&mut w)?;
    io.emit_ps_structs(&mut w)?;

    w.line("@fragment");
    w.line("fn fs_main(input: PsIn) -> @location(0) vec4<f32> {");
    w.indent();
    w.line("");
    emit_temp_and_output_decls(&mut w, module, &io)?;

    let ctx = EmitCtx {
        stage: ShaderStage::Pixel,
        io: &io,
        resources: &resources,
    };
    emit_instructions(&mut w, module, &ctx)?;

    let target_reg = io
        .ps_sv_target0_register
        .ok_or(ShaderTranslateError::PixelShaderMissingSvTarget0)?;
    w.line("");
    w.line(&format!("return o{target_reg};"));
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
    let mut inputs = BTreeMap::new();
    for p in &isgn.parameters {
        inputs.insert(p.register, ParamInfo::from_sig_param("input", p)?);
    }

    let mut outputs = BTreeMap::new();
    for p in &osgn.parameters {
        outputs.insert(p.register, ParamInfo::from_sig_param("output", p)?);
    }

    let mut vs_position_reg = None;
    for p in &osgn.parameters {
        if is_sv_position_param(p) {
            vs_position_reg = Some(p.register);
            break;
        }
    }

    let mut ps_position_reg = None;
    for p in &isgn.parameters {
        if is_sv_position_param(p) {
            ps_position_reg = Some(p.register);
            break;
        }
    }

    let mut ps_sv_target0_reg = None;
    for p in &osgn.parameters {
        if is_sv_target_param(p) && p.semantic_index == 0 {
            ps_sv_target0_reg = Some(p.register);
            break;
        }
    }

    let mut vs_vertex_id_reg = None;
    let mut vs_instance_id_reg = None;
    let mut ps_front_facing_reg = None;

    for p in &isgn.parameters {
        if is_sv_vertex_id_param(p) {
            vs_vertex_id_reg = Some(p.register);
        }
        if is_sv_instance_id_param(p) {
            vs_instance_id_reg = Some(p.register);
        }
        if is_sv_is_front_face_param(p) {
            ps_front_facing_reg = Some(p.register);
        }
    }

    // Merge declaration-driven system value bindings. These cover the case where
    // the signature's `system_value_type` is unset (0) and the semantic name
    // isn't the canonical `SV_*` string, while the token stream uses
    // `dcl_input_siv` / `dcl_output_siv`.
    for decl in &module.decls {
        match decl {
            Sm4Decl::InputSiv { reg, sys_value, .. } => match *sys_value {
                D3D_NAME_VERTEX_ID => vs_vertex_id_reg = Some(*reg),
                D3D_NAME_INSTANCE_ID => vs_instance_id_reg = Some(*reg),
                D3D_NAME_IS_FRONT_FACE => ps_front_facing_reg = Some(*reg),
                D3D_NAME_POSITION => ps_position_reg = Some(*reg),
                _ => {}
            },
            Sm4Decl::OutputSiv { reg, sys_value, .. } => match *sys_value {
                D3D_NAME_POSITION => vs_position_reg = Some(*reg),
                D3D_NAME_TARGET => {
                    // Assume the first declared target corresponds to SV_Target0.
                    ps_sv_target0_reg.get_or_insert(*reg);
                }
                _ => {}
            },
            _ => {}
        }
    }

    Ok(IoMaps {
        inputs,
        outputs,
        vs_position_register: vs_position_reg,
        ps_position_register: ps_position_reg,
        ps_sv_target0_register: ps_sv_target0_reg,
        vs_vertex_id_register: vs_vertex_id_reg,
        vs_instance_id_register: vs_instance_id_reg,
        ps_front_facing_register: ps_front_facing_reg,
    })
}

#[derive(Debug, Clone)]
struct ParamInfo {
    param: DxbcSignatureParameter,
    wgsl_ty: &'static str,
    component_count: usize,
    components: [u8; 4],
}

impl ParamInfo {
    fn from_sig_param(
        io: &'static str,
        param: &DxbcSignatureParameter,
    ) -> Result<Self, ShaderTranslateError> {
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

        Ok(Self {
            param: param.clone(),
            wgsl_ty,
            component_count: count,
            components: comps,
        })
    }

    fn field_name(&self, prefix: char) -> String {
        format!("{prefix}{}", self.param.register)
    }
}

#[derive(Debug, Clone)]
struct IoMaps {
    inputs: BTreeMap<u32, ParamInfo>,
    outputs: BTreeMap<u32, ParamInfo>,
    vs_position_register: Option<u32>,
    ps_position_register: Option<u32>,
    ps_sv_target0_register: Option<u32>,
    vs_vertex_id_register: Option<u32>,
    vs_instance_id_register: Option<u32>,
    ps_front_facing_register: Option<u32>,
}

impl IoMaps {
    fn inputs_reflection(&self) -> Vec<IoParam> {
        self.inputs
            .values()
            .map(|p| {
                let builtin = self.input_builtin(p.param.register, &p.param.semantic_name);
                IoParam {
                    semantic_name: p.param.semantic_name.clone(),
                    semantic_index: p.param.semantic_index,
                    register: p.param.register,
                    location: builtin.is_none().then_some(p.param.register),
                    builtin,
                    mask: p.param.mask,
                }
            })
            .collect()
    }

    fn outputs_reflection_vertex(&self) -> Vec<IoParam> {
        let pos_reg = self.vs_position_register;
        self.outputs
            .values()
            .map(|p| {
                let builtin = pos_reg
                    .filter(|&r| r == p.param.register)
                    .or_else(|| is_sv_position(&p.param.semantic_name).then_some(p.param.register))
                    .map(|_| Builtin::Position);
                IoParam {
                    semantic_name: p.param.semantic_name.clone(),
                    semantic_index: p.param.semantic_index,
                    register: p.param.register,
                    location: builtin.is_none().then_some(p.param.register),
                    builtin,
                    mask: p.param.mask,
                }
            })
            .collect()
    }

    fn outputs_reflection_pixel(&self) -> Vec<IoParam> {
        self.outputs
            .values()
            .map(|p| {
                let is_target = is_sv_target(&p.param.semantic_name) || p.param.system_value_type == D3D_NAME_TARGET;
                IoParam {
                    semantic_name: p.param.semantic_name.clone(),
                    semantic_index: p.param.semantic_index,
                    register: p.param.register,
                    location: is_target.then_some(p.param.register),
                    builtin: None,
                    mask: p.param.mask,
                }
            })
            .collect()
    }

    fn emit_vs_structs(&self, w: &mut WgslWriter) -> Result<(), ShaderTranslateError> {
        w.line("struct VsIn {");
        w.indent();

        if self.vs_vertex_id_register.is_some() {
            w.line("@builtin(vertex_index) vertex_id: u32,");
        }
        if self.vs_instance_id_register.is_some() {
            w.line("@builtin(instance_index) instance_id: u32,");
        }

        for p in self.inputs.values() {
            if Some(p.param.register) == self.vs_vertex_id_register
                || Some(p.param.register) == self.vs_instance_id_register
            {
                continue;
            }
            w.line(&format!(
                "@location({}) {}: {},",
                p.param.register,
                p.field_name('v'),
                p.wgsl_ty
            ));
        }
        w.dedent();
        w.line("};");
        w.line("");

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
                p.wgsl_ty
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
        if self.ps_front_facing_register.is_some() {
            w.line("@builtin(front_facing) front_facing: bool,");
        }
        for p in self.inputs.values() {
            if Some(p.param.register) == self.ps_position_register
                || Some(p.param.register) == self.ps_front_facing_register
            {
                continue;
            }
            w.line(&format!(
                "@location({}) {}: {},",
                p.param.register,
                p.field_name('v'),
                p.wgsl_ty
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
        w.line(&format!("out.pos = o{pos_reg};"));
        for p in self.outputs.values() {
            if p.param.register == pos_reg {
                continue;
            }
            let src = extract_from_vec4(&format!("o{}", p.param.register), p);
            w.line(&format!("out.{} = {src};", p.field_name('o')));
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
                    return Ok(expand_to_vec4("f32(input.vertex_id)", p));
                }
                if Some(reg) == self.vs_instance_id_register {
                    let p = self.inputs.get(&reg).ok_or(
                        ShaderTranslateError::SignatureMissingRegister {
                            io: "input",
                            register: reg,
                        },
                    )?;
                    return Ok(expand_to_vec4("f32(input.instance_id)", p));
                }
                let p = self.inputs.get(&reg).ok_or(
                    ShaderTranslateError::SignatureMissingRegister {
                        io: "input",
                        register: reg,
                    },
                )?;
                Ok(expand_to_vec4(&format!("input.{}", p.field_name('v')), p))
            }
            ShaderStage::Pixel => {
                if Some(reg) == self.ps_position_register {
                    return Ok("input.pos".to_owned());
                }
                if Some(reg) == self.ps_front_facing_register {
                    let p = self.inputs.get(&reg).ok_or(
                        ShaderTranslateError::SignatureMissingRegister {
                            io: "input",
                            register: reg,
                        },
                    )?;
                    return Ok(expand_to_vec4(
                        "select(0.0, 1.0, input.front_facing)",
                        p,
                    ));
                }
                let p = self.inputs.get(&reg).ok_or(
                    ShaderTranslateError::SignatureMissingRegister {
                        io: "input",
                        register: reg,
                    },
                )?;
                Ok(expand_to_vec4(&format!("input.{}", p.field_name('v')), p))
            }
            _ => Err(ShaderTranslateError::UnsupportedStage(stage)),
        }
    }

    fn input_builtin(&self, reg: u32, semantic_name: &str) -> Option<Builtin> {
        if Some(reg) == self.vs_vertex_id_register || is_sv_vertex_id(semantic_name) {
            return Some(Builtin::VertexIndex);
        }
        if Some(reg) == self.vs_instance_id_register || is_sv_instance_id(semantic_name) {
            return Some(Builtin::InstanceIndex);
        }
        if Some(reg) == self.ps_front_facing_register || is_sv_is_front_face(semantic_name) {
            return Some(Builtin::FrontFacing);
        }
        if Some(reg) == self.ps_position_register || is_sv_position(semantic_name) {
            return Some(Builtin::Position);
        }
        None
    }
}

fn is_sv_position(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_Position") || name.eq_ignore_ascii_case("SV_POSITION")
}

fn is_sv_target(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_Target") || name.eq_ignore_ascii_case("SV_TARGET")
}

fn is_sv_vertex_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_VertexID") || name.eq_ignore_ascii_case("SV_VERTEXID")
}

fn is_sv_instance_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_InstanceID") || name.eq_ignore_ascii_case("SV_INSTANCEID")
}

fn is_sv_is_front_face(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_IsFrontFace") || name.eq_ignore_ascii_case("SV_ISFRONTFACE")
}

fn is_sv_position_param(p: &DxbcSignatureParameter) -> bool {
    is_sv_position(&p.semantic_name) || p.system_value_type == D3D_NAME_POSITION
}

fn is_sv_target_param(p: &DxbcSignatureParameter) -> bool {
    is_sv_target(&p.semantic_name) || p.system_value_type == D3D_NAME_TARGET
}

fn is_sv_vertex_id_param(p: &DxbcSignatureParameter) -> bool {
    is_sv_vertex_id(&p.semantic_name) || p.system_value_type == D3D_NAME_VERTEX_ID
}

fn is_sv_instance_id_param(p: &DxbcSignatureParameter) -> bool {
    is_sv_instance_id(&p.semantic_name) || p.system_value_type == D3D_NAME_INSTANCE_ID
}

fn is_sv_is_front_face_param(p: &DxbcSignatureParameter) -> bool {
    is_sv_is_front_face(&p.semantic_name) || p.system_value_type == D3D_NAME_IS_FRONT_FACE
}

// `D3D_NAME` system value identifiers we need for builtin mapping.
const D3D_NAME_POSITION: u32 = 1;
const D3D_NAME_VERTEX_ID: u32 = 6;
const D3D_NAME_INSTANCE_ID: u32 = 8;
const D3D_NAME_IS_FRONT_FACE: u32 = 9;
const D3D_NAME_TARGET: u32 = 64;

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
    for dst_comp in 0..4usize {
        let want = p
            .components
            .iter()
            .take(p.component_count)
            .any(|&c| c as usize == dst_comp);
        if want {
            out[dst_comp] = src[next].clone();
            next += 1;
        } else if dst_comp == 3 {
            out[dst_comp] = "1.0".to_owned();
        } else {
            out[dst_comp] = "0.0".to_owned();
        }
    }

    format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3])
}

fn extract_from_vec4(reg_expr: &str, p: &ParamInfo) -> String {
    let mut parts = Vec::<String>::with_capacity(p.component_count);
    for &c in p.components.iter().take(p.component_count) {
        parts.push(format!("{reg_expr}.{}", component_char(c)));
    }
    match p.component_count {
        1 => parts[0].clone(),
        2 => format!("vec2<f32>({}, {})", parts[0], parts[1]),
        3 => format!("vec3<f32>({}, {}, {})", parts[0], parts[1], parts[2]),
        4 => reg_expr.to_owned(),
        _ => reg_expr.to_owned(),
    }
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

#[derive(Debug, Clone)]
struct ResourceUsage {
    cbuffers: BTreeMap<u32, u32>,
    textures: BTreeSet<u32>,
    samplers: BTreeSet<u32>,
}

impl ResourceUsage {
    fn bindings(&self, stage: ShaderStage) -> Vec<Binding> {
        let visibility = match stage {
            ShaderStage::Vertex => wgpu::ShaderStages::VERTEX,
            ShaderStage::Pixel => wgpu::ShaderStages::FRAGMENT,
            _ => wgpu::ShaderStages::empty(),
        };

        let mut out = Vec::new();
        for (&slot, &reg_count) in &self.cbuffers {
            out.push(Binding {
                group: 0,
                binding: slot,
                visibility,
                kind: BindingKind::ConstantBuffer { slot, reg_count },
            });
        }
        for &slot in &self.textures {
            out.push(Binding {
                group: 1,
                binding: slot,
                visibility,
                kind: BindingKind::Texture2D { slot },
            });
        }
        for &slot in &self.samplers {
            out.push(Binding {
                group: 2,
                binding: slot,
                visibility,
                kind: BindingKind::Sampler { slot },
            });
        }
        out
    }

    fn emit_decls(&self, w: &mut WgslWriter) -> Result<(), ShaderTranslateError> {
        for (&slot, &reg_count) in &self.cbuffers {
            w.line(&format!(
                "struct Cb{slot} {{ regs: array<vec4<u32>, {reg_count}> }};"
            ));
            w.line(&format!(
                "@group(0) @binding({slot}) var<uniform> cb{slot}: Cb{slot};"
            ));
            w.line("");
        }
        for &slot in &self.textures {
            w.line(&format!(
                "@group(1) @binding({slot}) var t{slot}: texture_2d<f32>;"
            ));
        }
        if !self.textures.is_empty() {
            w.line("");
        }
        for &slot in &self.samplers {
            w.line(&format!("@group(2) @binding({slot}) var s{slot}: sampler;"));
        }
        if !self.samplers.is_empty() {
            w.line("");
        }
        Ok(())
    }
}

fn scan_resources(module: &Sm4Module) -> ResourceUsage {
    let mut cbuffers: BTreeMap<u32, u32> = BTreeMap::new();
    let mut textures = BTreeSet::new();
    let mut samplers = BTreeSet::new();
    let mut declared_cbuffer_sizes: BTreeMap<u32, u32> = BTreeMap::new();

    for decl in &module.decls {
        if let Sm4Decl::ConstantBuffer { slot, reg_count } = decl {
            let entry = declared_cbuffer_sizes.entry(*slot).or_insert(0);
            *entry = (*entry).max(*reg_count);
        }
    }

    for inst in &module.instructions {
        let mut scan_src = |src: &crate::sm4_ir::SrcOperand| {
            if let SrcKind::ConstantBuffer { slot, reg } = src.kind {
                let entry = cbuffers.entry(slot).or_insert(0);
                *entry = (*entry).max(reg + 1);
            }
        };
        match inst {
            Sm4Inst::Mov { dst: _, src } => scan_src(src),
            Sm4Inst::Add { dst: _, a, b }
            | Sm4Inst::Mul { dst: _, a, b }
            | Sm4Inst::Dp3 { dst: _, a, b }
            | Sm4Inst::Dp4 { dst: _, a, b }
            | Sm4Inst::Min { dst: _, a, b }
            | Sm4Inst::Max { dst: _, a, b } => {
                scan_src(a);
                scan_src(b);
            }
            Sm4Inst::Mad { dst: _, a, b, c } => {
                scan_src(a);
                scan_src(b);
                scan_src(c);
            }
            Sm4Inst::Rcp { dst: _, src } | Sm4Inst::Rsq { dst: _, src } => scan_src(src),
            Sm4Inst::Sample {
                dst: _,
                coord,
                texture,
                sampler,
            } => {
                scan_src(coord);
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
                scan_src(coord);
                scan_src(lod);
                textures.insert(texture.slot);
                samplers.insert(sampler.slot);
            }
            Sm4Inst::Unknown { .. } => {}
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

    ResourceUsage {
        cbuffers,
        textures,
        samplers,
    }
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
            RegFile::Input => {}
        };

        match inst {
            Sm4Inst::Mov { dst, src } => {
                scan_reg(dst.reg);
                scan_src_regs(src, &mut scan_reg);
            }
            Sm4Inst::Add { dst, a, b }
            | Sm4Inst::Mul { dst, a, b }
            | Sm4Inst::Dp3 { dst, a, b }
            | Sm4Inst::Dp4 { dst, a, b }
            | Sm4Inst::Min { dst, a, b }
            | Sm4Inst::Max { dst, a, b } => {
                scan_reg(dst.reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::Mad { dst, a, b, c } => {
                scan_reg(dst.reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
                scan_src_regs(c, &mut scan_reg);
            }
            Sm4Inst::Rcp { dst, src } | Sm4Inst::Rsq { dst, src } => {
                scan_reg(dst.reg);
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
            Sm4Inst::Unknown { .. } => {}
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
    if let Some(ps_target) = io.ps_sv_target0_register {
        outputs.insert(ps_target);
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
    for (inst_index, inst) in module.instructions.iter().enumerate() {
        let maybe_saturate = |dst: &crate::sm4_ir::DstOperand, expr: String| {
            if dst.saturate {
                format!("clamp(({expr}), vec4<f32>(0.0), vec4<f32>(1.0))")
            } else {
                expr
            }
        };

        match inst {
            Sm4Inst::Mov { dst, src } => {
                let rhs = emit_src_vec4(src, inst_index, "mov", ctx)?;
                let rhs = maybe_saturate(dst, rhs);
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "mov", ctx)?;
            }
            Sm4Inst::Add { dst, a, b } => {
                let a = emit_src_vec4(a, inst_index, "add", ctx)?;
                let b = emit_src_vec4(b, inst_index, "add", ctx)?;
                let rhs = maybe_saturate(dst, format!("({a}) + ({b})"));
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "add", ctx)?;
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
            Sm4Inst::Sample {
                dst,
                coord,
                texture,
                sampler,
            } => {
                let coord = emit_src_vec4(coord, inst_index, "sample", ctx)?;
                let expr = format!(
                    "textureSample(t{}, s{}, ({coord}).xy)",
                    texture.slot, sampler.slot
                );
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
            Sm4Inst::Unknown { opcode } => {
                return Err(ShaderTranslateError::UnsupportedInstruction {
                    inst_index,
                    opcode: format!("opcode_{opcode}"),
                });
            }
            Sm4Inst::Ret => break,
        }
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
        SrcKind::Register(reg) => match reg.file {
            RegFile::Temp => format!("r{}", reg.index),
            RegFile::Output => format!("o{}", reg.index),
            RegFile::Input => ctx.io.read_input_vec4(ctx.stage, reg.index)?,
        },
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

fn emit_write_masked(
    w: &mut WgslWriter,
    dst: RegisterRef,
    mask: WriteMask,
    rhs: String,
    inst_index: usize,
    opcode: &'static str,
    _ctx: &EmitCtx<'_>,
) -> Result<(), ShaderTranslateError> {
    let dst_expr = match dst.file {
        RegFile::Temp => format!("r{}", dst.index),
        RegFile::Output => format!("o{}", dst.index),
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
