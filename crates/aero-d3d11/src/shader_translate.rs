use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use crate::binding_model::{
    BINDING_BASE_CBUFFER, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, MAX_CBUFFER_SLOTS,
    MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS,
};
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
    ResourceSlotOutOfRange {
        kind: &'static str,
        slot: u32,
        max: u32,
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
            ShaderTranslateError::ResourceSlotOutOfRange { kind, slot, max } => {
                match *kind {
                    "cbuffer" => write!(
                        f,
                        "cbuffer slot {slot} is out of range (max {max}); b# slots map to @binding({BINDING_BASE_CBUFFER} + slot) and must stay below the texture base @binding({BINDING_BASE_TEXTURE})"
                    ),
                    "texture" => write!(
                        f,
                        "texture slot {slot} is out of range (max {max}); t# slots map to @binding({BINDING_BASE_TEXTURE} + slot) and must stay below the sampler base @binding({BINDING_BASE_SAMPLER})"
                    ),
                    "sampler" => write!(
                        f,
                        "sampler slot {slot} is out of range (max {max}); s# slots map to @binding({BINDING_BASE_SAMPLER} + slot)"
                    ),
                    _ => write!(f, "{kind} slot {slot} is out of range (max {max})"),
                }
            }
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

/// Scans a decoded SM4/SM5 module and produces bind group layout entries for the
/// module's declared shader stage.
///
/// Note: Full compute-stage WGSL translation is not implemented yet, but the
/// binding model reserves `@group(2)` for compute resources. This helper is used
/// by tests and is intended to support future compute-stage translation work.
pub fn reflect_resource_bindings(module: &Sm4Module) -> Result<Vec<Binding>, ShaderTranslateError> {
    Ok(scan_resources(module)?.bindings(module.stage))
}

fn translate_vs(
    module: &Sm4Module,
    isgn: &DxbcSignature,
    osgn: &DxbcSignature,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    let io = build_io_maps(module, isgn, osgn)?;
    let resources = scan_resources(module)?;

    let reflection = ShaderReflection {
        inputs: io.inputs_reflection(),
        outputs: io.outputs_reflection_vertex(),
        bindings: resources.bindings(ShaderStage::Vertex),
    };

    let mut w = WgslWriter::new();

    resources.emit_decls(&mut w, ShaderStage::Vertex)?;
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
    let resources = scan_resources(module)?;

    let reflection = ShaderReflection {
        inputs: io.inputs_reflection(),
        outputs: io.outputs_reflection_pixel(),
        bindings: resources.bindings(ShaderStage::Pixel),
    };

    let mut w = WgslWriter::new();

    resources.emit_decls(&mut w, ShaderStage::Pixel)?;
    let ps_has_inputs = !io.inputs.is_empty() || io.ps_position_register.is_some();
    if ps_has_inputs {
        io.emit_ps_structs(&mut w)?;
    }

    w.line("@fragment");
    if ps_has_inputs {
        w.line("fn fs_main(input: PsIn) -> @location(0) vec4<f32> {");
    } else {
        w.line("fn fs_main() -> @location(0) vec4<f32> {");
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

    let mut inputs = BTreeMap::new();
    for p in &isgn.parameters {
        let sys_value = resolve_sys_value_type(p, &input_sivs);
        inputs.insert(
            p.register,
            ParamInfo::from_sig_param("input", p, sys_value)?,
        );
    }

    let mut outputs = BTreeMap::new();
    for p in &osgn.parameters {
        let sys_value = resolve_sys_value_type(p, &output_sivs);
        outputs.insert(
            p.register,
            ParamInfo::from_sig_param("output", p, sys_value)?,
        );
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

    let mut ps_sv_target0_reg = None;
    if module.stage == ShaderStage::Pixel {
        ps_sv_target0_reg = outputs
            .values()
            .find(|p| p.sys_value == Some(D3D_NAME_TARGET) && p.param.semantic_index == 0)
            .map(|p| p.param.register);
        if ps_sv_target0_reg.is_none() {
            // Legacy `COLOR` can stand in for `SV_Target` in some SM4-era shaders.
            ps_sv_target0_reg = osgn
                .parameters
                .iter()
                .find(|p| p.semantic_index == 0 && p.semantic_name.eq_ignore_ascii_case("COLOR"))
                .map(|p| p.register);
            if let Some(reg) = ps_sv_target0_reg {
                if let Some(p) = outputs.get_mut(&reg) {
                    p.sys_value = Some(D3D_NAME_TARGET);
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
            Some(Builtin::VertexIndex) | Some(Builtin::InstanceIndex) => ("u32", 1, [0, 0, 0, 0]),
            Some(Builtin::FrontFacing) => ("bool", 1, [0, 0, 0, 0]),
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
            .map(|p| IoParam {
                semantic_name: p.param.semantic_name.clone(),
                semantic_index: p.param.semantic_index,
                register: p.param.register,
                location: p.builtin.is_none().then_some(p.param.register),
                builtin: p.builtin,
                mask: p.param.mask,
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
                    return Ok(expand_to_vec4("select(0.0, 1.0, input.front_facing)", p));
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
}

const D3D_NAME_POSITION: u32 = 1;
const D3D_NAME_VERTEX_ID: u32 = 6;
const D3D_NAME_INSTANCE_ID: u32 = 8;
const D3D_NAME_IS_FRONT_FACE: u32 = 9;
const D3D_NAME_TARGET: u32 = 64;

fn builtin_from_d3d_name(name: u32) -> Option<Builtin> {
    match name {
        D3D_NAME_POSITION => Some(Builtin::Position),
        D3D_NAME_VERTEX_ID => Some(Builtin::VertexIndex),
        D3D_NAME_INSTANCE_ID => Some(Builtin::InstanceIndex),
        D3D_NAME_IS_FRONT_FACE => Some(Builtin::FrontFacing),
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
    if is_sv_vertex_id(name) {
        return Some(D3D_NAME_VERTEX_ID);
    }
    if is_sv_instance_id(name) {
        return Some(D3D_NAME_INSTANCE_ID);
    }
    if is_sv_is_front_face(name) {
        return Some(D3D_NAME_IS_FRONT_FACE);
    }
    if is_sv_target(name) {
        return Some(D3D_NAME_TARGET);
    }
    None
}

fn is_sv_position(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_Position") || name.eq_ignore_ascii_case("SV_POSITION")
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

fn is_sv_target(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_Target") || name.eq_ignore_ascii_case("SV_TARGET")
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
    fn stage_bind_group(stage: ShaderStage) -> u32 {
        match stage {
            ShaderStage::Vertex => 0,
            ShaderStage::Pixel => 1,
            ShaderStage::Compute => 2,
            _ => 0,
        }
    }

    fn bindings(&self, stage: ShaderStage) -> Vec<Binding> {
        let visibility = match stage {
            ShaderStage::Vertex => wgpu::ShaderStages::VERTEX,
            ShaderStage::Pixel => wgpu::ShaderStages::FRAGMENT,
            ShaderStage::Compute => wgpu::ShaderStages::COMPUTE,
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
        for &slot in &self.samplers {
            out.push(Binding {
                group,
                binding: BINDING_BASE_SAMPLER + slot,
                visibility,
                kind: BindingKind::Sampler { slot },
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
        if !self.textures.is_empty() {
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
        Ok(())
    }
}

fn scan_resources(module: &Sm4Module) -> Result<ResourceUsage, ShaderTranslateError> {
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
    let mut samplers = BTreeSet::new();
    let mut declared_cbuffer_sizes: BTreeMap<u32, u32> = BTreeMap::new();

    for decl in &module.decls {
        if let Sm4Decl::ConstantBuffer { slot, reg_count } = decl {
            let entry = declared_cbuffer_sizes.entry(*slot).or_insert(0);
            *entry = (*entry).max(*reg_count);
        }
    }

    for inst in &module.instructions {
        let mut scan_src = |src: &crate::sm4_ir::SrcOperand| -> Result<(), ShaderTranslateError> {
            if let SrcKind::ConstantBuffer { slot, reg } = src.kind {
                validate_slot("cbuffer", slot, MAX_CBUFFER_SLOTS)?;
                let entry = cbuffers.entry(slot).or_insert(0);
                *entry = (*entry).max(reg + 1);
            }
            Ok(())
        };
        match inst {
            Sm4Inst::Mov { dst: _, src } => scan_src(src)?,
            Sm4Inst::Add { dst: _, a, b }
            | Sm4Inst::Mul { dst: _, a, b }
            | Sm4Inst::Dp3 { dst: _, a, b }
            | Sm4Inst::Dp4 { dst: _, a, b }
            | Sm4Inst::Min { dst: _, a, b }
            | Sm4Inst::Max { dst: _, a, b } => {
                scan_src(a)?;
                scan_src(b)?;
            }
            Sm4Inst::Mad { dst: _, a, b, c } => {
                scan_src(a)?;
                scan_src(b)?;
                scan_src(c)?;
            }
            Sm4Inst::Rcp { dst: _, src } | Sm4Inst::Rsq { dst: _, src } => scan_src(src)?,
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

    Ok(ResourceUsage {
        cbuffers,
        textures,
        samplers,
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
            Sm4Inst::Ld {
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
            Sm4Inst::Ld {
                dst,
                coord,
                texture,
                lod,
            } => {
                // SM4 `ld` / WGSL `textureLoad` operates on integer texel coordinates and an
                // integer mip level.
                //
                // Our internal register model uses `vec4<f32>` for everything, so integer values
                // can show up in two forms:
                // - As exact float values (e.g. when system-value inputs like `SV_VertexID` are
                //   expanded into a float lane).
                // - As raw integer bits (common for real DXBC, where integer ops write integer
                //   bit patterns into the untyped register file).
                //
                // To cover both, we derive an `i32` value from each lane by picking between:
                // - `i32(f32)` (numeric conversion)
                // - `bitcast<i32>(f32)` (bit reinterpretation)
                // based on whether the float value looks like an exact integer.
                let coord_f = emit_src_vec4(coord, inst_index, "ld", ctx)?;
                let coord_i = emit_src_vec4_i32(coord, inst_index, "ld", ctx)?;
                let x = format!(
                    "select(({coord_i}).x, i32(({coord_f}).x), ({coord_f}).x == floor(({coord_f}).x))"
                );
                let y = format!(
                    "select(({coord_i}).y, i32(({coord_f}).y), ({coord_f}).y == floor(({coord_f}).y))"
                );

                let lod_f = emit_src_vec4(lod, inst_index, "ld", ctx)?;
                let lod_i = emit_src_vec4_i32(lod, inst_index, "ld", ctx)?;
                let lod_scalar = format!(
                    "select(({lod_i}).x, i32(({lod_f}).x), ({lod_f}).x == floor(({lod_f}).x))"
                );

                let expr = format!(
                    "textureLoad(t{}, vec2<i32>({x}, {y}), {lod_scalar})",
                    texture.slot
                );
                let expr = maybe_saturate(dst, expr);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ld", ctx)?;
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
                RegFile::Input => ctx.io.read_input_vec4(ctx.stage, reg.index)?,
            };
            format!("bitcast<vec4<i32>>({expr})")
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_usage_bindings_compute_visibility_is_compute() {
        let mut cbuffers = BTreeMap::new();
        cbuffers.insert(0, 1);

        let usage = ResourceUsage {
            cbuffers,
            textures: BTreeSet::new(),
            samplers: BTreeSet::new(),
        };

        let bindings = usage.bindings(ShaderStage::Compute);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].visibility, wgpu::ShaderStages::COMPUTE);
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

    #[test]
    fn texture_slot_128_triggers_error() {
        let module = minimal_module(vec![Sm4Inst::Sample {
            dst: dummy_dst(),
            coord: dummy_coord(),
            texture: crate::sm4_ir::TextureRef { slot: 128 },
            sampler: crate::sm4_ir::SamplerRef { slot: 0 },
        }]);

        let err = scan_resources(&module).unwrap_err();
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

        let err = scan_resources(&module).unwrap_err();
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
    fn cbuffer_slot_32_triggers_error() {
        let module = minimal_module(vec![Sm4Inst::Mov {
            dst: dummy_dst(),
            src: crate::sm4_ir::SrcOperand {
                kind: SrcKind::ConstantBuffer { slot: 32, reg: 0 },
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }]);

        let err = scan_resources(&module).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::ResourceSlotOutOfRange {
                kind: "cbuffer",
                slot: 32,
                max: 31
            }
        ));
    }
}
