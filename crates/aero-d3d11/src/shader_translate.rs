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
    BufferKind, CmpOp, CmpType, ComputeBuiltin, OperandModifier, PredicateDstOperand, RegFile,
    RegisterRef, Sm4CmpOp, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, Swizzle, WriteMask,
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
    /// D3D signature register index (e.g. `v#`, `o#`).
    ///
    /// Most parameters map directly to a register number from the DXBC `ISGN`/`OSGN` signature
    /// tables. However, some builtins do not have an explicit `v#` register (notably SM5 compute
    /// thread-ID operand types like `D3D11_SB_OPERAND_TYPE_INPUT_THREAD_ID`).
    ///
    /// In those cases, Aero assigns a stable *synthetic* register index in a reserved range
    /// (`0xffff_ff00..`) so reflection consumers can still differentiate inputs. When [`Self::builtin`]
    /// is `Some`, callers should prefer that field over relying on the numeric register value.
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
    DomainLocation,
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
    Texture2DArray {
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
    UavTexture2DWriteOnly {
        slot: u32,
        format: StorageTextureFormat,
    },
    /// An expansion-internal storage buffer binding used by compute-based GS/HS/DS emulation.
    ///
    /// These bindings are not part of the D3D11 resource binding model, so they do not map to a
    /// register slot. Instead, they use a reserved binding-number range within `@group(3)` starting
    /// at [`crate::binding_model::BINDING_BASE_INTERNAL`].
    ExpansionStorageBuffer {
        read_only: bool,
    },
}

/// Supported typed UAV storage texture formats.
///
/// This is intentionally limited to formats that can be expressed as WGSL/WGPU storage textures.
#[cfg_attr(target_arch = "wasm32", derive(serde::Deserialize, serde::Serialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageTextureFormat {
    Rgba8Unorm,
    Rgba8Snorm,
    Rgba8Uint,
    Rgba8Sint,
    Rgba16Float,
    Rgba16Uint,
    Rgba16Sint,
    Rg32Float,
    Rg32Uint,
    Rg32Sint,
    Rgba32Float,
    Rgba32Uint,
    Rgba32Sint,
    R32Float,
    R32Uint,
    R32Sint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageTextureValueType {
    F32,
    U32,
    I32,
}

impl StorageTextureFormat {
    pub fn wgsl_format(self) -> &'static str {
        match self {
            StorageTextureFormat::Rgba8Unorm => "rgba8unorm",
            StorageTextureFormat::Rgba8Snorm => "rgba8snorm",
            StorageTextureFormat::Rgba8Uint => "rgba8uint",
            StorageTextureFormat::Rgba8Sint => "rgba8sint",
            StorageTextureFormat::Rgba16Float => "rgba16float",
            StorageTextureFormat::Rgba16Uint => "rgba16uint",
            StorageTextureFormat::Rgba16Sint => "rgba16sint",
            StorageTextureFormat::Rg32Float => "rg32float",
            StorageTextureFormat::Rg32Uint => "rg32uint",
            StorageTextureFormat::Rg32Sint => "rg32sint",
            StorageTextureFormat::Rgba32Float => "rgba32float",
            StorageTextureFormat::Rgba32Uint => "rgba32uint",
            StorageTextureFormat::Rgba32Sint => "rgba32sint",
            StorageTextureFormat::R32Float => "r32float",
            StorageTextureFormat::R32Uint => "r32uint",
            StorageTextureFormat::R32Sint => "r32sint",
        }
    }

    pub fn wgpu_format(self) -> wgpu::TextureFormat {
        match self {
            StorageTextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            StorageTextureFormat::Rgba8Snorm => wgpu::TextureFormat::Rgba8Snorm,
            StorageTextureFormat::Rgba8Uint => wgpu::TextureFormat::Rgba8Uint,
            StorageTextureFormat::Rgba8Sint => wgpu::TextureFormat::Rgba8Sint,
            StorageTextureFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
            StorageTextureFormat::Rgba16Uint => wgpu::TextureFormat::Rgba16Uint,
            StorageTextureFormat::Rgba16Sint => wgpu::TextureFormat::Rgba16Sint,
            StorageTextureFormat::Rg32Float => wgpu::TextureFormat::Rg32Float,
            StorageTextureFormat::Rg32Uint => wgpu::TextureFormat::Rg32Uint,
            StorageTextureFormat::Rg32Sint => wgpu::TextureFormat::Rg32Sint,
            StorageTextureFormat::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
            StorageTextureFormat::Rgba32Uint => wgpu::TextureFormat::Rgba32Uint,
            StorageTextureFormat::Rgba32Sint => wgpu::TextureFormat::Rgba32Sint,
            StorageTextureFormat::R32Float => wgpu::TextureFormat::R32Float,
            StorageTextureFormat::R32Uint => wgpu::TextureFormat::R32Uint,
            StorageTextureFormat::R32Sint => wgpu::TextureFormat::R32Sint,
        }
    }

    fn store_value_type(self) -> StorageTextureValueType {
        match self {
            StorageTextureFormat::Rgba8Unorm
            | StorageTextureFormat::Rgba8Snorm
            | StorageTextureFormat::Rgba16Float
            | StorageTextureFormat::Rg32Float
            | StorageTextureFormat::Rgba32Float
            | StorageTextureFormat::R32Float => StorageTextureValueType::F32,
            StorageTextureFormat::Rgba8Uint
            | StorageTextureFormat::Rgba16Uint
            | StorageTextureFormat::Rg32Uint
            | StorageTextureFormat::Rgba32Uint
            | StorageTextureFormat::R32Uint => StorageTextureValueType::U32,
            StorageTextureFormat::Rgba8Sint
            | StorageTextureFormat::Rgba16Sint
            | StorageTextureFormat::Rg32Sint
            | StorageTextureFormat::Rgba32Sint
            | StorageTextureFormat::R32Sint => StorageTextureValueType::I32,
        }
    }
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
    InvalidControlFlow {
        inst_index: usize,
        opcode: &'static str,
        msg: &'static str,
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
    MissingUavTypedDeclaration {
        slot: u32,
    },
    UnsupportedUavTextureFormat {
        slot: u32,
        format: u32,
    },
    UavSlotUsedAsBufferAndTexture {
        slot: u32,
    },
    TextureSlotUsedAsBufferAndTexture {
        slot: u32,
    },
    MissingStructuredBufferStride {
        kind: &'static str,
        slot: u32,
    },
    StructuredBufferStrideNotMultipleOf4 {
        kind: &'static str,
        slot: u32,
        stride_bytes: u32,
    },
    PixelShaderMissingColorOutputs,
    UavMixedAtomicAndNonAtomicAccess {
        slot: u32,
    },
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
            ShaderTranslateError::InvalidControlFlow {
                inst_index,
                opcode,
                msg,
            } => write!(
                f,
                "invalid structured control flow ({opcode}) at instruction index {inst_index}: {msg}"
            ),
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
                        "D3D11 cbuffer slot {slot} is out of range (max {max}); D3D11 exposes 14 constant buffer slots per stage (b0..b13). b# slots map to @binding({BINDING_BASE_CBUFFER} + slot) and must stay below the texture base @binding({BINDING_BASE_TEXTURE})"
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
            ShaderTranslateError::MissingUavTypedDeclaration { slot } => write!(
                f,
                "shader uses typed UAV u{slot} but is missing a corresponding dcl_uav_typed declaration"
            ),
            ShaderTranslateError::UnsupportedUavTextureFormat { slot, format } => write!(
                f,
                "typed UAV u{slot} uses unsupported DXGI format {format}; supported formats: rgba8unorm (28), rgba8snorm (31), rgba8uint (30), rgba8sint (32), rgba16float (10), rgba16uint (12), rgba16sint (14), rg32float (16), rg32uint (17), rg32sint (18), rgba32float (2), rgba32uint (3), rgba32sint (4), r32float (41), r32uint (42), r32sint (43)"
            ),
            ShaderTranslateError::UavSlotUsedAsBufferAndTexture { slot } => write!(
                f,
                "uav slot {slot} is used as both a UAV buffer and a typed UAV texture; u# slots must be used consistently"
            ),
            ShaderTranslateError::TextureSlotUsedAsBufferAndTexture { slot } => write!(
                f,
                "t# slot {slot} is used as both a texture SRV and an SRV buffer; t# slots must be used consistently"
            ),
            ShaderTranslateError::MissingStructuredBufferStride { kind, slot } => write!(
                f,
                "{kind} structured buffer slot {slot} is missing a byte stride declaration"
            ),
            ShaderTranslateError::StructuredBufferStrideNotMultipleOf4 {
                kind,
                slot,
                stride_bytes,
            } => write!(
                f,
                "{kind} structured buffer slot {slot} has unsupported stride {stride_bytes} (expected multiple of 4)"
            ),
            ShaderTranslateError::PixelShaderMissingColorOutputs => {
                write!(
                    f,
                    "pixel shader output signature declares no render-target outputs (SV_Target0..7 or legacy COLOR0..7)"
                )
            }
            ShaderTranslateError::UavMixedAtomicAndNonAtomicAccess { slot } => write!(
                f,
                "uav slot {slot} is used with both atomic and non-atomic operations; this translator currently requires UAV buffers to be either fully atomic (declared as array<atomic<u32>>) or fully non-atomic"
            ),
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
        (ShaderStage::Hull, rdef) => {
            let isgn = signatures
                .isgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("ISGN"))?;
            let osgn = signatures
                .osgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("OSGN"))?;
            // Hull shaders have two output signatures:
            // - Control point outputs (`OSGN` / `OSG1`)
            // - Patch constant outputs (`PCSG` / `PCG1`, sometimes emitted as `PSGN` / `PSG1`)
            //
            // Prefer `PCSG` but fall back to `PSGN` for toolchains that still use it.
            let pcsg = signatures
                .pcsg
                .as_ref()
                .or(signatures.psgn.as_ref())
                .ok_or(ShaderTranslateError::MissingSignature("PCSG/PSGN"))?;
            translate_hs(module, isgn, osgn, pcsg, rdef)
        }
        (ShaderStage::Domain, rdef) => {
            let isgn = signatures
                .isgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("ISGN"))?;
            let osgn = signatures
                .osgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("OSGN"))?;
            let psgn = signatures
                .psgn
                .as_ref()
                .ok_or(ShaderTranslateError::MissingSignature("PSGN"))?;
            translate_ds(module, isgn, psgn, osgn, rdef)
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
    let mut used_default_thread_group_size = false;
    let (x, y, z) = match thread_group_size {
        Some(size) => size,
        None => {
            // `dcl_thread_group` is required for valid DXBC compute shaders, but keep translation
            // resilient for fuzzed inputs by falling back to the smallest possible workgroup.
            used_default_thread_group_size = true;
            (1, 1, 1)
        }
    };
    if x == 0 || y == 0 || z == 0 {
        return Err(ShaderTranslateError::InvalidThreadGroupSize { x, y, z });
    }

    let used_regs = scan_used_input_registers(module);
    let used_sivs = scan_used_compute_sivs(module, &io);
    let mut reflected_inputs = Vec::<IoParam>::new();
    let mut reflected_sivs = BTreeSet::<ComputeSysValue>::new();
    for reg in &used_regs {
        let Some(siv) = io.cs_inputs.get(reg) else {
            continue;
        };
        reflected_sivs.insert(*siv);

        let mask = module
            .decls
            .iter()
            .find_map(|decl| match decl {
                Sm4Decl::InputSiv {
                    reg: decl_reg,
                    mask,
                    sys_value,
                } if decl_reg == reg
                    && compute_sys_value_from_d3d_name(*sys_value) == Some(*siv) =>
                {
                    Some(mask.0)
                }
                _ => None,
            })
            .unwrap_or(siv.default_mask());

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

    // Reflect compute builtins referenced via dedicated SM5 operand types.
    //
    // These do not come from the `v#` input register file and therefore won't show up in
    // `used_regs`/`io.cs_inputs`. Still, they are part of the WGSL entry point interface, so expose
    // them via reflection.
    for siv in &used_sivs {
        if reflected_sivs.contains(siv) {
            continue;
        }
        reflected_inputs.push(IoParam {
            semantic_name: siv.d3d_semantic_name().to_owned(),
            semantic_index: 0,
            register: siv.synthetic_register(),
            location: None,
            builtin: Some(siv.builtin()),
            mask: siv.default_mask(),
            stream: 0,
        });
    }

    let reflection = ShaderReflection {
        inputs: reflected_inputs,
        outputs: Vec::new(),
        bindings: resources.bindings(ShaderStage::Compute),
        rdef,
    };

    let mut w = WgslWriter::new();
    // Bindings follow the shared AeroGPU D3D11 binding model (see `binding_model.rs`):
    // compute-stage resources live in the stage-scoped bind group `@group(2)`.
    //
    // WGSL emission and reflection must agree on the group index; runtimes that consume reflection
    // use it to build pipeline layouts and bind groups. The `protocol_d3d11` runtime supports this
    // by inserting empty groups 0/1 and placing the real compute layout at group 2.
    resources.emit_decls(&mut w, ShaderStage::Compute)?;

    if used_default_thread_group_size {
        w.line(
            "// NOTE: DXBC is missing dcl_thread_group; defaulting to @workgroup_size(1, 1, 1).",
        );
        w.line("");
    }

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

fn translate_hs(
    module: &Sm4Module,
    isgn: &DxbcSignature,
    osgn: &DxbcSignature,
    pcsg: &DxbcSignature,
    rdef: Option<RdefChunk>,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    // HS is executed via compute emulation. We currently support a minimal subset:
    // - domain("tri")
    // - partitioning("integer")
    // - outputtopology("triangle_cw") (ccw is also accepted)
    // - outputcontrolpoints <= 32

    // Validate declared tessellation metadata when present in the IR. Older decoders may not
    // populate these declarations yet, so absence is treated as "unknown" rather than an error.
    //
    // Note: Real D3D11 hull shaders can have different input/output control point counts. The
    // current compute-emulation path assumes they match (each invocation corresponds to the same
    // control point index in the input patch and output patch), so reject mismatches when both
    // declarations are present.
    let mut input_control_points: Option<u32> = None;
    let mut output_control_points: Option<u32> = None;
    for decl in &module.decls {
        match decl {
            Sm4Decl::InputControlPointCount { count } => {
                if *count == 0 || *count > 32 {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index: 0,
                        opcode: format!("hs_input_control_points_{count}"),
                    });
                }
                if let Some(prev) = input_control_points {
                    if prev != *count {
                        return Err(ShaderTranslateError::UnsupportedInstruction {
                            inst_index: 0,
                            opcode: format!("hs_input_control_points_{prev}_vs_{count}"),
                        });
                    }
                } else {
                    input_control_points = Some(*count);
                }
            }
            Sm4Decl::HsDomain { domain } => {
                if *domain != crate::sm4_ir::HsDomain::Tri {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index: 0,
                        opcode: format!("hs_domain_{domain:?}"),
                    });
                }
            }
            Sm4Decl::HsPartitioning { partitioning } => {
                if *partitioning != crate::sm4_ir::HsPartitioning::Integer {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index: 0,
                        opcode: format!("hs_partitioning_{partitioning:?}"),
                    });
                }
            }
            Sm4Decl::HsOutputTopology { topology } => {
                if !matches!(
                    topology,
                    crate::sm4_ir::HsOutputTopology::TriangleCw
                        | crate::sm4_ir::HsOutputTopology::TriangleCcw
                ) {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index: 0,
                        opcode: format!("hs_output_topology_{topology:?}"),
                    });
                }
            }
            Sm4Decl::HsOutputControlPointCount { count } => {
                if *count == 0 || *count > 32 {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index: 0,
                        opcode: format!("hs_output_control_points_{count}"),
                    });
                }
                if let Some(prev) = output_control_points {
                    if prev != *count {
                        return Err(ShaderTranslateError::UnsupportedInstruction {
                            inst_index: 0,
                            opcode: format!("hs_output_control_points_{prev}_vs_{count}"),
                        });
                    }
                } else {
                    output_control_points = Some(*count);
                }
            }
            _ => {}
        }
    }
    if let (Some(input), Some(output)) = (input_control_points, output_control_points) {
        if input != output {
            return Err(ShaderTranslateError::UnsupportedInstruction {
                inst_index: 0,
                opcode: format!("hs_input_control_points_{input}_output_control_points_{output}"),
            });
        }
    }

    let resources = scan_resources(module, rdef.as_ref())?;

    // Build IO maps for:
    // - Control point phase: ISGN -> OSGN
    // - Patch constant phase: ISGN -> PCSG/PSGN
    let mut io_cp = build_io_maps(module, isgn, osgn)?;
    let mut io_pc = build_io_maps(module, isgn, pcsg)?;

    // Hull patch-constant code can index either:
    // - input control points (from ISGN), or
    // - output control points (from OSGN) when using `OutputPatch` in HLSL.
    //
    // Both are encoded as `SrcKind::GsInput` in our IR, so keep register sets around for
    // disambiguation during WGSL emission.
    let hs_input_regs: BTreeSet<u32> = isgn.parameters.iter().map(|p| p.register).collect();
    let hs_cp_output_regs: BTreeSet<u32> = osgn.parameters.iter().map(|p| p.register).collect();
    io_cp.hs_input_regs = hs_input_regs.clone();
    io_cp.hs_cp_output_regs = hs_cp_output_regs.clone();
    io_pc.hs_input_regs = hs_input_regs;
    io_pc.hs_cp_output_regs = hs_cp_output_regs;

    // Map HS system values (`SV_PrimitiveID`, `SV_OutputControlPointID`) onto synthetic variables
    // derived from the compute invocation IDs.
    let mut hs_inputs = BTreeMap::<u32, HullSysValue>::new();
    for decl in &module.decls {
        if let Sm4Decl::InputSiv { reg, sys_value, .. } = decl {
            if let Some(siv) = hull_sys_value_from_d3d_name(*sys_value) {
                hs_inputs.insert(*reg, siv);
            }
        }
    }
    // Fall back to signature-driven system value detection when `dcl_input_siv` is missing.
    for (reg, p) in &io_cp.inputs {
        if let Some(sys_value) = p.sys_value {
            if let Some(siv) = hull_sys_value_from_d3d_name(sys_value) {
                hs_inputs.insert(*reg, siv);
            }
        }
    }
    io_cp.hs_inputs = hs_inputs.clone();
    io_pc.hs_inputs = hs_inputs;

    // Split the linear instruction stream into two phases using the first top-level `ret` as a
    // boundary. FXC emits separate `ret`s per phase in common SM5 hull shaders.
    let mut depth = 0u32;
    let mut split_at: Option<usize> = None;
    for (i, inst) in module.instructions.iter().enumerate() {
        match inst {
            Sm4Inst::If { .. } | Sm4Inst::IfC { .. } => depth += 1,
            Sm4Inst::EndIf => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && matches!(inst, Sm4Inst::Ret) {
            split_at = Some(i);
            break;
        }
    }

    let (cp_insts, pc_insts) = if let Some(i) = split_at {
        (
            module.instructions[..=i].to_vec(),
            module.instructions[i + 1..].to_vec(),
        )
    } else {
        (module.instructions.clone(), Vec::new())
    };

    let mut module_cp = module.clone();
    module_cp.instructions = cp_insts;
    let mut module_pc = module.clone();
    module_pc.instructions = pc_insts;

    // Determine the number of non-system input registers we need to source from the patch buffer.
    let used_inputs = scan_used_input_registers(module);
    let max_non_siv_input = used_inputs
        .iter()
        // Ignore indexed `OutputPatch` reads (encoded as `SrcKind::GsInput`) when they refer to
        // HS control-point outputs rather than true HS inputs.
        .filter(|r| io_cp.hs_input_regs.contains(r))
        .filter(|r| !io_cp.hs_inputs.contains_key(r))
        .max()
        .copied();
    let hs_in_stride = max_non_siv_input.map(|m| m.saturating_add(1)).unwrap_or(1);

    let max_cp_out = io_cp.outputs.keys().max().copied();
    let hs_cp_out_stride = max_cp_out.map(|m| m.saturating_add(1)).unwrap_or(1);

    // Patch-constant output buffers exclude tess factors; those are written to a separate compact
    // scalar buffer consumed by the tessellator.
    let hs_pc_layout = build_hs_patch_constant_layout(pcsg);
    io_pc.hs_is_patch_constant_phase = true;
    io_pc.hs_pc_patch_constant_reg_masks = hs_pc_layout.patch_constant_reg_masks.clone();
    io_pc.hs_pc_tess_factor_writes = hs_pc_layout.tess_factor_writes.clone();
    io_pc.hs_pc_tess_factor_stride = hs_pc_layout.tess_factor_stride;
    let hs_pc_out_stride = hs_pc_layout.patch_constant_reg_count.max(1);

    let mut outputs_reflection = io_cp.outputs_reflection_vertex();
    outputs_reflection.extend(io_pc.outputs_reflection_vertex());

    let reflection = ShaderReflection {
        inputs: io_cp.inputs_reflection(),
        outputs: outputs_reflection,
        bindings: resources.bindings(ShaderStage::Hull),
        rdef,
    };

    let mut w = WgslWriter::new();

    // HS stage interface buffers (inputs + outputs) for compute emulation.
    //
    // Layout:
    // - Control point inputs/outputs are indexed as:
    //     ((primitive_id * HS_CONTROL_POINTS_PER_PATCH + control_point_id) * STRIDE + reg_index)
    //   where `HS_CONTROL_POINTS_PER_PATCH` is the expected control-point count per patch (<= 32).
    // - Patch constant outputs are indexed as:
    //     (primitive_id * STRIDE + reg_index)
    w.line("struct HsRegBuffer { data: array<vec4<f32>> };");
    w.line("struct HsF32Buffer { data: array<f32> };");
    w.line("@group(0) @binding(0) var<storage, read> hs_in: HsRegBuffer;");
    w.line("@group(0) @binding(1) var<storage, read_write> hs_out_cp: HsRegBuffer;");
    w.line("@group(0) @binding(2) var<storage, read_write> hs_patch_constants_buf: HsRegBuffer;");
    w.line("@group(0) @binding(3) var<storage, read_write> hs_tess_factors: HsF32Buffer;");
    w.line("");
    // Bounds-checked accessors for runtime-sized HS scratch buffers.
    //
    // Note: Unlike fixed-size `array<T, N>`, runtime-sized arrays allow `arrayLength()` queries.
    // These helpers keep the generated WGSL well-defined even if the host provides conservative
    // allocations or runs with robust buffer access disabled.
    w.line("fn hs_load_in(idx: u32) -> vec4<f32> {");
    w.indent();
    w.line("let len = arrayLength(&hs_in.data);");
    w.line("if (idx >= len) { return vec4<f32>(0.0); }");
    w.line("return hs_in.data[idx];");
    w.dedent();
    w.line("}");
    w.line("");
    w.line("fn hs_load_out_cp(idx: u32) -> vec4<f32> {");
    w.indent();
    w.line("let len = arrayLength(&hs_out_cp.data);");
    w.line("if (idx >= len) { return vec4<f32>(0.0); }");
    w.line("return hs_out_cp.data[idx];");
    w.dedent();
    w.line("}");
    w.line("");
    w.line("fn hs_store_out_cp(idx: u32, value: vec4<f32>) {");
    w.indent();
    w.line("let len = arrayLength(&hs_out_cp.data);");
    w.line("if (idx >= len) { return; }");
    w.line("hs_out_cp.data[idx] = value;");
    w.dedent();
    w.line("}");
    w.line("");
    w.line("fn hs_store_patch_constants(idx: u32, value: vec4<f32>) {");
    w.indent();
    w.line("let len = arrayLength(&hs_patch_constants_buf.data);");
    w.line("if (idx >= len) { return; }");
    w.line("hs_patch_constants_buf.data[idx] = value;");
    w.dedent();
    w.line("}");
    w.line("");
    w.line("fn hs_store_tess_factor(idx: u32, value: f32) {");
    w.indent();
    w.line("let len = arrayLength(&hs_tess_factors.data);");
    w.line("if (idx >= len) { return; }");
    w.line("hs_tess_factors.data[idx] = value;");
    w.dedent();
    w.line("}");
    w.line("");

    resources.emit_decls(&mut w, ShaderStage::Hull)?;

    w.line(&format!("const HS_IN_STRIDE: u32 = {hs_in_stride}u;"));
    w.line(&format!(
        "const HS_CP_OUT_STRIDE: u32 = {hs_cp_out_stride}u;"
    ));
    w.line(&format!(
        "const HS_PC_OUT_STRIDE: u32 = {hs_pc_out_stride}u;"
    ));
    w.line(&format!(
        "const HS_TESS_FACTOR_STRIDE: u32 = {}u;",
        hs_pc_layout.tess_factor_stride
    ));
    w.line("const HS_MAX_CONTROL_POINTS: u32 = 32u;");
    if let Some(count) = input_control_points {
        w.line(&format!("const HS_INPUT_CONTROL_POINTS: u32 = {count}u;"));
    }
    if let Some(count) = output_control_points {
        w.line(&format!("const HS_OUTPUT_CONTROL_POINTS: u32 = {count}u;"));
    }
    if output_control_points.is_some() {
        w.line("const HS_CONTROL_POINTS_PER_PATCH: u32 = HS_OUTPUT_CONTROL_POINTS;");
    } else if input_control_points.is_some() {
        w.line("const HS_CONTROL_POINTS_PER_PATCH: u32 = HS_INPUT_CONTROL_POINTS;");
    } else {
        w.line("const HS_CONTROL_POINTS_PER_PATCH: u32 = HS_MAX_CONTROL_POINTS;");
    }
    w.line("");

    // Control-point phase: one invocation per output control point.
    w.line("@compute @workgroup_size(1)");
    w.line("fn hs_main(@builtin(global_invocation_id) id: vec3<u32>) {");
    w.indent();
    w.line("let hs_output_control_point_id: u32 = id.x;");
    w.line("let hs_primitive_id: u32 = id.y;");
    w.line("if (hs_output_control_point_id >= HS_CONTROL_POINTS_PER_PATCH) { return; }");
    w.line(
        "let hs_in_base: u32 = (hs_primitive_id * HS_CONTROL_POINTS_PER_PATCH + hs_output_control_point_id) * HS_IN_STRIDE;",
    );
    w.line(
        "let hs_out_base: u32 = (hs_primitive_id * HS_CONTROL_POINTS_PER_PATCH + hs_output_control_point_id) * HS_CP_OUT_STRIDE;",
    );
    w.line("");

    emit_temp_and_output_decls(&mut w, &module_cp, &io_cp)?;
    let ctx = EmitCtx {
        stage: ShaderStage::Hull,
        io: &io_cp,
        resources: &resources,
    };
    emit_instructions(&mut w, &module_cp, &ctx)?;

    w.line("");
    io_cp.emit_hs_commit_outputs(&mut w);
    w.dedent();
    w.line("}");
    w.line("");

    // Patch-constant phase: one invocation per patch.
    w.line("@compute @workgroup_size(1)");
    w.line("fn hs_patch_constants(@builtin(global_invocation_id) id: vec3<u32>) {");
    w.indent();
    w.line("let hs_primitive_id: u32 = id.x;");
    w.line("let hs_output_control_point_id: u32 = 0u;");
    w.line(
        "let hs_in_base: u32 = (hs_primitive_id * HS_CONTROL_POINTS_PER_PATCH + hs_output_control_point_id) * HS_IN_STRIDE;",
    );
    w.line("let hs_out_base: u32 = hs_primitive_id * HS_PC_OUT_STRIDE;");
    w.line("");

    emit_temp_and_output_decls(&mut w, &module_pc, &io_pc)?;
    let ctx = EmitCtx {
        stage: ShaderStage::Hull,
        io: &io_pc,
        resources: &resources,
    };
    emit_instructions(&mut w, &module_pc, &ctx)?;

    w.line("");
    io_pc.emit_hs_commit_outputs(&mut w);
    w.dedent();
    w.line("}");

    Ok(ShaderTranslation {
        wgsl: w.finish(),
        stage: ShaderStage::Hull,
        reflection,
    })
}

#[derive(Debug, Clone)]
struct HsTessFactorWrite {
    dst_index: u32,
    src_reg: u32,
    src_component: u8,
}

#[derive(Debug, Clone)]
struct HsPatchConstantLayout {
    /// Combined component masks for non-tess patch constants keyed by output register.
    patch_constant_reg_masks: BTreeMap<u32, u8>,
    /// Register-file stride for patch constants (max used register + 1).
    patch_constant_reg_count: u32,
    /// Scalar writes for the compact tess-factor buffer.
    tess_factor_writes: Vec<HsTessFactorWrite>,
    /// Stride (in scalars) of the compact tess-factor buffer per patch.
    tess_factor_stride: u32,
}

fn build_hs_patch_constant_layout(pcsg: &DxbcSignature) -> HsPatchConstantLayout {
    // Patch-constant signatures can pack both tess factors and user patch constants into the same
    // output register file. We split them into two buffers:
    // - `hs_patch_constants`: vec4 register file for user patch constants (by output register index)
    // - `hs_tess_factors`: compact scalar buffer (outer factors first, then inside factors)
    let mut patch_constant_reg_masks = BTreeMap::<u32, u8>::new();
    let mut outer_writes = Vec::<HsTessFactorWrite>::new();
    let mut inner_writes = Vec::<HsTessFactorWrite>::new();
    let mut outer_count = 0u32;
    let mut inner_count = 0u32;

    for p in &pcsg.parameters {
        if is_sv_tess_factor(&p.semantic_name) {
            let mut lane = 0u32;
            for (component, bit) in [(0u8, 1u8), (1, 2), (2, 4), (3, 8)] {
                if (p.mask & bit) == 0 {
                    continue;
                }
                let dst_index = p.semantic_index + lane;
                outer_count = outer_count.max(dst_index.saturating_add(1));
                outer_writes.push(HsTessFactorWrite {
                    dst_index,
                    src_reg: p.register,
                    src_component: component,
                });
                lane += 1;
            }
            continue;
        }
        if is_sv_inside_tess_factor(&p.semantic_name) {
            let mut lane = 0u32;
            for (component, bit) in [(0u8, 1u8), (1, 2), (2, 4), (3, 8)] {
                if (p.mask & bit) == 0 {
                    continue;
                }
                let dst_index = p.semantic_index + lane;
                inner_count = inner_count.max(dst_index.saturating_add(1));
                inner_writes.push(HsTessFactorWrite {
                    dst_index,
                    src_reg: p.register,
                    src_component: component,
                });
                lane += 1;
            }
            continue;
        }

        patch_constant_reg_masks
            .entry(p.register)
            .and_modify(|m| *m |= p.mask)
            .or_insert(p.mask);
    }

    let patch_constant_reg_count = patch_constant_reg_masks
        .keys()
        .max()
        .map(|r| r.saturating_add(1))
        .unwrap_or(0);

    // Apply the outer-factor offset to inside tess factors and combine.
    let tess_factor_stride = outer_count + inner_count;
    let mut tess_factor_writes = Vec::<HsTessFactorWrite>::new();
    tess_factor_writes.extend(outer_writes);
    tess_factor_writes.extend(inner_writes.into_iter().map(|mut w| {
        w.dst_index += outer_count;
        w
    }));

    HsPatchConstantLayout {
        patch_constant_reg_masks,
        patch_constant_reg_count,
        tess_factor_writes,
        tess_factor_stride,
    }
}

fn translate_ds(
    module: &Sm4Module,
    isgn: &DxbcSignature,
    psgn: &DxbcSignature,
    osgn: &DxbcSignature,
    rdef: Option<RdefChunk>,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    // Domain shaders are executed via compute emulation. We currently support a minimal subset:
    // - domain("tri")
    // - partitioning("integer")
    //
    // The fixed-function tessellator is emulated by mapping `@builtin(global_invocation_id)` to:
    // - `id.y` = patch (primitive) id
    // - `id.x` = vertex index within that patch
    //
    // Domain location (`SV_DomainLocation`) is derived from the patch tess factor and `id.x`
    // assuming a uniform triangular grid.

    // Validate declared tessellation metadata when present in the IR. Older decoders may not
    // populate these declarations yet, so absence is treated as "unknown" rather than an error.
    for decl in &module.decls {
        match decl {
            Sm4Decl::HsDomain { domain } => {
                if *domain != crate::sm4_ir::HsDomain::Tri {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index: 0,
                        opcode: format!("ds_domain_{domain:?}"),
                    });
                }
            }
            Sm4Decl::HsPartitioning { partitioning } => {
                if *partitioning != crate::sm4_ir::HsPartitioning::Integer {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index: 0,
                        opcode: format!("ds_partitioning_{partitioning:?}"),
                    });
                }
            }
            _ => {}
        }
    }

    let io = build_ds_io_maps(module, isgn, psgn, osgn)?;
    let resources = scan_resources(module, rdef.as_ref())?;

    let reflection = ShaderReflection {
        inputs: io.inputs_reflection(),
        outputs: io.outputs_reflection_vertex(),
        bindings: resources.bindings(ShaderStage::Domain),
        rdef,
    };

    // These strides must match the buffers produced by HS emulation.
    let cp_in_stride = isgn
        .parameters
        .iter()
        .filter(|p| {
            !is_sv_domain_location(&p.semantic_name) && !is_sv_primitive_id(&p.semantic_name)
        })
        .map(|p| p.register)
        .max()
        .map(|m| m.saturating_add(1))
        .unwrap_or(1)
        .max(1);
    let pc_in_stride = psgn
        .parameters
        .iter()
        .map(|p| p.register)
        .max()
        .map(|m| m.saturating_add(1))
        .unwrap_or(1)
        .max(1);

    let pos_reg = io
        .vs_position_register
        .ok_or(ShaderTranslateError::MissingSignature(
            "domain output SV_Position",
        ))?;

    let mut w = WgslWriter::new();

    // DS stage interface buffers for compute emulation.
    w.line("struct DsRegBuffer { data: array<vec4<f32>> };");
    w.line("struct DsF32Buffer { data: array<f32> };");
    w.line("@group(0) @binding(0) var<storage, read> ds_in_cp: DsRegBuffer;");
    w.line("@group(0) @binding(1) var<storage, read> ds_in_pc: DsRegBuffer;");
    w.line("@group(0) @binding(3) var<storage, read> ds_tess_factors: DsF32Buffer;");
    w.line("");

    // Output struct mirrors `VsOut`, but without stage I/O attributes (written to a storage buffer).
    w.line("struct DsOut {");
    w.indent();
    w.line("pos: vec4<f32>,");
    for p in io.outputs.values() {
        if p.param.register == pos_reg {
            continue;
        }
        w.line(&format!("{}: vec4<f32>,", p.field_name('o')));
    }
    w.dedent();
    w.line("};");
    w.line("");

    w.line("struct DsOutBuffer { data: array<DsOut> };");
    w.line("@group(0) @binding(2) var<storage, read_write> ds_out: DsOutBuffer;");
    w.line("");

    resources.emit_decls(&mut w, ShaderStage::Domain)?;

    w.line(&format!("const DS_CP_IN_STRIDE: u32 = {cp_in_stride}u;"));
    w.line(&format!("const DS_PC_IN_STRIDE: u32 = {pc_in_stride}u;"));
    // Compact tess factors buffer: outer[3] + inner[1] for tri-domain.
    w.line("const DS_TESS_FACTOR_STRIDE: u32 = 4u;");
    w.line("const DS_MAX_CONTROL_POINTS: u32 = 32u;");
    w.line("");

    // DS body function (returns a struct so `ret` can map to `return out;`).
    w.line("fn ds_invoke(patch_id: u32, domain_location: vec3<f32>, primitive_id: u32) -> DsOut {");
    w.indent();
    w.line("let ds_patch_id: u32 = patch_id;");
    w.line("let ds_domain_location: vec3<f32> = domain_location;");
    w.line("let ds_primitive_id: u32 = primitive_id;");
    w.line("let ds_pc_base: u32 = ds_patch_id * DS_PC_IN_STRIDE;");
    w.line("");
    w.line("var out: DsOut;");
    w.line("");

    emit_temp_and_output_decls(&mut w, module, &io)?;
    let ctx = EmitCtx {
        stage: ShaderStage::Domain,
        io: &io,
        resources: &resources,
    };
    emit_instructions(&mut w, module, &ctx)?;

    w.line("");
    // `DsOut` uses the same field names as `VsOut` (pos + `o#`), so reuse the VS return emitter.
    io.emit_vs_return(&mut w)?;
    w.dedent();
    w.line("}");
    w.line("");

    // ---- DS compute entry point (tessellator emulation) ----
    //
    // Workgroup size is an arbitrary default; the runtime is expected to dispatch enough
    // invocations for the patch grid.
    w.line("@compute @workgroup_size(64)");
    w.line("fn ds_main(@builtin(global_invocation_id) id: vec3<u32>) {");
    w.indent();
    w.line("let patch_id: u32 = id.y;");
    w.line("let vert_in_patch: u32 = id.x;");
    w.line("let pc_base: u32 = patch_id * DS_PC_IN_STRIDE;");
    w.line("let tf_base: u32 = patch_id * DS_TESS_FACTOR_STRIDE;");
    w.line("");

    // For now, derive a single tess level from outer tess factor 0 (integer partition).
    // Clamp to at least 1 to avoid division-by-zero.
    w.line("let tess_f: f32 = ds_tess_factors.data[tf_base + 0u];");
    w.line("let tess: u32 = max(1u, u32(round(tess_f)));");
    w.line("let verts_per_patch: u32 = (tess + 1u) * (tess + 2u) / 2u;");
    w.line("if (vert_in_patch >= verts_per_patch) { return; }");
    w.line("");

    // Map `vert_in_patch` to barycentric coords (u,v,w) for a triangular grid.
    w.line("var idx: u32 = vert_in_patch;");
    w.line("var row: u32 = 0u;");
    w.line("var row_len: u32 = tess + 1u;");
    w.line("loop {");
    w.indent();
    w.line("if (idx < row_len) { break; }");
    w.line("idx = idx - row_len;");
    w.line("row = row + 1u;");
    w.line("row_len = row_len - 1u;");
    w.dedent();
    w.line("}");
    w.line("let col: u32 = idx;");
    w.line("let n: f32 = f32(tess);");
    w.line("let u: f32 = f32(row) / n;");
    w.line("let v: f32 = f32(col) / n;");
    w.line("let w_bary: f32 = 1.0 - u - v;");
    w.line("let domain_loc: vec3<f32> = vec3<f32>(u, v, w_bary);");
    w.line("");

    w.line("let prim_id: u32 = patch_id;");
    w.line("let out_index: u32 = patch_id * verts_per_patch + vert_in_patch;");
    w.line("let out_vertex: DsOut = ds_invoke(patch_id, domain_loc, prim_id);");
    w.line("ds_out.data[out_index] = out_vertex;");
    w.dedent();
    w.line("}");

    Ok(ShaderTranslation {
        wgsl: w.finish(),
        stage: ShaderStage::Domain,
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
    let ps_has_depth_output = io.ps_sv_depth.is_some();
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
        w.line(&format!(
            "@location({location}) target{location}: vec4<f32>,"
        ));
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
    let mut ps_sv_depth: Option<ParamInfo> = None;
    for p in &osgn.parameters {
        let mut sys_value = resolve_sys_value_type(p, &output_sivs);
        if module.stage == ShaderStage::Pixel
            && sys_value.is_none()
            && p.semantic_index == 0
            && p.semantic_name.eq_ignore_ascii_case("DEPTH")
        {
            // Some toolchains emit the legacy `DEPTH` semantic with `system_value_type` unset.
            sys_value = Some(D3D_NAME_DEPTH);
        }
        let info = ParamInfo::from_sig_param("output", p, sys_value)?;

        if module.stage == ShaderStage::Pixel
            && matches!(
                info.sys_value,
                Some(D3D_NAME_DEPTH)
                    | Some(D3D_NAME_DEPTH_GREATER_EQUAL)
                    | Some(D3D_NAME_DEPTH_LESS_EQUAL)
            )
        {
            // SV_Depth uses a dedicated register file (`oDepth`) and can share a register number
            // with color outputs in the signature table. Keep it out of the regular `o#` output map
            // (which is keyed only by register number) and track it separately.
            match ps_sv_depth.as_mut() {
                Some(existing) => {
                    if existing.sys_value != info.sys_value {
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
                    *existing =
                        ParamInfo::from_sig_param("output", &merged_param, existing.sys_value)?;
                }
                None => {
                    ps_sv_depth = Some(info);
                }
            }
            continue;
        }
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

    // `ps_sv_depth` is extracted while building the output map (see above).

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
        ps_sv_depth,
        vs_vertex_id_register: vs_vertex_id_reg,
        vs_instance_id_register: vs_instance_id_reg,
        ps_front_facing_register: ps_front_facing_reg,
        cs_inputs: BTreeMap::new(),
        hs_inputs: BTreeMap::new(),
        hs_input_regs: BTreeSet::new(),
        hs_cp_output_regs: BTreeSet::new(),
        hs_is_patch_constant_phase: false,
        hs_pc_patch_constant_reg_masks: BTreeMap::new(),
        hs_pc_tess_factor_writes: Vec::new(),
        hs_pc_tess_factor_stride: 0,
    })
}

fn build_ds_io_maps(
    module: &Sm4Module,
    isgn: &DxbcSignature,
    psgn: &DxbcSignature,
    osgn: &DxbcSignature,
) -> Result<IoMaps, ShaderTranslateError> {
    // Domain shader inputs are split between:
    // - ISGN: control point data + per-invocation system values (SV_DomainLocation, SV_PrimitiveID)
    // - PSGN: patch constant data produced by the hull shader
    //
    // Our IR model uses a single non-indexed `v#` register file for patch-constant inputs and
    // system values. Merge the patch constant signature with the system-value subset of ISGN.
    let mut combined_params = psgn.parameters.clone();
    for p in &isgn.parameters {
        if is_sv_domain_location(&p.semantic_name) || is_sv_primitive_id(&p.semantic_name) {
            combined_params.push(p.clone());
        }
    }
    let combined_isgn = DxbcSignature {
        parameters: combined_params,
    };
    build_io_maps(module, &combined_isgn, osgn)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ComputeSysValue {
    DispatchThreadId,
    GroupThreadId,
    GroupId,
    GroupIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HullSysValue {
    PrimitiveId,
    OutputControlPointId,
}

impl ComputeSysValue {
    fn from_compute_builtin(builtin: ComputeBuiltin) -> Self {
        match builtin {
            ComputeBuiltin::DispatchThreadId => ComputeSysValue::DispatchThreadId,
            ComputeBuiltin::GroupThreadId => ComputeSysValue::GroupThreadId,
            ComputeBuiltin::GroupId => ComputeSysValue::GroupId,
            ComputeBuiltin::GroupIndex => ComputeSysValue::GroupIndex,
        }
    }

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

    fn default_mask(self) -> u8 {
        match self {
            ComputeSysValue::GroupIndex => 0b0001,
            _ => 0b0111,
        }
    }

    fn synthetic_register(self) -> u32 {
        // Compute system values referenced via dedicated SM5 operand types do not have an explicit
        // `v#` register index like `dcl_input_siv` declarations do.
        //
        // For reflection, assign them stable synthetic register indices in a reserved range so
        // callers can still differentiate them.
        const BASE: u32 = 0xffff_ff00;
        match self {
            ComputeSysValue::DispatchThreadId => BASE,
            ComputeSysValue::GroupThreadId => BASE + 1,
            ComputeSysValue::GroupId => BASE + 2,
            ComputeSysValue::GroupIndex => BASE + 3,
        }
    }

    fn expand_to_vec4(self) -> String {
        match self {
            ComputeSysValue::DispatchThreadId
            | ComputeSysValue::GroupThreadId
            | ComputeSysValue::GroupId => {
                let field = format!("input.{}", self.wgsl_field_name());
                format!(
                    "vec4<f32>(bitcast<f32>({field}.x), bitcast<f32>({field}.y), bitcast<f32>({field}.z), bitcast<f32>(1u))"
                )
            }
            ComputeSysValue::GroupIndex => {
                let field = format!("input.{}", self.wgsl_field_name());
                format!("vec4<f32>(bitcast<f32>({field}), 0.0, 0.0, bitcast<f32>(1u))")
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

fn hull_sys_value_from_d3d_name(name: u32) -> Option<HullSysValue> {
    match name {
        D3D_NAME_PRIMITIVE_ID => Some(HullSysValue::PrimitiveId),
        D3D_NAME_OUTPUT_CONTROL_POINT_ID => Some(HullSysValue::OutputControlPointId),
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
        ps_sv_depth: None,
        vs_vertex_id_register: None,
        vs_instance_id_register: None,
        ps_front_facing_register: None,
        cs_inputs,
        hs_inputs: BTreeMap::new(),
        hs_input_regs: BTreeSet::new(),
        hs_cp_output_regs: BTreeSet::new(),
        hs_is_patch_constant_phase: false,
        hs_pc_patch_constant_reg_masks: BTreeMap::new(),
        hs_pc_tess_factor_writes: Vec::new(),
        hs_pc_tess_factor_stride: 0,
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

        let mut inst = inst;
        while let Sm4Inst::Predicated { inner, .. } = inst {
            inst = inner;
        }

        match inst {
            Sm4Inst::If { cond, .. } => scan_src_regs(cond, &mut scan_reg),
            Sm4Inst::IfC { a, b, .. }
            | Sm4Inst::BreakC { a, b, .. }
            | Sm4Inst::ContinueC { a, b, .. } => {
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::Discard { cond, .. } => scan_src_regs(cond, &mut scan_reg),
            Sm4Inst::Clip { src } => scan_src_regs(src, &mut scan_reg),
            Sm4Inst::Else
            | Sm4Inst::EndIf
            | Sm4Inst::Loop
            | Sm4Inst::EndLoop
            | Sm4Inst::Break
            | Sm4Inst::Continue => {}
            Sm4Inst::Mov { dst: _, src }
            | Sm4Inst::Itof { dst: _, src }
            | Sm4Inst::Utof { dst: _, src }
            | Sm4Inst::Ftoi { dst: _, src }
            | Sm4Inst::Ftou { dst: _, src }
            | Sm4Inst::F32ToF16 { dst: _, src }
            | Sm4Inst::F16ToF32 { dst: _, src } => scan_src_regs(src, &mut scan_reg),
            Sm4Inst::Movc { dst: _, cond, a, b } => {
                scan_src_regs(cond, &mut scan_reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::Add { dst: _, a, b }
            | Sm4Inst::Mul { dst: _, a, b }
            | Sm4Inst::IMul { a, b, .. }
            | Sm4Inst::UMul { a, b, .. }
            | Sm4Inst::Dp3 { dst: _, a, b }
            | Sm4Inst::Dp4 { dst: _, a, b }
            | Sm4Inst::Min { dst: _, a, b }
            | Sm4Inst::Max { dst: _, a, b }
            | Sm4Inst::IAdd { dst: _, a, b }
            | Sm4Inst::ISub { dst: _, a, b }
            | Sm4Inst::And { dst: _, a, b }
            | Sm4Inst::Or { dst: _, a, b }
            | Sm4Inst::Xor { dst: _, a, b }
            | Sm4Inst::IShl { dst: _, a, b }
            | Sm4Inst::IShr { dst: _, a, b }
            | Sm4Inst::UShr { dst: _, a, b }
            | Sm4Inst::IMin { dst: _, a, b }
            | Sm4Inst::IMax { dst: _, a, b }
            | Sm4Inst::UMin { dst: _, a, b }
            | Sm4Inst::UMax { dst: _, a, b }
            | Sm4Inst::Cmp { dst: _, a, b, .. }
            | Sm4Inst::IAddC { a, b, .. }
            | Sm4Inst::UAddC { a, b, .. }
            | Sm4Inst::ISubC { a, b, .. }
            | Sm4Inst::USubB { a, b, .. }
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
            Sm4Inst::Mad { dst: _, a, b, c }
            | Sm4Inst::IMad { a, b, c, .. }
            | Sm4Inst::UMad { a, b, c, .. } => {
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
                scan_src_regs(c, &mut scan_reg);
            }
            Sm4Inst::Rcp { dst: _, src }
            | Sm4Inst::Rsq { dst: _, src }
            | Sm4Inst::IAbs { dst: _, src }
            | Sm4Inst::INeg { dst: _, src }
            | Sm4Inst::Not { dst: _, src }
            | Sm4Inst::Bfrev { dst: _, src }
            | Sm4Inst::CountBits { dst: _, src }
            | Sm4Inst::FirstbitHi { dst: _, src }
            | Sm4Inst::FirstbitLo { dst: _, src }
            | Sm4Inst::FirstbitShi { dst: _, src } => scan_src_regs(src, &mut scan_reg),
            Sm4Inst::Bfi {
                dst: _,
                width,
                offset,
                insert,
                base,
            } => {
                scan_src_regs(width, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
                scan_src_regs(insert, &mut scan_reg);
                scan_src_regs(base, &mut scan_reg);
            }
            Sm4Inst::Ubfe {
                width, offset, src, ..
            }
            | Sm4Inst::Ibfe {
                width, offset, src, ..
            } => {
                scan_src_regs(width, &mut scan_reg);
                scan_src_regs(offset, &mut scan_reg);
                scan_src_regs(src, &mut scan_reg);
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
            Sm4Inst::Setp { a, b, .. } => {
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::ResInfo {
                dst: _,
                mip_level,
                texture: _,
            } => {
                scan_src_regs(mip_level, &mut scan_reg);
            }
            Sm4Inst::LdRaw { addr, .. } | Sm4Inst::LdUavRaw { addr, .. } => {
                scan_src_regs(addr, &mut scan_reg)
            }
            Sm4Inst::StoreRaw { addr, value, .. } => {
                scan_src_regs(addr, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::LdStructured { index, offset, .. }
            | Sm4Inst::LdStructuredUav { index, offset, .. } => {
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
            Sm4Inst::StoreUavTyped { coord, value, .. } => {
                scan_src_regs(coord, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::Sync { .. } => {}
            Sm4Inst::AtomicAdd { addr, value, .. } => {
                scan_src_regs(addr, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::Switch { selector } => scan_src_regs(selector, &mut scan_reg),
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => {}
            Sm4Inst::Emit { .. }
            | Sm4Inst::Cut { .. }
            | Sm4Inst::EmitThenCut { .. }
            | Sm4Inst::BufInfoRaw { .. }
            | Sm4Inst::BufInfoStructured { .. }
            | Sm4Inst::BufInfoRawUav { .. }
            | Sm4Inst::BufInfoStructuredUav { .. }
            | Sm4Inst::Unknown { .. }
            | Sm4Inst::Ret => {}
            Sm4Inst::Predicated { .. } => unreachable!("predication wrapper was stripped above"),
        }
    }
    inputs
}

#[deny(unreachable_patterns)]
fn scan_used_compute_sivs(module: &Sm4Module, io: &IoMaps) -> BTreeSet<ComputeSysValue> {
    let used_regs = scan_used_input_registers(module);
    let mut out = BTreeSet::<ComputeSysValue>::new();
    for reg in used_regs {
        if let Some(siv) = io.cs_inputs.get(&reg) {
            out.insert(*siv);
        }
    }

    // SM5 compute shaders can also reference thread IDs using dedicated operand types (e.g.
    // `D3D11_SB_OPERAND_TYPE_INPUT_THREAD_ID`) rather than the regular `v#` register file.
    // Those are represented in our IR as `SrcKind::ComputeBuiltin`, and must also contribute to
    // the set of builtins we declare on the WGSL entry point.
    let mut scan_src = |src: &crate::sm4_ir::SrcOperand| {
        if let SrcKind::ComputeBuiltin(builtin) = src.kind {
            out.insert(ComputeSysValue::from_compute_builtin(builtin));
        }
    };
    for inst in &module.instructions {
        let mut inst = inst;
        while let Sm4Inst::Predicated { inner, .. } = inst {
            inst = inner.as_ref();
        }

        match inst {
            Sm4Inst::If { cond, .. } => scan_src(cond),
            Sm4Inst::IfC { a, b, .. }
            | Sm4Inst::BreakC { a, b, .. }
            | Sm4Inst::ContinueC { a, b, .. } => {
                scan_src(a);
                scan_src(b);
            }
            Sm4Inst::Discard { cond, .. } => scan_src(cond),
            Sm4Inst::Else
            | Sm4Inst::EndIf
            | Sm4Inst::Loop
            | Sm4Inst::EndLoop
            | Sm4Inst::Continue => {}
            Sm4Inst::Mov { dst: _, src }
            | Sm4Inst::Utof { dst: _, src }
            | Sm4Inst::Itof { dst: _, src }
            | Sm4Inst::Ftoi { dst: _, src }
            | Sm4Inst::Ftou { dst: _, src }
            | Sm4Inst::F32ToF16 { dst: _, src }
            | Sm4Inst::F16ToF32 { dst: _, src } => scan_src(src),
            Sm4Inst::Movc { dst: _, cond, a, b } => {
                scan_src(cond);
                scan_src(a);
                scan_src(b);
            }
            Sm4Inst::Setp { a, b, .. } => {
                scan_src(a);
                scan_src(b);
            }
            Sm4Inst::And { dst: _, a, b }
            | Sm4Inst::Add { dst: _, a, b }
            | Sm4Inst::IAdd { dst: _, a, b }
            | Sm4Inst::ISub { dst: _, a, b }
            | Sm4Inst::IMul { a, b, .. }
            | Sm4Inst::UMul { a, b, .. }
            | Sm4Inst::Or { dst: _, a, b }
            | Sm4Inst::Xor { dst: _, a, b }
            | Sm4Inst::IShl { dst: _, a, b }
            | Sm4Inst::IShr { dst: _, a, b }
            | Sm4Inst::UShr { dst: _, a, b }
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
                dst_carry: _,
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
            | Sm4Inst::Cmp {
                dst: _,
                a,
                b,
                op: _,
                ty: _,
            }
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
                scan_src(a);
                scan_src(b);
            }
            Sm4Inst::Mad { dst: _, a, b, c }
            | Sm4Inst::IMad { a, b, c, .. }
            | Sm4Inst::UMad { a, b, c, .. } => {
                scan_src(a);
                scan_src(b);
                scan_src(c);
            }
            Sm4Inst::Bfi {
                dst: _,
                width,
                offset,
                insert,
                base,
            } => {
                scan_src(width);
                scan_src(offset);
                scan_src(insert);
                scan_src(base);
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
                scan_src(width);
                scan_src(offset);
                scan_src(src);
            }
            Sm4Inst::Rcp { dst: _, src }
            | Sm4Inst::Rsq { dst: _, src }
            | Sm4Inst::Not { dst: _, src }
            | Sm4Inst::Clip { src }
            | Sm4Inst::IAbs { dst: _, src }
            | Sm4Inst::Bfrev { dst: _, src }
            | Sm4Inst::CountBits { dst: _, src }
            | Sm4Inst::FirstbitHi { dst: _, src }
            | Sm4Inst::FirstbitLo { dst: _, src }
            | Sm4Inst::FirstbitShi { dst: _, src }
            | Sm4Inst::INeg { dst: _, src } => scan_src(src),
            Sm4Inst::Sample {
                dst: _,
                coord,
                texture: _,
                sampler: _,
            } => scan_src(coord),
            Sm4Inst::SampleL {
                dst: _,
                coord,
                texture: _,
                sampler: _,
                lod,
            } => {
                scan_src(coord);
                scan_src(lod);
            }
            Sm4Inst::Ld {
                dst: _, coord, lod, ..
            } => {
                scan_src(coord);
                scan_src(lod);
            }
            Sm4Inst::ResInfo { mip_level, .. } => scan_src(mip_level),
            Sm4Inst::LdRaw { addr, .. } => scan_src(addr),
            Sm4Inst::LdUavRaw { addr, .. } => scan_src(addr),
            Sm4Inst::StoreRaw { addr, value, .. } => {
                scan_src(addr);
                scan_src(value);
            }
            Sm4Inst::StoreUavTyped { coord, value, .. } => {
                scan_src(coord);
                scan_src(value);
            }
            Sm4Inst::LdStructured { index, offset, .. } => {
                scan_src(index);
                scan_src(offset);
            }
            Sm4Inst::LdStructuredUav { index, offset, .. } => {
                scan_src(index);
                scan_src(offset);
            }
            Sm4Inst::StoreStructured {
                index,
                offset,
                value,
                ..
            } => {
                scan_src(index);
                scan_src(offset);
                scan_src(value);
            }
            Sm4Inst::AtomicAdd { addr, value, .. } => {
                scan_src(addr);
                scan_src(value);
            }
            Sm4Inst::Sync { .. } => {}
            Sm4Inst::Switch { selector } => scan_src(selector),
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch | Sm4Inst::Break => {}
            Sm4Inst::Emit { .. }
            | Sm4Inst::Cut { .. }
            | Sm4Inst::EmitThenCut { .. }
            | Sm4Inst::BufInfoRaw { .. }
            | Sm4Inst::BufInfoStructured { .. }
            | Sm4Inst::BufInfoRawUav { .. }
            | Sm4Inst::BufInfoStructuredUav { .. }
            | Sm4Inst::Unknown { .. }
            | Sm4Inst::Ret => {}
            Sm4Inst::Predicated { .. } => unreachable!("predication wrapper was stripped above"),
        }
    }
    out
}

#[derive(Debug, Clone)]
struct ParamInfo {
    param: DxbcSignatureParameter,
    sys_value: Option<u32>,
    builtin: Option<Builtin>,
    scalar_ty: WgslScalarTy,
    wgsl_ty: &'static str,
    component_count: usize,
    components: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WgslScalarTy {
    F32,
    U32,
    I32,
}

impl WgslScalarTy {
    fn from_d3d_component_type(component_type: u32) -> Option<Self> {
        // `D3D_REGISTER_COMPONENT_TYPE` values as stored in DXBC signature tables.
        //
        // https://learn.microsoft.com/en-us/windows/win32/api/d3dcommon/ne-d3dcommon-d3d_register_component_type
        match component_type {
            1 => Some(Self::U32),
            2 => Some(Self::I32),
            3 => Some(Self::F32),
            _ => None,
        }
    }
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

        // Default to float for legacy signatures that omit `component_type` (0).
        let base_ty = WgslScalarTy::from_d3d_component_type(param.component_type)
            .unwrap_or(WgslScalarTy::F32);
        let wgsl_ty = match (base_ty, count) {
            (WgslScalarTy::F32, 1) => "f32",
            (WgslScalarTy::F32, 2) => "vec2<f32>",
            (WgslScalarTy::F32, 3) => "vec3<f32>",
            (WgslScalarTy::F32, 4) => "vec4<f32>",
            (WgslScalarTy::U32, 1) => "u32",
            (WgslScalarTy::U32, 2) => "vec2<u32>",
            (WgslScalarTy::U32, 3) => "vec3<u32>",
            (WgslScalarTy::U32, 4) => "vec4<u32>",
            (WgslScalarTy::I32, 1) => "i32",
            (WgslScalarTy::I32, 2) => "vec2<i32>",
            (WgslScalarTy::I32, 3) => "vec3<i32>",
            (WgslScalarTy::I32, 4) => "vec4<i32>",
            _ => "vec4<f32>",
        };

        // WGSL builtin inputs have fixed types that do not match the signature component count.
        // We still keep the vec4<f32> internal register model and expand scalar builtins into x
        // with D3D default fill (0,0,0,1).
        let (scalar_ty, wgsl_ty, component_count, components) = match builtin {
            Some(Builtin::VertexIndex)
            | Some(Builtin::InstanceIndex)
            | Some(Builtin::PrimitiveIndex)
            | Some(Builtin::GsInstanceIndex)
            | Some(Builtin::LocalInvocationIndex) => (WgslScalarTy::U32, "u32", 1, [0, 0, 0, 0]),
            Some(Builtin::FrontFacing) => (WgslScalarTy::U32, "bool", 1, [0, 0, 0, 0]),
            Some(Builtin::GlobalInvocationId)
            | Some(Builtin::LocalInvocationId)
            | Some(Builtin::WorkgroupId) => (WgslScalarTy::U32, "vec3<u32>", 3, [0, 1, 2, 0]),
            _ => (base_ty, wgsl_ty, count, comps),
        };

        Ok(Self {
            param: param.clone(),
            scalar_ty,
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
    ps_sv_depth: Option<ParamInfo>,
    vs_vertex_id_register: Option<u32>,
    vs_instance_id_register: Option<u32>,
    ps_front_facing_register: Option<u32>,
    cs_inputs: BTreeMap<u32, ComputeSysValue>,
    hs_inputs: BTreeMap<u32, HullSysValue>,
    /// Registers declared in the HS input signature (ISGN).
    ///
    /// Used to disambiguate indexed hull inputs (`SrcKind::GsInput`) between input and output
    /// patches.
    hs_input_regs: BTreeSet<u32>,
    /// Registers declared in the HS control-point output signature (OSGN).
    ///
    /// When the patch-constant phase reads an `OutputPatch` in HLSL, FXC encodes those reads as a
    /// 2D-indexed input operand (`SrcKind::GsInput`) where the register index matches the OSGN
    /// output register.
    hs_cp_output_regs: BTreeSet<u32>,

    /// True when this `IoMaps` instance is used for HS patch-constant phase emission.
    hs_is_patch_constant_phase: bool,
    /// Patch-constant outputs (non tess-factor) keyed by output register index -> combined mask.
    hs_pc_patch_constant_reg_masks: BTreeMap<u32, u8>,
    hs_pc_tess_factor_writes: Vec<HsTessFactorWrite>,
    hs_pc_tess_factor_stride: u32,
}

impl IoMaps {
    fn ps_depth_needs_dedicated_reg(&self) -> bool {
        let Some(depth) = &self.ps_sv_depth else {
            return false;
        };
        let depth_reg = depth.param.register;
        self.outputs
            .values()
            .any(|p| p.sys_value == Some(D3D_NAME_TARGET) && p.param.register == depth_reg)
    }

    fn ps_depth_var(&self) -> Result<String, ShaderTranslateError> {
        let depth = self
            .ps_sv_depth
            .as_ref()
            .ok_or(ShaderTranslateError::MissingSignature(
                "pixel output SV_Depth",
            ))?;
        let depth_reg = depth.param.register;
        if self.ps_depth_needs_dedicated_reg() {
            Ok("oDepth".to_owned())
        } else {
            Ok(format!("o{depth_reg}"))
        }
    }

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
        let mut out: Vec<IoParam> = self
            .outputs
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
            .collect();

        // Pixel depth outputs (`SV_Depth*`) use a dedicated register file (`oDepth`) and may share a
        // register index with `SV_Target` outputs in the signature table. Keep them out of the
        // regular `outputs` map to avoid false conflicts, but still expose them via reflection so
        // callers see the full pixel output signature.
        if let Some(p) = &self.ps_sv_depth {
            out.push(IoParam {
                semantic_name: p.param.semantic_name.clone(),
                semantic_index: p.param.semantic_index,
                register: p.param.register,
                location: None,
                builtin: None,
                mask: p.param.mask,
                stream: p.param.stream,
            });
        }

        out
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

        let has_depth = self.ps_sv_depth.is_some();
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
        if let Some(depth_param) = &self.ps_sv_depth {
            let depth_var = self.ps_depth_var()?;
            let depth_expr = apply_sig_mask_to_scalar(&depth_var, depth_param.param.mask);
            w.line(&format!("out.depth = {depth_expr};"));
        }
        w.line("return out;");
        Ok(())
    }

    fn emit_hs_commit_outputs(&self, w: &mut WgslWriter) {
        if !self.hs_is_patch_constant_phase {
            for &reg in self.outputs.keys() {
                w.line(&format!("hs_store_out_cp(hs_out_base + {reg}u, o{reg});"));
            }
            return;
        }

        // Patch constants (exclude tess factors).
        for (&reg, &mask) in &self.hs_pc_patch_constant_reg_masks {
            let expr = apply_sig_mask_to_vec4(&format!("o{reg}"), mask);
            w.line(&format!(
                "hs_store_patch_constants(hs_out_base + {reg}u, {expr});"
            ));
        }
        if !self.hs_pc_patch_constant_reg_masks.is_empty() && self.hs_pc_tess_factor_stride != 0 {
            w.line("");
        }

        // Tess factors (compact scalar buffer).
        if self.hs_pc_tess_factor_stride != 0 {
            w.line("let tf_base: u32 = hs_primitive_id * HS_TESS_FACTOR_STRIDE;");
            for wri in &self.hs_pc_tess_factor_writes {
                let comp = component_char(wri.src_component);
                w.line(&format!(
                    "hs_store_tess_factor(tf_base + {}u, o{}.{});",
                    wri.dst_index, wri.src_reg, comp
                ));
            }
        }
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
                    return Ok(expand_to_vec4_bitpattern(
                        "bitcast<f32>(input.vertex_id)",
                        p,
                    ));
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
                    return Ok(expand_to_vec4_bitpattern(
                        "bitcast<f32>(input.instance_id)",
                        p,
                    ));
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
                        let mut expr = if f.info.component_count == 1 {
                            base.clone()
                        } else {
                            format!("({base}).{lane_char}")
                        };
                        if f.info.scalar_ty != WgslScalarTy::F32 {
                            expr = format!("bitcast<f32>({expr})");
                        }

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
                    return Ok(expand_to_vec4_bitpattern(
                        "bitcast<f32>(input.primitive_id)",
                        p,
                    ));
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
            ShaderStage::Hull => {
                if let Some(siv) = self.hs_inputs.get(&reg) {
                    // Like `SV_VertexID`, HS system values are integer-typed in D3D. Preserve raw
                    // bits by bitcasting into the untyped `vec4<f32>` register model.
                    let expr = match siv {
                        HullSysValue::PrimitiveId => "hs_primitive_id",
                        HullSysValue::OutputControlPointId => "hs_output_control_point_id",
                    };
                    return Ok(format!(
                        "vec4<f32>(bitcast<f32>({expr}), 0.0, 0.0, bitcast<f32>(1u))"
                    ));
                }

                let p = self.inputs.get(&reg).ok_or(
                    ShaderTranslateError::SignatureMissingRegister {
                        io: "input",
                        register: reg,
                    },
                )?;
                // HS inputs are provided via an emulated "patch buffer" (storage buffer) and are
                // modeled as full `vec4<f32>` registers. Use a bounds-checked helper to avoid
                // undefined behaviour if the runtime provides a smaller scratch allocation than
                // the shader expects.
                Ok(apply_sig_mask_to_vec4(
                    &format!("hs_load_in(hs_in_base + {reg}u)"),
                    p.param.mask,
                ))
            }
            ShaderStage::Domain => {
                let p = self.inputs.get(&reg).ok_or(
                    ShaderTranslateError::SignatureMissingRegister {
                        io: "input",
                        register: reg,
                    },
                )?;
                if p.sys_value == Some(D3D_NAME_DOMAIN_LOCATION) {
                    return Ok(expand_to_vec4("ds_domain_location", p));
                }
                if p.sys_value == Some(D3D_NAME_PRIMITIVE_ID) {
                    return Ok(expand_to_vec4("bitcast<f32>(ds_primitive_id)", p));
                }

                // Domain shader patch-constant inputs are provided via HS patch-constant output
                // buffers. Treat all other `v#` inputs as patch-constant registers, even when the
                // signature marks them as system values (e.g. tess factors).
                Ok(apply_sig_mask_to_vec4(
                    &format!("ds_in_pc.data[ds_pc_base + {reg}u]"),
                    p.param.mask,
                ))
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
// Domain shader builtin: `SV_DomainLocation` (barycentric / domain coordinates).
const D3D_NAME_DOMAIN_LOCATION: u32 = 12;
// Hull shader built-in: `SV_OutputControlPointID`.
const D3D_NAME_OUTPUT_CONTROL_POINT_ID: u32 = 17;
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
        D3D_NAME_DOMAIN_LOCATION => Some(Builtin::DomainLocation),
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
    if is_sv_vertex_id(name) {
        return Some(D3D_NAME_VERTEX_ID);
    }
    if is_sv_primitive_id(name) {
        return Some(D3D_NAME_PRIMITIVE_ID);
    }
    if is_sv_domain_location(name) {
        return Some(D3D_NAME_DOMAIN_LOCATION);
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
    if is_sv_output_control_point_id(name) {
        return Some(D3D_NAME_OUTPUT_CONTROL_POINT_ID);
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

fn is_sv_domain_location(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_DomainLocation") || name.eq_ignore_ascii_case("SV_DOMAINLOCATION")
}

fn is_sv_tess_factor(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_TessFactor") || name.eq_ignore_ascii_case("SV_TESSFACTOR")
}

fn is_sv_inside_tess_factor(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_InsideTessFactor")
        || name.eq_ignore_ascii_case("SV_INSIDETESSFACTOR")
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

fn is_sv_output_control_point_id(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_OutputControlPointID")
        || name.eq_ignore_ascii_case("SV_OUTPUTCONTROLPOINTID")
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
    name.eq_ignore_ascii_case("SV_DepthGreaterEqual")
        || name.eq_ignore_ascii_case("SV_DEPTHGREATEREQUAL")
}

fn is_sv_depth_less_equal(name: &str) -> bool {
    name.eq_ignore_ascii_case("SV_DepthLessEqual") || name.eq_ignore_ascii_case("SV_DEPTHLESSEQUAL")
}

fn expand_to_vec4(expr: &str, p: &ParamInfo) -> String {
    // D3D input assembler fills missing components with (0,0,0,1). We apply the
    // same rule when expanding signature-typed values into internal vec4
    // registers.
    let mut src = Vec::<String>::with_capacity(p.component_count);
    let wrap_scalar = |scalar_expr: String| -> String {
        match p.scalar_ty {
            WgslScalarTy::F32 => scalar_expr,
            WgslScalarTy::U32 | WgslScalarTy::I32 => format!("bitcast<f32>({scalar_expr})"),
        }
    };
    match p.component_count {
        1 => src.push(wrap_scalar(expr.to_owned())),
        2 => {
            src.push(wrap_scalar(format!("{expr}.x")));
            src.push(wrap_scalar(format!("{expr}.y")));
        }
        3 => {
            src.push(wrap_scalar(format!("{expr}.x")));
            src.push(wrap_scalar(format!("{expr}.y")));
            src.push(wrap_scalar(format!("{expr}.z")));
        }
        4 => {
            // Most signature parameters are float vectors, so keep the common case cheap.
            if p.scalar_ty == WgslScalarTy::F32 {
                return expr.to_owned();
            }
            return format!("bitcast<vec4<f32>>({expr})");
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

fn expand_to_vec4_bitpattern(expr: &str, p: &ParamInfo) -> String {
    // Variant of `expand_to_vec4` used for values that are stored in the internal register model as
    // raw 32-bit *bit patterns* (e.g. integer system values bitcast into `f32` lanes).
    //
    // For these values, the default-fill lanes should use the integer bit patterns for 0/1, not
    // the float-typed `0.0`/`1.0` constants. Otherwise, interpreting the default W lane as an
    // integer (e.g. via `utof`) would treat the float `1.0` bit pattern (`0x3f800000`) as the
    // integer value `1065353216`.
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
            *out_comp = "bitcast<f32>(1u)".to_owned();
        } else {
            // `bitcast<f32>(0u)` is equivalent to `0.0` but makes the intent explicit.
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
    textures_2d: BTreeSet<u32>,
    textures_2d_array: BTreeSet<u32>,
    srv_buffers: BTreeSet<u32>,
    samplers: BTreeSet<u32>,
    uav_buffers: BTreeSet<u32>,
    uav_textures: BTreeMap<u32, StorageTextureFormat>,
    uavs_atomic: BTreeSet<u32>,
}

impl ResourceUsage {
    fn texture_is_array(&self, slot: u32) -> bool {
        self.textures_2d_array.contains(&slot)
    }

    fn stage_bind_group(stage: ShaderStage) -> u32 {
        match stage {
            ShaderStage::Vertex => 0,
            ShaderStage::Pixel => 1,
            ShaderStage::Compute => 2,
            // Extended D3D11 stages (GS/HS/DS) are currently executed via compute emulation passes.
            // Place their resources in a dedicated bind group so they don't collide with VS/PS/CS.
            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => 3,
            _ => 0,
        }
    }

    fn bindings(&self, stage: ShaderStage) -> Vec<Binding> {
        let visibility = match stage {
            ShaderStage::Vertex => wgpu::ShaderStages::VERTEX,
            ShaderStage::Pixel => wgpu::ShaderStages::FRAGMENT,
            ShaderStage::Compute => wgpu::ShaderStages::COMPUTE,
            // GS/HS/DS are executed via compute emulation paths, but still use their own stage-scoped
            // bind group (`@group(3)`) so they don't collide with true compute shader resources
            // (`@group(2)`). Since the pipeline is compute, their resources must be visible to
            // compute.
            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                wgpu::ShaderStages::COMPUTE
            }
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
        for &slot in &self.textures_2d {
            out.push(Binding {
                group,
                binding: BINDING_BASE_TEXTURE + slot,
                visibility,
                kind: BindingKind::Texture2D { slot },
            });
        }
        for &slot in &self.textures_2d_array {
            out.push(Binding {
                group,
                binding: BINDING_BASE_TEXTURE + slot,
                visibility,
                kind: BindingKind::Texture2DArray { slot },
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
        for (&slot, &format) in &self.uav_textures {
            out.push(Binding {
                group,
                binding: BINDING_BASE_UAV + slot,
                visibility,
                kind: BindingKind::UavTexture2DWriteOnly { slot, format },
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
        for &slot in &self.textures_2d {
            w.line(&format!(
                "@group({group}) @binding({}) var t{slot}: texture_2d<f32>;",
                BINDING_BASE_TEXTURE + slot
            ));
        }
        for &slot in &self.textures_2d_array {
            w.line(&format!(
                "@group({group}) @binding({}) var t{slot}: texture_2d_array<f32>;",
                BINDING_BASE_TEXTURE + slot
            ));
        }
        if !self.textures_2d.is_empty()
            || !self.textures_2d_array.is_empty()
            || !self.srv_buffers.is_empty()
        {
            w.line("");
        }
        let needs_u32_struct = !self.srv_buffers.is_empty()
            || self
                .uav_buffers
                .iter()
                .any(|slot| !self.uavs_atomic.contains(slot));
        let needs_atomic_struct = !self.uavs_atomic.is_empty();
        if needs_u32_struct || needs_atomic_struct {
            // WGSL requires storage buffers to have a `struct` as the top-level type; arrays
            // cannot be declared directly as a `var<storage>`.
            if needs_u32_struct {
                w.line("struct AeroStorageBufferU32 { data: array<u32> };");
            }
            if needs_atomic_struct {
                w.line("struct AeroStorageBufferAtomicU32 { data: array<atomic<u32>> };");
            }
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
            if self.uavs_atomic.contains(&slot) {
                w.line(&format!(
                    "@group({group}) @binding({}) var<storage, read_write> u{slot}: AeroStorageBufferAtomicU32;",
                    BINDING_BASE_UAV + slot
                ));
            } else {
                w.line(&format!(
                    "@group({group}) @binding({}) var<storage, read_write> u{slot}: AeroStorageBufferU32;",
                    BINDING_BASE_UAV + slot
                ));
            }
        }
        for (&slot, &format) in &self.uav_textures {
            w.line(&format!(
                "@group({group}) @binding({}) var u{slot}: texture_storage_2d<{}, write>;",
                BINDING_BASE_UAV + slot,
                format.wgsl_format()
            ));
        }
        if !self.uav_buffers.is_empty() || !self.uav_textures.is_empty() {
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
    // Collect all `t#` slots referenced by the shader instruction stream (and expand arrays using
    // `RDEF` when available) into `textures_2d` / `textures_2d_array`.
    let mut texture_slots = BTreeSet::new();
    let mut textures_2d = BTreeSet::new();
    let mut textures_2d_array = BTreeSet::new();
    let mut srv_buffers = BTreeSet::new();
    let mut samplers = BTreeSet::new();
    let mut uav_buffers = BTreeSet::new();
    let mut uavs_atomic = BTreeSet::new();
    let mut uavs_non_atomic_used = BTreeSet::new();
    let mut used_uav_texture_slots = BTreeSet::new();
    let mut declared_srv_buffers: BTreeMap<u32, (crate::sm4_ir::BufferKind, u32)> = BTreeMap::new();
    let mut declared_uav_buffers: BTreeMap<u32, (crate::sm4_ir::BufferKind, u32)> = BTreeMap::new();
    let mut declared_cbuffer_sizes: BTreeMap<u32, u32> = BTreeMap::new();
    let mut declared_uav_formats: BTreeMap<u32, u32> = BTreeMap::new();

    for decl in &module.decls {
        match decl {
            Sm4Decl::ConstantBuffer { slot, reg_count } => {
                let entry = declared_cbuffer_sizes.entry(*slot).or_insert(0);
                *entry = (*entry).max(*reg_count);
            }
            Sm4Decl::ResourceBuffer { slot, stride, kind } => {
                validate_slot("srv_buffer", *slot, MAX_TEXTURE_SLOTS)?;
                srv_buffers.insert(*slot);

                let entry = declared_srv_buffers
                    .entry(*slot)
                    .or_insert((*kind, *stride));
                // Prefer a larger stride if multiple declarations exist (defensive).
                entry.0 = *kind;
                entry.1 = entry.1.max(*stride);
            }
            Sm4Decl::UavBuffer { slot, stride, kind } => {
                validate_slot("uav_buffer", *slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(*slot);

                let entry = declared_uav_buffers
                    .entry(*slot)
                    .or_insert((*kind, *stride));
                entry.0 = *kind;
                entry.1 = entry.1.max(*stride);
            }
            Sm4Decl::UavTyped2D { slot, format } => {
                validate_slot("uav", *slot, MAX_UAV_SLOTS)?;
                declared_uav_formats.insert(*slot, *format);
            }
            _ => {}
        }
    }

    for inst in &module.instructions {
        let mut scan_src = |src: &crate::sm4_ir::SrcOperand| -> Result<(), ShaderTranslateError> {
            if let SrcKind::ConstantBuffer { slot, reg } = src.kind {
                // D3D11 only exposes `b0..b13` (14 slots) per stage, even though the binding model
                // could represent more without colliding with `t#` bindings.
                validate_slot("cbuffer", slot, D3D11_MAX_CONSTANT_BUFFER_SLOTS)?;
                let entry = cbuffers.entry(slot).or_insert(0);
                *entry = (*entry).max(reg + 1);
            }
            Ok(())
        };

        let mut inst = inst;
        while let Sm4Inst::Predicated { inner, .. } = inst {
            inst = inner;
        }

        match inst {
            Sm4Inst::If { cond, .. } => scan_src(cond)?,
            Sm4Inst::IfC { a, b, .. }
            | Sm4Inst::BreakC { a, b, .. }
            | Sm4Inst::ContinueC { a, b, .. } => {
                scan_src(a)?;
                scan_src(b)?;
            }
            Sm4Inst::Discard { cond, .. } => scan_src(cond)?,
            Sm4Inst::Clip { src } => scan_src(src)?,
            Sm4Inst::Else
            | Sm4Inst::EndIf
            | Sm4Inst::Loop
            | Sm4Inst::EndLoop
            | Sm4Inst::Break
            | Sm4Inst::Continue => {}
            Sm4Inst::Mov { dst: _, src } => scan_src(src)?,
            Sm4Inst::Movc { dst: _, cond, a, b } => {
                scan_src(cond)?;
                scan_src(a)?;
                scan_src(b)?;
            }
            Sm4Inst::Setp { a, b, .. }
            | Sm4Inst::Add { dst: _, a, b }
            | Sm4Inst::Mul { dst: _, a, b }
            | Sm4Inst::Dp3 { dst: _, a, b }
            | Sm4Inst::Dp4 { dst: _, a, b }
            | Sm4Inst::Min { dst: _, a, b }
            | Sm4Inst::Max { dst: _, a, b }
            | Sm4Inst::IMin { dst: _, a, b }
            | Sm4Inst::IMax { dst: _, a, b }
            | Sm4Inst::UMin { dst: _, a, b }
            | Sm4Inst::UMax { dst: _, a, b }
            | Sm4Inst::Cmp { dst: _, a, b, .. }
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
            | Sm4Inst::IAdd { dst: _, a, b }
            | Sm4Inst::ISub { dst: _, a, b }
            | Sm4Inst::IMul { a, b, .. }
            | Sm4Inst::UMul { a, b, .. }
            | Sm4Inst::And { dst: _, a, b }
            | Sm4Inst::Or { dst: _, a, b }
            | Sm4Inst::Xor { dst: _, a, b }
            | Sm4Inst::IShl { dst: _, a, b }
            | Sm4Inst::IShr { dst: _, a, b }
            | Sm4Inst::UShr { dst: _, a, b }
            | Sm4Inst::IAddC { a, b, .. }
            | Sm4Inst::UAddC { a, b, .. }
            | Sm4Inst::ISubC { a, b, .. }
            | Sm4Inst::USubB { a, b, .. } => {
                scan_src(a)?;
                scan_src(b)?;
            }
            Sm4Inst::IMad {
                dst_lo: _,
                dst_hi: _,
                a,
                b,
                c,
            }
            | Sm4Inst::UMad {
                dst_lo: _,
                dst_hi: _,
                a,
                b,
                c,
            }
            | Sm4Inst::Mad { dst: _, a, b, c } => {
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
            | Sm4Inst::INeg { dst: _, src }
            | Sm4Inst::Itof { dst: _, src }
            | Sm4Inst::Utof { dst: _, src }
            | Sm4Inst::Ftoi { dst: _, src }
            | Sm4Inst::Ftou { dst: _, src }
            | Sm4Inst::Not { dst: _, src }
            | Sm4Inst::F32ToF16 { dst: _, src }
            | Sm4Inst::F16ToF32 { dst: _, src } => scan_src(src)?,
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
                texture_slots.insert(texture.slot);
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
                texture_slots.insert(texture.slot);
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
                texture_slots.insert(texture.slot);
            }
            Sm4Inst::ResInfo {
                dst: _,
                mip_level,
                texture,
            } => {
                scan_src(mip_level)?;
                validate_slot("texture", texture.slot, MAX_TEXTURE_SLOTS)?;
                texture_slots.insert(texture.slot);
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
            Sm4Inst::LdUavRaw { dst: _, addr, uav } => {
                scan_src(addr)?;
                validate_slot("uav_buffer", uav.slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(uav.slot);
            }
            Sm4Inst::StoreRaw {
                uav, addr, value, ..
            } => {
                scan_src(addr)?;
                scan_src(value)?;
                validate_slot("uav_buffer", uav.slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(uav.slot);
                if uavs_atomic.contains(&uav.slot) {
                    return Err(ShaderTranslateError::UavMixedAtomicAndNonAtomicAccess {
                        slot: uav.slot,
                    });
                }
                uavs_non_atomic_used.insert(uav.slot);
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

                let (kind, stride) = declared_srv_buffers.get(&buffer.slot).copied().ok_or(
                    ShaderTranslateError::MissingStructuredBufferStride {
                        kind: "srv_buffer",
                        slot: buffer.slot,
                    },
                )?;
                if !matches!(kind, crate::sm4_ir::BufferKind::Structured) || stride == 0 {
                    return Err(ShaderTranslateError::MissingStructuredBufferStride {
                        kind: "srv_buffer",
                        slot: buffer.slot,
                    });
                }
                if (stride % 4) != 0 {
                    return Err(ShaderTranslateError::StructuredBufferStrideNotMultipleOf4 {
                        kind: "srv_buffer",
                        slot: buffer.slot,
                        stride_bytes: stride,
                    });
                }
            }
            Sm4Inst::LdStructuredUav {
                dst: _,
                index,
                offset,
                uav,
            } => {
                scan_src(index)?;
                scan_src(offset)?;
                validate_slot("uav_buffer", uav.slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(uav.slot);

                let (kind, stride) = declared_uav_buffers.get(&uav.slot).copied().ok_or(
                    ShaderTranslateError::MissingStructuredBufferStride {
                        kind: "uav_buffer",
                        slot: uav.slot,
                    },
                )?;
                if !matches!(kind, crate::sm4_ir::BufferKind::Structured) || stride == 0 {
                    return Err(ShaderTranslateError::MissingStructuredBufferStride {
                        kind: "uav_buffer",
                        slot: uav.slot,
                    });
                }
                if (stride % 4) != 0 {
                    return Err(ShaderTranslateError::StructuredBufferStrideNotMultipleOf4 {
                        kind: "uav_buffer",
                        slot: uav.slot,
                        stride_bytes: stride,
                    });
                }
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
                if uavs_atomic.contains(&uav.slot) {
                    return Err(ShaderTranslateError::UavMixedAtomicAndNonAtomicAccess {
                        slot: uav.slot,
                    });
                }
                uavs_non_atomic_used.insert(uav.slot);

                let (kind, stride) = declared_uav_buffers.get(&uav.slot).copied().ok_or(
                    ShaderTranslateError::MissingStructuredBufferStride {
                        kind: "uav_buffer",
                        slot: uav.slot,
                    },
                )?;
                if !matches!(kind, crate::sm4_ir::BufferKind::Structured) || stride == 0 {
                    return Err(ShaderTranslateError::MissingStructuredBufferStride {
                        kind: "uav_buffer",
                        slot: uav.slot,
                    });
                }
                if (stride % 4) != 0 {
                    return Err(ShaderTranslateError::StructuredBufferStrideNotMultipleOf4 {
                        kind: "uav_buffer",
                        slot: uav.slot,
                        stride_bytes: stride,
                    });
                }
            }
            Sm4Inst::AtomicAdd {
                dst: _,
                uav,
                addr,
                value,
            } => {
                scan_src(addr)?;
                scan_src(value)?;
                validate_slot("uav_buffer", uav.slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(uav.slot);
                if uavs_non_atomic_used.contains(&uav.slot) {
                    return Err(ShaderTranslateError::UavMixedAtomicAndNonAtomicAccess {
                        slot: uav.slot,
                    });
                }
                uavs_atomic.insert(uav.slot);
            }
            Sm4Inst::StoreUavTyped {
                uav,
                coord,
                value,
                mask: _,
            } => {
                scan_src(coord)?;
                scan_src(value)?;
                validate_slot("uav", uav.slot, MAX_UAV_SLOTS)?;
                used_uav_texture_slots.insert(uav.slot);
            }
            Sm4Inst::Sync { .. } => {}
            Sm4Inst::Switch { selector } => {
                scan_src(selector)?;
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => {}
            Sm4Inst::BufInfoRaw { dst: _, buffer }
            | Sm4Inst::BufInfoStructured { dst: _, buffer, .. } => {
                validate_slot("srv_buffer", buffer.slot, MAX_TEXTURE_SLOTS)?;
                srv_buffers.insert(buffer.slot);
            }
            Sm4Inst::BufInfoRawUav { dst: _, uav }
            | Sm4Inst::BufInfoStructuredUav { dst: _, uav, .. } => {
                validate_slot("uav_buffer", uav.slot, MAX_UAV_SLOTS)?;
                uav_buffers.insert(uav.slot);
            }
            Sm4Inst::Unknown { .. } => {}
            Sm4Inst::Emit { .. } | Sm4Inst::Cut { .. } | Sm4Inst::EmitThenCut { .. } => {}
            Sm4Inst::Ret => {}
            Sm4Inst::Predicated { .. } => unreachable!("predication wrapper was stripped above"),
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
            let reg_count_u64 = u64::from(cb.size).div_ceil(16);
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
                    if set_intersects_range(&texture_slots, res.bind_point, res.bind_count) {
                        // D3D_SRV_DIMENSION_TEXTURE2DARRAY
                        let is_array = res.dimension == 5;
                        expand_set_range(
                            &mut texture_slots,
                            res.bind_point,
                            res.bind_count,
                            MAX_TEXTURE_SLOTS,
                        );

                        let end = res
                            .bind_point
                            .saturating_add(res.bind_count)
                            .min(MAX_TEXTURE_SLOTS);
                        for slot in res.bind_point..end {
                            if is_array {
                                textures_2d_array.insert(slot);
                            } else {
                                textures_2d.insert(slot);
                            }
                        }
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
                // - D3D_SIT_UAV_RWSTRUCTURED
                // - D3D_SIT_UAV_RWBYTEADDRESS
                // - D3D_SIT_UAV_APPEND_STRUCTURED
                // - D3D_SIT_UAV_CONSUME_STRUCTURED
                // - D3D_SIT_UAV_RWSTRUCTURED_WITH_COUNTER
                //
                // Note: `D3D_SIT_UAV_RWTYPED` (4) may refer to typed UAV textures (RWTexture*) as
                // well as typed UAV buffers (RWBuffer*). Since this translator only models typed
                // UAV *textures* via `dcl_uav_typed`, do not use RDEF input-type 4 to expand the
                // `u#` UAV buffer slot set.
                6 | 8 | 9 | 10 | 11 => {
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

    // Any texture slots referenced by the instruction stream but not described by `RDEF` default to
    // `Texture2D` (common in stripped/shader-cache DXBC blobs).
    for slot in texture_slots {
        if !textures_2d_array.contains(&slot) {
            textures_2d.insert(slot);
        }
    }

    // `u#` slots are a single namespace in D3D11: a slot is either a UAV buffer or a UAV texture.
    // Reject shaders that try to use the same slot in both ways (this would otherwise result in
    // conflicting WGSL declarations and invalid bind group layouts).
    for &slot in &used_uav_texture_slots {
        if uav_buffers.contains(&slot) {
            return Err(ShaderTranslateError::UavSlotUsedAsBufferAndTexture { slot });
        }
    }
    // `t#` slots are likewise a single namespace for SRVs (textures and buffers share the same
    // binding indices in the Aero D3D11 binding model).
    for &slot in textures_2d.iter().chain(textures_2d_array.iter()) {
        if srv_buffers.contains(&slot) {
            return Err(ShaderTranslateError::TextureSlotUsedAsBufferAndTexture { slot });
        }
    }

    let mut uav_textures = BTreeMap::new();
    for slot in used_uav_texture_slots {
        let Some(&dxgi_format) = declared_uav_formats.get(&slot) else {
            return Err(ShaderTranslateError::MissingUavTypedDeclaration { slot });
        };
        let format = match dxgi_format {
            // DXGI_FORMAT_R8G8B8A8_UNORM
            28 => StorageTextureFormat::Rgba8Unorm,
            // DXGI_FORMAT_R8G8B8A8_SNORM
            31 => StorageTextureFormat::Rgba8Snorm,
            // DXGI_FORMAT_R8G8B8A8_UINT
            30 => StorageTextureFormat::Rgba8Uint,
            // DXGI_FORMAT_R8G8B8A8_SINT
            32 => StorageTextureFormat::Rgba8Sint,
            // DXGI_FORMAT_R16G16B16A16_FLOAT
            10 => StorageTextureFormat::Rgba16Float,
            // DXGI_FORMAT_R16G16B16A16_UINT
            12 => StorageTextureFormat::Rgba16Uint,
            // DXGI_FORMAT_R16G16B16A16_SINT
            14 => StorageTextureFormat::Rgba16Sint,
            // DXGI_FORMAT_R32G32_FLOAT
            16 => StorageTextureFormat::Rg32Float,
            // DXGI_FORMAT_R32G32_UINT
            17 => StorageTextureFormat::Rg32Uint,
            // DXGI_FORMAT_R32G32_SINT
            18 => StorageTextureFormat::Rg32Sint,
            // DXGI_FORMAT_R32G32B32A32_FLOAT
            2 => StorageTextureFormat::Rgba32Float,
            // DXGI_FORMAT_R32G32B32A32_UINT
            3 => StorageTextureFormat::Rgba32Uint,
            // DXGI_FORMAT_R32G32B32A32_SINT
            4 => StorageTextureFormat::Rgba32Sint,
            // DXGI_FORMAT_R32_FLOAT
            41 => StorageTextureFormat::R32Float,
            // DXGI_FORMAT_R32_UINT
            42 => StorageTextureFormat::R32Uint,
            // DXGI_FORMAT_R32_SINT
            43 => StorageTextureFormat::R32Sint,
            other => {
                return Err(ShaderTranslateError::UnsupportedUavTextureFormat {
                    slot,
                    format: other,
                });
            }
        };
        uav_textures.insert(slot, format);
    }

    Ok(ResourceUsage {
        cbuffers,
        textures_2d,
        textures_2d_array,
        srv_buffers,
        samplers,
        uav_buffers,
        uav_textures,
        uavs_atomic,
    })
}

fn emit_temp_and_output_decls(
    w: &mut WgslWriter,
    module: &Sm4Module,
    io: &IoMaps,
) -> Result<(), ShaderTranslateError> {
    let mut temps = BTreeSet::<u32>::new();
    let mut predicates = BTreeSet::<u32>::new();
    let mut outputs = BTreeSet::<u32>::new();
    let mut needs_depth_reg = false;
    let depth_reg = io.ps_sv_depth.as_ref().map(|p| p.param.register);

    for inst in &module.instructions {
        let mut scan_reg = |reg: RegisterRef| match reg.file {
            RegFile::Temp => {
                temps.insert(reg.index);
            }
            RegFile::Output => {
                outputs.insert(reg.index);
            }
            RegFile::OutputDepth => {
                // Depth output registers are mapped either to the signature's register index (when
                // it does not overlap with any color outputs) or to a dedicated `oDepth` temp.
                if io.ps_sv_depth.is_some() && io.ps_depth_needs_dedicated_reg() {
                    needs_depth_reg = true;
                } else if let Some(depth_reg) = depth_reg {
                    outputs.insert(depth_reg);
                } else {
                    outputs.insert(reg.index);
                }
            }
            RegFile::Input => {}
        };

        let mut inst = inst;
        while let Sm4Inst::Predicated { pred, inner } = inst {
            predicates.insert(pred.reg.index);
            inst = inner;
        }

        match inst {
            Sm4Inst::If { cond, .. } => {
                scan_src_regs(cond, &mut scan_reg);
            }
            Sm4Inst::IfC { a, b, .. }
            | Sm4Inst::BreakC { a, b, .. }
            | Sm4Inst::ContinueC { a, b, .. } => {
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::Discard { cond, .. } => {
                scan_src_regs(cond, &mut scan_reg);
            }
            Sm4Inst::Clip { src } => {
                scan_src_regs(src, &mut scan_reg);
            }
            Sm4Inst::Else
            | Sm4Inst::EndIf
            | Sm4Inst::Loop
            | Sm4Inst::EndLoop
            | Sm4Inst::Break
            | Sm4Inst::Continue => {}
            Sm4Inst::Mov { dst, src } => {
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
                dst_carry,
                a,
                b,
            } => {
                scan_reg(dst_diff.reg);
                scan_reg(dst_carry.reg);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::USubB {
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
            Sm4Inst::Setp { dst, op: _, a, b } => {
                predicates.insert(dst.reg.index);
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
            }
            Sm4Inst::IMul {
                dst_lo,
                dst_hi,
                a,
                b,
            }
            | Sm4Inst::UMul {
                dst_lo,
                dst_hi,
                a,
                b,
            } => {
                scan_reg(dst_lo.reg);
                if let Some(dst_hi) = dst_hi {
                    scan_reg(dst_hi.reg);
                }
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
            | Sm4Inst::IAdd { dst, a, b }
            | Sm4Inst::ISub { dst, a, b }
            | Sm4Inst::Or { dst, a, b }
            | Sm4Inst::Xor { dst, a, b }
            | Sm4Inst::IShl { dst, a, b }
            | Sm4Inst::IShr { dst, a, b }
            | Sm4Inst::UShr { dst, a, b }
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
            Sm4Inst::IMad {
                dst_lo,
                dst_hi,
                a,
                b,
                c,
            }
            | Sm4Inst::UMad {
                dst_lo,
                dst_hi,
                a,
                b,
                c,
            } => {
                scan_reg(dst_lo.reg);
                if let Some(dst_hi) = dst_hi {
                    scan_reg(dst_hi.reg);
                }
                scan_src_regs(a, &mut scan_reg);
                scan_src_regs(b, &mut scan_reg);
                scan_src_regs(c, &mut scan_reg);
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
            | Sm4Inst::INeg { dst, src }
            | Sm4Inst::Itof { dst, src }
            | Sm4Inst::Utof { dst, src }
            | Sm4Inst::Ftoi { dst, src }
            | Sm4Inst::Ftou { dst, src }
            | Sm4Inst::Not { dst, src }
            | Sm4Inst::F32ToF16 { dst, src }
            | Sm4Inst::F16ToF32 { dst, src } => {
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
            Sm4Inst::ResInfo { dst, mip_level, .. } => {
                scan_reg(dst.reg);
                scan_src_regs(mip_level, &mut scan_reg);
            }
            Sm4Inst::LdRaw { dst, addr, .. } | Sm4Inst::LdUavRaw { dst, addr, .. } => {
                scan_reg(dst.reg);
                scan_src_regs(addr, &mut scan_reg);
            }
            Sm4Inst::StoreRaw { addr, value, .. } => {
                scan_src_regs(addr, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::LdStructured {
                dst, index, offset, ..
            }
            | Sm4Inst::LdStructuredUav {
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
            Sm4Inst::AtomicAdd {
                dst,
                uav: _,
                addr,
                value,
            } => {
                if let Some(dst) = dst {
                    scan_reg(dst.reg);
                }
                scan_src_regs(addr, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::Sync { .. } => {}
            Sm4Inst::Switch { selector } => {
                scan_src_regs(selector, &mut scan_reg);
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => {}
            Sm4Inst::BufInfoRaw { dst, .. }
            | Sm4Inst::BufInfoStructured { dst, .. }
            | Sm4Inst::BufInfoRawUav { dst, .. }
            | Sm4Inst::BufInfoStructuredUav { dst, .. } => {
                scan_reg(dst.reg);
            }
            Sm4Inst::StoreUavTyped { coord, value, .. } => {
                scan_src_regs(coord, &mut scan_reg);
                scan_src_regs(value, &mut scan_reg);
            }
            Sm4Inst::Unknown { .. } => {}
            Sm4Inst::Emit { .. } | Sm4Inst::Cut { .. } | Sm4Inst::EmitThenCut { .. } => {}
            Sm4Inst::Ret => {}
            Sm4Inst::Predicated { .. } => unreachable!("predication wrapper was stripped above"),
        }
    }

    // Ensure we have internal output regs for any signature-declared outputs that are
    // not written by the shader body (common for unused varyings).
    for &reg in io.outputs.keys() {
        outputs.insert(reg);
    }
    if io.ps_sv_depth.is_some() {
        if io.ps_depth_needs_dedicated_reg() {
            needs_depth_reg = true;
        } else if let Some(depth_reg) = depth_reg {
            outputs.insert(depth_reg);
        }
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
    let has_preds = !predicates.is_empty();
    for &idx in &predicates {
        w.line(&format!("var p{idx}: vec4<bool> = vec4<bool>(false);"));
    }
    if has_preds {
        w.line("");
    }
    let has_outputs = !outputs.is_empty() || needs_depth_reg;
    for &idx in &outputs {
        w.line(&format!("var o{idx}: vec4<f32> = vec4<f32>(0.0);"));
    }
    if needs_depth_reg {
        w.line("var oDepth: vec4<f32> = vec4<f32>(0.0);");
    }
    if has_outputs {
        w.line("");
    }

    Ok(())
}

fn scan_src_regs(src: &crate::sm4_ir::SrcOperand, f: &mut impl FnMut(RegisterRef)) {
    match &src.kind {
        SrcKind::Register(r) => f(*r),
        // Indexed inputs (`v#[]`) are represented as `SrcKind::GsInput` in the IR. For register
        // scanning purposes treat them as reads from the input register file.
        SrcKind::GsInput { reg, .. } => f(RegisterRef {
            file: RegFile::Input,
            index: *reg,
        }),
        _ => {}
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

    #[derive(Debug)]
    enum CfFrame {
        Switch(SwitchFrame),
        Case,
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
    // Once we emit a conditional early-exit (`return`) some invocations may stop executing the
    // remainder of the shader. WGSL barriers require all invocations in a workgroup to reach them,
    // so any subsequent barrier-like operation is potentially non-uniform.
    let mut has_conditional_return = false;

    let fmt_case_values = |values: &[i32]| -> String {
        values
            .iter()
            .map(|v| format!("{v}i"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let close_case_body =
        |w: &mut WgslWriter, cf_stack: &mut Vec<CfFrame>| -> Result<(), ShaderTranslateError> {
            let Some(CfFrame::Case) = cf_stack.last() else {
                return Ok(());
            };

            // Close the WGSL case block.
            w.dedent();
            w.line("}");
            cf_stack.pop();
            Ok(())
        };

    let flush_pending_labels = |w: &mut WgslWriter,
                                cf_stack: &mut Vec<CfFrame>,
                                inst_index: usize|
     -> Result<(), ShaderTranslateError> {
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

        // If the label set contains a default label, we may need an extra empty clause stub, since
        // WGSL can't combine `default` with `case` selectors in a single clause.
        match (has_default, last_label) {
            (false, _) => {
                let selectors = fmt_case_values(&case_values);
                w.line(&format!("case {selectors}: {{"));
                w.indent();
                cf_stack.push(CfFrame::Case);
            }
            (true, SwitchLabel::Default) => {
                if !case_values.is_empty() {
                    let selectors = fmt_case_values(&case_values);
                    w.line(&format!("case {selectors}: {{"));
                    w.indent();
                    w.dedent();
                    w.line("}");
                }
                w.line("default: {");
                w.indent();
                cf_stack.push(CfFrame::Case);
            }
            (true, SwitchLabel::Case(_)) => {
                // Emit the default empty clause first so it can reach the case body.
                w.line("default: {");
                w.indent();
                w.dedent();
                w.line("}");
                let selectors = fmt_case_values(&case_values);
                w.line(&format!("case {selectors}: {{"));
                w.indent();
                cf_stack.push(CfFrame::Case);
            }
        }

        Ok(())
    };

    // Structured buffer access (`*_structured`) requires the element stride in bytes, which is
    // provided via `dcl_resource_structured` / `dcl_uav_structured`. Collect those declarations so
    // we can lower address calculations when emitting WGSL.
    let mut srv_buffer_decls = BTreeMap::<u32, (BufferKind, u32)>::new();
    let mut uav_buffer_decls = BTreeMap::<u32, (BufferKind, u32)>::new();
    let mut texture2d_decls = BTreeSet::<u32>::new();
    for decl in &module.decls {
        match decl {
            Sm4Decl::ResourceBuffer { slot, stride, kind } => {
                srv_buffer_decls.insert(*slot, (*kind, *stride));
            }
            Sm4Decl::UavBuffer { slot, stride, kind } => {
                uav_buffer_decls.insert(*slot, (*kind, *stride));
            }
            Sm4Decl::ResourceTexture2D { slot } => {
                texture2d_decls.insert(*slot);
            }
            _ => {}
        }
    }

    for (inst_index, inst) in module.instructions.iter().enumerate() {
        match inst {
            Sm4Inst::Case { value } => {
                close_case_body(w, &mut cf_stack)?;

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
                close_case_body(w, &mut cf_stack)?;

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
                // Close any open case body. If the clause falls through naturally (no `break;`),
                // reaching the end of the final clause still exits the `switch`.
                close_case_body(w, &mut cf_stack)?;

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
                    close_case_body(w, &mut cf_stack)?;
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

        let maybe_saturate = |dst: &crate::sm4_ir::DstOperand, expr: String| {
            if dst.saturate {
                format!("clamp(({expr}), vec4<f32>(0.0), vec4<f32>(1.0))")
            } else {
                expr
            }
        };
        // Helper for SM4/SM5 compare-based control-flow (`ifc`/`breakc`/`continuec`).
        //
        // DXBC registers are untyped 32-bit lanes, but these instructions perform scalar
        // floating-point comparisons by default. The `*_U` variants follow the D3D10/11 tokenized
        // program format and are **unordered** float compares (true when either operand is NaN).
        let emit_cmp = |op: Sm4CmpOp,
                        a: &crate::sm4_ir::SrcOperand,
                        b: &crate::sm4_ir::SrcOperand,
                        opcode: &'static str|
         -> Result<String, ShaderTranslateError> {
            let a_expr = emit_src_vec4(a, inst_index, opcode, ctx)?;
            let b_expr = emit_src_vec4(b, inst_index, opcode, ctx)?;

            let a_scalar = format!("({a_expr}).x");
            let b_scalar = format!("({b_expr}).x");
            Ok(emit_sm4_cmp_op_scalar_bool(op, &a_scalar, &b_scalar))
        };

        match inst {
            Sm4Inst::Switch { selector } => {
                // Integer instructions consume raw integer bits from the untyped register file.
                // Do not attempt to reinterpret float-typed sources as numeric integers.
                let selector_i = emit_src_vec4_i32(selector, inst_index, "switch", ctx)?;
                let selector = format!("({selector_i}).x");
                w.line(&format!("switch({selector}) {{"));
                w.indent();
                cf_stack.push(CfFrame::Switch(SwitchFrame::default()));
            }
            Sm4Inst::Break => {
                let inside_case = matches!(cf_stack.last(), Some(CfFrame::Case));
                let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                if !inside_case && !inside_loop {
                    return Err(ShaderTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop or switch case".to_owned(),
                        found: blocks
                            .last()
                            .map(|b| b.describe())
                            .unwrap_or_else(|| "none".to_owned()),
                    });
                }
                w.line("break;");
            }
            Sm4Inst::BreakC { op, a, b } => {
                let inside_case = matches!(cf_stack.last(), Some(CfFrame::Case));
                let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                if !inside_case && !inside_loop {
                    return Err(ShaderTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop or switch case".to_owned(),
                        found: blocks
                            .last()
                            .map(|b| b.describe())
                            .unwrap_or_else(|| "none".to_owned()),
                    });
                }
                let expr = emit_cmp(*op, a, b, "breakc")?;
                w.line(&format!("if ({expr}) {{"));
                w.indent();
                w.line("break;");
                w.dedent();
                w.line("}");
            }
            Sm4Inst::Continue => {
                let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                if !inside_loop {
                    return Err(ShaderTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: blocks
                            .last()
                            .map(|b| b.describe())
                            .unwrap_or_else(|| "none".to_owned()),
                    });
                }
                w.line("continue;");
            }
            Sm4Inst::ContinueC { op, a, b } => {
                let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                if !inside_loop {
                    return Err(ShaderTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: blocks
                            .last()
                            .map(|b| b.describe())
                            .unwrap_or_else(|| "none".to_owned()),
                    });
                }
                let expr = emit_cmp(*op, a, b, "continuec")?;
                w.line(&format!("if ({expr}) {{"));
                w.indent();
                w.line("continue;");
                w.dedent();
                w.line("}");
            }
            Sm4Inst::If { cond, test } => {
                let expr = emit_test_bool_scalar(cond, *test, inst_index, "if", ctx)?;
                w.line(&format!("if ({expr}) {{"));
                w.indent();
                blocks.push(BlockKind::If { has_else: false });
            }
            Sm4Inst::IfC { op, a, b } => {
                let expr = emit_cmp(*op, a, b, "ifc")?;
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
            Sm4Inst::EndIf => match blocks.last() {
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
            },
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
            Sm4Inst::Predicated { pred, inner } => {
                // Most predicated instructions can be expressed by emitting a WGSL `if` wrapper
                // around the inner op. Some DXBC instructions are structurally incompatible with
                // this lowering (e.g. predicating an `if` would create unbalanced structured control
                // flow). `ret` is also special: `emit_instructions` elides top-level `ret` tokens
                // because we always emit an explicit stage return sequence after instruction
                // emission. A predicated `ret` must therefore be handled directly here so that the
                // early-exit is preserved.
                if matches!(inner.as_ref(), Sm4Inst::Ret) {
                    let cond = emit_test_predicate_scalar(pred);
                    w.line(&format!("if ({cond}) {{"));
                    w.indent();
                    match ctx.stage {
                        ShaderStage::Vertex | ShaderStage::Domain => ctx.io.emit_vs_return(w)?,
                        ShaderStage::Pixel => ctx.io.emit_ps_return(w)?,
                        ShaderStage::Compute | ShaderStage::Hull => {
                            // Compute entry points return `()`.
                            w.line("return;");
                        }
                        other => return Err(ShaderTranslateError::UnsupportedStage(other)),
                    }
                    w.dedent();
                    w.line("}");
                    has_conditional_return = true;
                    continue;
                }

                if let Sm4Inst::Sample {
                    dst,
                    coord,
                    texture,
                    sampler,
                } = inner.as_ref()
                {
                    // `textureSample` uses implicit derivatives and therefore comes with WGSL/WebGPU
                    // uniformity requirements. A direct lowering of predicated `sample` as
                    // `if (p0.x) { textureSample(...) }` can violate those requirements when the
                    // predicate is non-uniform.
                    //
                    // Preserve the predicated write semantics by evaluating the sample
                    // unconditionally in uniform control flow, then guarding the destination write.
                    if ctx.stage == ShaderStage::Pixel {
                        let coord = emit_src_vec4(coord, inst_index, "sample", ctx)?;
                        let expr = format!(
                            "textureSample(t{}, s{}, ({coord}).xy)",
                            texture.slot, sampler.slot
                        );
                        let expr = maybe_saturate(dst, expr);
                        let tmp = format!("pred_sample_{inst_index}");
                        w.line(&format!("let {tmp}: vec4<f32> = {expr};"));
                        let cond = emit_test_predicate_scalar(pred);
                        w.line(&format!("if ({cond}) {{"));
                        w.indent();
                        emit_write_masked(w, dst.reg, dst.mask, tmp, inst_index, "sample", ctx)?;
                        w.dedent();
                        w.line("}");
                        continue;
                    }
                }

                // Predicated control-flow exits need access to the *current* structured control-flow
                // context (loop/switch stacks). The generic predication lowering below wraps the
                // inner instruction in a fresh `Sm4Module` and calls `emit_instructions`
                // recursively, which would reset that context and incorrectly reject `break` /
                // `continue` as malformed control flow.
                if matches!(inner.as_ref(), Sm4Inst::Break) {
                    let inside_case = matches!(cf_stack.last(), Some(CfFrame::Case));
                    let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                    if !inside_case && !inside_loop {
                        return Err(ShaderTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "loop or switch case".to_owned(),
                            found: blocks
                                .last()
                                .map(|b| b.describe())
                                .unwrap_or_else(|| "none".to_owned()),
                        });
                    }
                    let cond = emit_test_predicate_scalar(pred);
                    w.line(&format!("if ({cond}) {{"));
                    w.indent();
                    w.line("break;");
                    w.dedent();
                    w.line("}");
                    continue;
                }
                if let Sm4Inst::BreakC { op, a, b } = inner.as_ref() {
                    let inside_case = matches!(cf_stack.last(), Some(CfFrame::Case));
                    let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                    if !inside_case && !inside_loop {
                        return Err(ShaderTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "loop or switch case".to_owned(),
                            found: blocks
                                .last()
                                .map(|b| b.describe())
                                .unwrap_or_else(|| "none".to_owned()),
                        });
                    }
                    let cond = emit_test_predicate_scalar(pred);
                    let expr = emit_cmp(*op, a, b, "breakc")?;
                    w.line(&format!("if ({cond} && ({expr})) {{"));
                    w.indent();
                    w.line("break;");
                    w.dedent();
                    w.line("}");
                    continue;
                }
                if matches!(inner.as_ref(), Sm4Inst::Continue) {
                    let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                    if !inside_loop {
                        return Err(ShaderTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "loop".to_owned(),
                            found: blocks
                                .last()
                                .map(|b| b.describe())
                                .unwrap_or_else(|| "none".to_owned()),
                        });
                    }
                    let cond = emit_test_predicate_scalar(pred);
                    w.line(&format!("if ({cond}) {{"));
                    w.indent();
                    w.line("continue;");
                    w.dedent();
                    w.line("}");
                    continue;
                }
                if let Sm4Inst::ContinueC { op, a, b } = inner.as_ref() {
                    let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                    if !inside_loop {
                        return Err(ShaderTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "loop".to_owned(),
                            found: blocks
                                .last()
                                .map(|b| b.describe())
                                .unwrap_or_else(|| "none".to_owned()),
                        });
                    }
                    let cond = emit_test_predicate_scalar(pred);
                    let expr = emit_cmp(*op, a, b, "continuec")?;
                    w.line(&format!("if ({cond} && ({expr})) {{"));
                    w.indent();
                    w.line("continue;");
                    w.dedent();
                    w.line("}");
                    continue;
                }

                match inner.as_ref() {
                    Sm4Inst::If { .. } => {
                        return Err(ShaderTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "predicated_if".to_owned(),
                        })
                    }
                    Sm4Inst::IfC { .. } => {
                        return Err(ShaderTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "predicated_ifc".to_owned(),
                        })
                    }
                    Sm4Inst::Else => {
                        return Err(ShaderTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "predicated_else".to_owned(),
                        })
                    }
                    Sm4Inst::EndIf => {
                        return Err(ShaderTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "predicated_endif".to_owned(),
                        })
                    }
                    Sm4Inst::Sync { flags } => {
                        // Predication is lowered to WGSL `if` control flow. Barrier operations must
                        // be executed uniformly; even fence-only `sync` (lowered via
                        // `storageBarrier()`) is treated as a workgroup-level barrier by
                        // WebGPU/Naga and can deadlock when not reached by all invocations.
                        //
                        // We conservatively reject any predicated `sync` that would emit a WGSL
                        // barrier built-in.
                        if ctx.stage == ShaderStage::Compute {
                            let group_sync =
                                (flags & crate::sm4::opcode::SYNC_FLAG_THREAD_GROUP_SYNC) != 0;
                            let uav_fence = (flags & crate::sm4::opcode::SYNC_FLAG_UAV_MEMORY) != 0;
                            if group_sync || uav_fence {
                                return Err(ShaderTranslateError::UnsupportedInstruction {
                                    inst_index,
                                    opcode: "predicated_sync".to_owned(),
                                });
                            }
                        }
                    }
                    _ => {}
                }

                let cond = emit_test_predicate_scalar(pred);
                w.line(&format!("if ({cond}) {{"));
                w.indent();
                let inner_module = Sm4Module {
                    stage: module.stage,
                    model: module.model,
                    // Instruction predication does not alter the surrounding module's
                    // declarations. Preserve them here so predicating an instruction that depends
                    // on declarations (e.g. `resinfo`, structured buffer ops) still emits correctly.
                    decls: module.decls.clone(),
                    instructions: vec![inner.as_ref().clone()],
                };
                emit_instructions(w, &inner_module, ctx)?;
                w.dedent();
                w.line("}");
            }
            Sm4Inst::Setp { dst, op, a, b } => {
                // `Sm4CmpOp::*U` corresponds to `D3D10_SB_COMPARISON_*_U` / `D3D11_SB_COMPARISON_*_U`
                // ("unordered" float compares) rather than unsigned-integer compares.
                let a_expr = emit_src_vec4(a, inst_index, "setp", ctx)?;
                let b_expr = emit_src_vec4(b, inst_index, "setp", ctx)?;

                // Avoid repeating potentially complex expressions (swizzles/modifiers) per-lane when
                // lowering unordered comparisons.
                let a_var = format!("setp_a_{inst_index}");
                let b_var = format!("setp_b_{inst_index}");
                let cmp_var = format!("setp_cmp_{inst_index}");
                w.line(&format!("let {a_var} = {a_expr};"));
                w.line(&format!("let {b_var} = {b_expr};"));
                let cmp_expr = emit_sm4_cmp_op_vec4_bool(*op, &a_var, &b_var);
                w.line(&format!("let {cmp_var} = {cmp_expr};"));
                emit_write_masked_bool(w, *dst, cmp_var, inst_index, "setp")?;
            }
            Sm4Inst::Mov { dst, src } => {
                let rhs = emit_src_vec4(src, inst_index, "mov", ctx)?;
                let rhs = maybe_saturate(dst, rhs);
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "mov", ctx)?;
            }
            Sm4Inst::Movc { dst, cond, a, b } => {
                let a_vec = emit_src_vec4(a, inst_index, "movc", ctx)?;
                let b_vec = emit_src_vec4(b, inst_index, "movc", ctx)?;
                let cond_bool = emit_test_bool_vec4(
                    cond,
                    crate::sm4_ir::Sm4TestBool::NonZero,
                    inst_index,
                    "movc",
                    ctx,
                )?;
                let expr =
                    maybe_saturate(dst, format!("select(({b_vec}), ({a_vec}), {cond_bool})"));
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "movc", ctx)?;
            }
            Sm4Inst::And { dst, a, b } => {
                let a = emit_src_vec4_u32(a, inst_index, "and", ctx)?;
                let b = emit_src_vec4_u32(b, inst_index, "and", ctx)?;
                let rhs = format!("bitcast<vec4<f32>>(({a}) & ({b}))");
                emit_write_masked(w, dst.reg, dst.mask, rhs, inst_index, "and", ctx)?;
            }
            Sm4Inst::UMul {
                dst_lo,
                dst_hi,
                a,
                b,
            } => {
                let a = emit_src_vec4_u32_int(a, inst_index, "umul", ctx)?;
                let b = emit_src_vec4_u32_int(b, inst_index, "umul", ctx)?;
                let lo = format!("bitcast<vec4<f32>>((({a}) * ({b})))");
                emit_write_masked(w, dst_lo.reg, dst_lo.mask, lo, inst_index, "umul", ctx)?;

                if let Some(dst_hi) = dst_hi {
                    let hi_u = emit_u32_mul_hi(&a, &b);
                    let hi = format!("bitcast<vec4<f32>>({hi_u})");
                    emit_write_masked(w, dst_hi.reg, dst_hi.mask, hi, inst_index, "umul", ctx)?;
                }
            }
            Sm4Inst::IMul {
                dst_lo,
                dst_hi,
                a,
                b,
            } => {
                let a = emit_src_vec4_i32_int(a, inst_index, "imul", ctx)?;
                let b = emit_src_vec4_i32_int(b, inst_index, "imul", ctx)?;
                let lo = format!("bitcast<vec4<f32>>((({a}) * ({b})))");
                emit_write_masked(w, dst_lo.reg, dst_lo.mask, lo, inst_index, "imul", ctx)?;

                if let Some(dst_hi) = dst_hi {
                    let hi_i = emit_i32_mul_hi(&a, &b);
                    let hi = format!("bitcast<vec4<f32>>({hi_i})");
                    emit_write_masked(w, dst_hi.reg, dst_hi.mask, hi, inst_index, "imul", ctx)?;
                }
            }
            Sm4Inst::UMad {
                dst_lo,
                dst_hi,
                a,
                b,
                c,
            } => {
                let a = emit_src_vec4_u32_int(a, inst_index, "umad", ctx)?;
                let b = emit_src_vec4_u32_int(b, inst_index, "umad", ctx)?;
                let c = emit_src_vec4_u32_int(c, inst_index, "umad", ctx)?;
                let lo = format!("bitcast<vec4<f32>>((({a}) * ({b}) + ({c})))");
                emit_write_masked(w, dst_lo.reg, dst_lo.mask, lo, inst_index, "umad", ctx)?;

                if let Some(dst_hi) = dst_hi {
                    let hi_u = emit_u32_mad_hi(&a, &b, &c);
                    let hi = format!("bitcast<vec4<f32>>({hi_u})");
                    emit_write_masked(w, dst_hi.reg, dst_hi.mask, hi, inst_index, "umad", ctx)?;
                }
            }
            Sm4Inst::IMad {
                dst_lo,
                dst_hi,
                a,
                b,
                c,
            } => {
                let a = emit_src_vec4_i32_int(a, inst_index, "imad", ctx)?;
                let b = emit_src_vec4_i32_int(b, inst_index, "imad", ctx)?;
                let c = emit_src_vec4_i32_int(c, inst_index, "imad", ctx)?;
                let lo = format!("bitcast<vec4<f32>>((({a}) * ({b}) + ({c})))");
                emit_write_masked(w, dst_lo.reg, dst_lo.mask, lo, inst_index, "imad", ctx)?;

                if let Some(dst_hi) = dst_hi {
                    let hi_i = emit_i32_mad_hi(&a, &b, &c);
                    let hi = format!("bitcast<vec4<f32>>({hi_i})");
                    emit_write_masked(w, dst_hi.reg, dst_hi.mask, hi, inst_index, "imad", ctx)?;
                }
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
                dst_carry,
                a,
                b,
            } => {
                emit_sub_with_carry(w, "isubc", inst_index, dst_diff, dst_carry, a, b, ctx)?;
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
                let a_u = emit_src_vec4_u32(a, inst_index, "udiv", ctx)?;
                let b_u = emit_src_vec4_u32(b, inst_index, "udiv", ctx)?;
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
                let a_i = emit_src_vec4_i32(a, inst_index, "idiv", ctx)?;
                let b_i = emit_src_vec4_i32(b, inst_index, "idiv", ctx)?;
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
            Sm4Inst::IAdd { dst, a, b } => {
                let a = emit_src_vec4_i32(a, inst_index, "iadd", ctx)?;
                let b = emit_src_vec4_i32(b, inst_index, "iadd", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(({a}) + ({b}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "iadd", ctx)?;
            }
            Sm4Inst::ISub { dst, a, b } => {
                let a = emit_src_vec4_i32(a, inst_index, "isub", ctx)?;
                let b = emit_src_vec4_i32(b, inst_index, "isub", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(({a}) - ({b}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "isub", ctx)?;
            }
            Sm4Inst::Or { dst, a, b } => {
                let a = emit_src_vec4_u32(a, inst_index, "or", ctx)?;
                let b = emit_src_vec4_u32(b, inst_index, "or", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(({a}) | ({b}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "or", ctx)?;
            }
            Sm4Inst::Xor { dst, a, b } => {
                let a = emit_src_vec4_u32(a, inst_index, "xor", ctx)?;
                let b = emit_src_vec4_u32(b, inst_index, "xor", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(({a}) ^ ({b}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "xor", ctx)?;
            }
            Sm4Inst::Not { dst, src } => {
                let src = emit_src_vec4_u32(src, inst_index, "not", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(~({src}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "not", ctx)?;
            }
            Sm4Inst::IShl { dst, a, b } => {
                let a = emit_src_vec4_u32(a, inst_index, "ishl", ctx)?;
                let b = emit_src_vec4_u32(b, inst_index, "ishl", ctx)?;
                // DXBC shift ops mask the shift amount to 0..31 (lower 5 bits).
                let sh = format!("({b}) & vec4<u32>(31u)");
                let expr = format!("bitcast<vec4<f32>>(({a}) << ({sh}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ishl", ctx)?;
            }
            Sm4Inst::IShr { dst, a, b } => {
                let a = emit_src_vec4_i32(a, inst_index, "ishr", ctx)?;
                let b = emit_src_vec4_u32(b, inst_index, "ishr", ctx)?;
                // DXBC shift ops mask the shift amount to 0..31 (lower 5 bits).
                let sh = format!("({b}) & vec4<u32>(31u)");
                let expr = format!("bitcast<vec4<f32>>(({a}) >> ({sh}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ishr", ctx)?;
            }
            Sm4Inst::UShr { dst, a, b } => {
                let a = emit_src_vec4_u32(a, inst_index, "ushr", ctx)?;
                let b = emit_src_vec4_u32(b, inst_index, "ushr", ctx)?;
                // DXBC shift ops mask the shift amount to 0..31 (lower 5 bits).
                let sh = format!("({b}) & vec4<u32>(31u)");
                let expr = format!("bitcast<vec4<f32>>(({a}) >> ({sh}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ushr", ctx)?;
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
            Sm4Inst::Cmp { dst, a, b, op, ty } => {
                let opcode = "cmp";
                let cmp_expr = |a: &str, b: &str| match op {
                    CmpOp::Eq => format!("({a}) == ({b})"),
                    CmpOp::Ne => format!("({a}) != ({b})"),
                    CmpOp::Lt => format!("({a}) < ({b})"),
                    CmpOp::Le => format!("({a}) <= ({b})"),
                    CmpOp::Gt => format!("({a}) > ({b})"),
                    CmpOp::Ge => format!("({a}) >= ({b})"),
                };

                let cmp = match ty {
                    CmpType::F32 => {
                        let a = emit_src_vec4(a, inst_index, opcode, ctx)?;
                        let b = emit_src_vec4(b, inst_index, opcode, ctx)?;
                        cmp_expr(&a, &b)
                    }
                    CmpType::I32 => {
                        let a = emit_src_vec4_i32(a, inst_index, opcode, ctx)?;
                        let b = emit_src_vec4_i32(b, inst_index, opcode, ctx)?;
                        cmp_expr(&a, &b)
                    }
                    CmpType::U32 => {
                        let a = emit_src_vec4_u32(a, inst_index, opcode, ctx)?;
                        let b = emit_src_vec4_u32(b, inst_index, opcode, ctx)?;
                        cmp_expr(&a, &b)
                    }
                };

                // Convert the bool vector result into D3D-style predicate mask bits.
                let mask = format!("select(vec4<u32>(0u), vec4<u32>(0xffffffffu), {cmp})");
                let expr = format!("bitcast<vec4<f32>>({mask})");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, opcode, ctx)?;
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
            Sm4Inst::Itof { dst, src } => {
                let src_i = emit_src_vec4_i32(src, inst_index, "itof", ctx)?;
                let expr = maybe_saturate(dst, format!("vec4<f32>({src_i})"));
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "itof", ctx)?;
            }
            Sm4Inst::Utof { dst, src } => {
                let src_u = emit_src_vec4_u32(src, inst_index, "utof", ctx)?;
                let expr = maybe_saturate(dst, format!("vec4<f32>({src_u})"));
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "utof", ctx)?;
            }
            Sm4Inst::Ftoi { dst, src } => {
                let src_f = emit_src_vec4(src, inst_index, "ftoi", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(vec4<i32>({src_f}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ftoi", ctx)?;
            }
            Sm4Inst::Ftou { dst, src } => {
                let src_f = emit_src_vec4(src, inst_index, "ftou", ctx)?;
                let expr = format!("bitcast<vec4<f32>>(vec4<u32>({src_f}))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ftou", ctx)?;
            }
            Sm4Inst::F32ToF16 { dst, src } => {
                // DXBC `f32tof16` converts each component to a 16-bit float bit-pattern and stores
                // it in the low 16 bits of the destination lane (upper bits undefined/zero).
                //
                // WGSL `pack2x16float` returns a `u32` containing 2 packed half floats; pack the
                // input into the low half with a zero high half and mask to the low 16 bits so the
                // resulting lane matches DXBC's per-component layout.
                let src_f = emit_src_vec4(src, inst_index, "f32tof16", ctx)?;
                let src_f = maybe_saturate(dst, src_f);
                let pack_x = format!("pack2x16float(vec2<f32>(({src_f}).x, 0.0)) & 0xffffu");
                let pack_y = format!("pack2x16float(vec2<f32>(({src_f}).y, 0.0)) & 0xffffu");
                let pack_z = format!("pack2x16float(vec2<f32>(({src_f}).z, 0.0)) & 0xffffu");
                let pack_w = format!("pack2x16float(vec2<f32>(({src_f}).w, 0.0)) & 0xffffu");
                let expr = format!(
                    "bitcast<vec4<f32>>(vec4<u32>({pack_x}, {pack_y}, {pack_z}, {pack_w}))"
                );
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "f32tof16", ctx)?;
            }
            Sm4Inst::F16ToF32 { dst, src } => {
                // DXBC `f16tof32` interprets the low 16 bits of each lane as a 16-bit float bit
                // pattern and expands it to f32.
                //
                // Mask to the low 16 bits and use WGSL `unpack2x16float` (reading the low-half `.x`)
                // to do the conversion.
                // Operand modifiers would operate on the numeric f32 interpretation of the lane and
                // would therefore corrupt the packed binary16 bit pattern. Ignore them so we
                // preserve raw bits in the untyped register file.
                let mut src_nomod = src.clone();
                src_nomod.modifier = OperandModifier::None;
                let src_u = emit_src_vec4_u32(&src_nomod, inst_index, "f16tof32", ctx)?;
                let unpack_x = format!("unpack2x16float(({src_u}).x & 0xffffu).x");
                let unpack_y = format!("unpack2x16float(({src_u}).y & 0xffffu).x");
                let unpack_z = format!("unpack2x16float(({src_u}).z & 0xffffu).x");
                let unpack_w = format!("unpack2x16float(({src_u}).w & 0xffffu).x");
                let expr = format!("vec4<f32>({unpack_x}, {unpack_y}, {unpack_z}, {unpack_w})");
                let expr = maybe_saturate(dst, expr);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "f16tof32", ctx)?;
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
                let expr = format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3]);
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
                let expr = format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3]);
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
                let expr = format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3]);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ibfe", ctx)?;
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
            Sm4Inst::Discard { cond, test } => {
                let opcode_name = match test {
                    crate::sm4_ir::Sm4TestBool::Zero => "discard_z",
                    crate::sm4_ir::Sm4TestBool::NonZero => "discard_nz",
                };
                if ctx.stage != ShaderStage::Pixel {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: opcode_name.to_owned(),
                    });
                }

                let cmp = emit_test_bool_scalar(cond, *test, inst_index, "discard", ctx)?;

                w.line(&format!("if ({cmp}) {{"));
                w.indent();
                w.line("discard;");
                w.dedent();
                w.line("}");
            }
            Sm4Inst::Clip { src } => {
                if ctx.stage != ShaderStage::Pixel {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "clip".to_owned(),
                    });
                }

                let src_vec = emit_src_vec4(src, inst_index, "clip", ctx)?;
                w.line(&format!("if (any(({src_vec}) < vec4<f32>(0.0))) {{"));
                w.indent();
                w.line("discard;");
                w.dedent();
                w.line("}");
            }
            Sm4Inst::Sample {
                dst,
                coord,
                texture,
                sampler,
            } => {
                let coord = emit_src_vec4(coord, inst_index, "sample", ctx)?;
                let is_array = ctx.resources.texture_is_array(texture.slot);
                // WGSL forbids implicit-derivative sampling (`textureSample`) outside the fragment
                // stage, so map D3D-style `sample` to `textureSampleLevel(..., 0.0)` when
                // translating vertex/compute shaders.
                //
                // Note: On real D3D hardware, non-fragment `sample` uses an implementation-defined
                // LOD selection (typically base LOD). Using LOD 0 is a reasonable approximation and
                // keeps the generated WGSL valid.
                let expr = if is_array {
                    let slice = format!("i32(({coord}).z)");
                    match ctx.stage {
                        ShaderStage::Pixel => format!(
                            "textureSample(t{}, s{}, ({coord}).xy, {slice})",
                            texture.slot, sampler.slot
                        ),
                        _ => format!(
                            "textureSampleLevel(t{}, s{}, ({coord}).xy, {slice}, 0.0)",
                            texture.slot, sampler.slot
                        ),
                    }
                } else {
                    match ctx.stage {
                        ShaderStage::Pixel => format!(
                            "textureSample(t{}, s{}, ({coord}).xy)",
                            texture.slot, sampler.slot
                        ),
                        _ => format!(
                            "textureSampleLevel(t{}, s{}, ({coord}).xy, 0.0)",
                            texture.slot, sampler.slot
                        ),
                    }
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
                let expr = if ctx.resources.texture_is_array(texture.slot) {
                    let slice = format!("i32(({coord}).z)");
                    format!(
                        "textureSampleLevel(t{}, s{}, ({coord}).xy, {slice}, ({lod_vec}).x)",
                        texture.slot, sampler.slot
                    )
                } else {
                    format!(
                        "textureSampleLevel(t{}, s{}, ({coord}).xy, ({lod_vec}).x)",
                        texture.slot, sampler.slot
                    )
                };
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

                let expr = if ctx.resources.texture_is_array(texture.slot) {
                    let slice = format!("({coord_i}).z");
                    format!(
                        "textureLoad(t{}, vec2<i32>({x}, {y}), {slice}, {lod_scalar})",
                        texture.slot
                    )
                } else {
                    format!(
                        "textureLoad(t{}, vec2<i32>({x}, {y}), {lod_scalar})",
                        texture.slot
                    )
                };
                let expr = maybe_saturate(dst, expr);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ld", ctx)?;
            }
            Sm4Inst::ResInfo {
                dst,
                mip_level,
                texture,
            } => {
                // `resinfo` is used by `Texture2D.GetDimensions` and produces integer values.
                //
                // Output packing for `Texture2D`:
                // - x = width
                // - y = height
                // - z = 1
                // - w = mip level count
                //
                // DXBC register files are untyped; store the raw integer bits into our `vec4<f32>`
                // register model via a bitcast.
                if !texture2d_decls.contains(&texture.slot) {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "resinfo".to_owned(),
                    });
                }

                let mip_u = emit_src_vec4_u32(mip_level, inst_index, "resinfo", ctx)?;
                let level_i = format!("i32(({mip_u}).x)");
                let dims_name = format!("resinfo_dims{inst_index}");
                w.line(&format!(
                    "let {dims_name}: vec2<u32> = textureDimensions(t{}, {level_i});",
                    texture.slot
                ));
                let levels_name = format!("resinfo_levels{inst_index}");
                w.line(&format!(
                    "let {levels_name}: u32 = textureNumLevels(t{});",
                    texture.slot
                ));

                let expr = if ctx.resources.texture_is_array(texture.slot) {
                    let layers_name = format!("resinfo_layers{inst_index}");
                    w.line(&format!(
                        "let {layers_name}: u32 = textureNumLayers(t{});",
                        texture.slot
                    ));
                    format!(
                        "bitcast<vec4<f32>>(vec4<u32>(({dims_name}).x, ({dims_name}).y, {layers_name}, {levels_name}))"
                    )
                } else {
                    format!(
                        "bitcast<vec4<f32>>(vec4<u32>(({dims_name}).x, ({dims_name}).y, 1u, {levels_name}))"
                    )
                };
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "resinfo", ctx)?;
            }
            Sm4Inst::LdRaw { dst, addr, buffer } => {
                // Raw buffer loads operate on byte offsets. Model buffers as a storage
                // `array<u32>` and derive a word index from the byte address.
                let addr_u32 = emit_src_scalar_u32_addr(addr, inst_index, "ld_raw", ctx)?;
                let base_name = format!("ld_raw_base{inst_index}");
                w.line(&format!("let {base_name}: u32 = ({addr_u32}) / 4u;"));

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
                w.line(&format!(
                    "let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"
                ));

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
                // Raw UAV stores use byte offsets.
                //
                // DXBC register lanes are untyped 32-bit values; integer operations (including
                // buffer addressing) consume raw lane bits. Numeric float→int conversion must be
                // expressed explicitly in DXBC (e.g. via `ftou`), not inferred.
                let addr_u32 = emit_src_scalar_u32_addr(addr, inst_index, "store_raw", ctx)?;
                let base_name = format!("store_raw_base{inst_index}");
                w.line(&format!("let {base_name}: u32 = ({addr_u32}) / 4u;"));

                // Store raw bits. Buffer stores must preserve the underlying 32-bit lane patterns
                // (e.g. a `mov`-based `asuint` bitcast of `1.0` must store `0x3f800000`, not `1`).
                let value_u = emit_src_vec4_u32(value, inst_index, "store_raw", ctx)?;
                let value_name = format!("store_raw_val{inst_index}");
                w.line(&format!("let {value_name}: vec4<u32> = {value_u};"));

                let comps = [
                    ('x', 1u8, 0u32),
                    ('y', 2u8, 1),
                    ('z', 4u8, 2),
                    ('w', 8u8, 3),
                ];
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

                let index_u32 = emit_src_scalar_u32_addr(index, inst_index, "ld_structured", ctx)?;
                let offset_u32 =
                    emit_src_scalar_u32_addr(offset, inst_index, "ld_structured", ctx)?;
                // Keep index/offset in locals before multiplying by `stride`. Some address operands
                // are constant immediates (often from float-literal bit patterns), and constant
                // folding of overflowing `u32` multiplications can fail WGSL parsing.
                let index_name = format!("ld_struct_index{inst_index}");
                let offset_name = format!("ld_struct_offset{inst_index}");
                w.line(&format!("var {index_name}: u32 = ({index_u32});"));
                w.line(&format!("var {offset_name}: u32 = ({offset_u32});"));
                let base_name = format!("ld_struct_base{inst_index}");
                w.line(&format!(
                    "let {base_name}: u32 = (({index_name}) * {stride}u + ({offset_name})) / 4u;"
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
                w.line(&format!(
                    "let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"
                ));

                let expr = maybe_saturate(dst, f_name);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ld_structured", ctx)?;
            }
            Sm4Inst::LdStructuredUav {
                dst,
                index,
                offset,
                uav,
            } => {
                let Some((kind, stride)) = uav_buffer_decls.get(&uav.slot).copied() else {
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

                let index_u32 = emit_src_scalar_u32_addr(index, inst_index, "ld_structured", ctx)?;
                let offset_u32 =
                    emit_src_scalar_u32_addr(offset, inst_index, "ld_structured", ctx)?;
                let index_name = format!("ld_uav_struct_index{inst_index}");
                let offset_name = format!("ld_uav_struct_offset{inst_index}");
                w.line(&format!("var {index_name}: u32 = ({index_u32});"));
                w.line(&format!("var {offset_name}: u32 = ({offset_u32});"));
                let base_name = format!("ld_uav_struct_base{inst_index}");
                w.line(&format!(
                    "let {base_name}: u32 = (({index_name}) * {stride}u + ({offset_name})) / 4u;"
                ));

                let mask_bits = dst.mask.0 & 0xF;
                let load_lane = |bit: u8, offset: u32| {
                    if (mask_bits & bit) != 0 {
                        format!("u{}.data[{base_name} + {offset}u]", uav.slot)
                    } else {
                        "0u".to_owned()
                    }
                };

                let u_name = format!("ld_uav_struct_u{inst_index}");
                w.line(&format!(
                    "let {u_name}: vec4<u32> = vec4<u32>({}, {}, {}, {});",
                    load_lane(1, 0),
                    load_lane(2, 1),
                    load_lane(4, 2),
                    load_lane(8, 3),
                ));
                let f_name = format!("ld_uav_struct_f{inst_index}");
                w.line(&format!(
                    "let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"
                ));

                let expr = maybe_saturate(dst, f_name);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ld_structured", ctx)?;
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

                let index_u32 =
                    emit_src_scalar_u32_addr(index, inst_index, "store_structured", ctx)?;
                let offset_u32 =
                    emit_src_scalar_u32_addr(offset, inst_index, "store_structured", ctx)?;
                let index_name = format!("store_struct_index{inst_index}");
                let offset_name = format!("store_struct_offset{inst_index}");
                w.line(&format!("var {index_name}: u32 = ({index_u32});"));
                w.line(&format!("var {offset_name}: u32 = ({offset_u32});"));
                let base_name = format!("store_struct_base{inst_index}");
                w.line(&format!(
                    "let {base_name}: u32 = (({index_name}) * {stride}u + ({offset_name})) / 4u;"
                ));

                // Store raw bits (see `store_raw` rationale above).
                let value_u = emit_src_vec4_u32(value, inst_index, "store_structured", ctx)?;
                let value_name = format!("store_struct_val{inst_index}");
                w.line(&format!("let {value_name}: vec4<u32> = {value_u};"));

                let comps = [
                    ('x', 1u8, 0u32),
                    ('y', 2u8, 1),
                    ('z', 4u8, 2),
                    ('w', 8u8, 3),
                ];
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
                let expr = format!("bitcast<vec4<f32>>(vec4<u32>(({bytes}), 0u, 0u, 0u))");
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
                let expr =
                    format!("bitcast<vec4<f32>>(vec4<u32>(({elem_count}), ({stride}), 0u, 0u))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "bufinfo", ctx)?;
            }
            Sm4Inst::BufInfoRawUav { dst, uav } => {
                let dwords = format!("arrayLength(&u{}.data)", uav.slot);
                let bytes = format!("({dwords}) * 4u");
                // Output packing:
                // - x = total byte size
                // - yzw = 0
                let expr = format!("bitcast<vec4<f32>>(vec4<u32>(({bytes}), 0u, 0u, 0u))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "bufinfo", ctx)?;
            }
            Sm4Inst::BufInfoStructuredUav {
                dst,
                uav,
                stride_bytes,
            } => {
                let dwords = format!("arrayLength(&u{}.data)", uav.slot);
                let byte_size = format!("({dwords}) * 4u");
                let stride = format!("{}u", stride_bytes);
                let elem_count = format!("({byte_size}) / ({stride})");
                // Output packing:
                // - x = element count
                // - y = stride (bytes)
                // - zw = 0
                let expr =
                    format!("bitcast<vec4<f32>>(vec4<u32>(({elem_count}), ({stride}), 0u, 0u))");
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "bufinfo", ctx)?;
            }
            Sm4Inst::Sync { flags } => {
                if ctx.stage != ShaderStage::Compute {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "sync".to_owned(),
                    });
                }

                // We currently only model a small subset of `D3D11_SB_SYNC_FLAGS`.
                let known_flags = crate::sm4::opcode::SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY
                    | crate::sm4::opcode::SYNC_FLAG_UAV_MEMORY
                    | crate::sm4::opcode::SYNC_FLAG_THREAD_GROUP_SYNC;
                let unknown_flags = flags & !known_flags;
                if unknown_flags != 0 {
                    return Err(ShaderTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: format!("sync_unknown_flags(0x{unknown_flags:x})"),
                    });
                }

                let group_sync = (flags & crate::sm4::opcode::SYNC_FLAG_THREAD_GROUP_SYNC) != 0;
                if group_sync {
                    if has_conditional_return {
                        return Err(ShaderTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "sync_group_sync_after_conditional_return".to_owned(),
                        });
                    }

                    // SM5 `sync_*_t` instructions are workgroup barriers that optionally include
                    // storage/UAV memory ordering semantics.
                    //
                    // WGSL exposes:
                    // - `workgroupBarrier()` for control + workgroup-memory synchronization.
                    // - `storageBarrier()` for storage-buffer memory ordering.
                    //
                    // For memory semantics, WGSL uses separate barrier built-ins:
                    // - `workgroupBarrier()` for workgroup/TGSM ordering.
                    // - `storageBarrier()` for storage/UAV ordering.
                    //
                    // DXBC `sync` encodes these bits independently, so we select the minimal WGSL
                    // sequence:
                    // - GroupMemoryBarrierWithGroupSync(): `workgroupBarrier()`
                    // - DeviceMemoryBarrierWithGroupSync(): `storageBarrier()`
                    // - AllMemoryBarrierWithGroupSync(): `storageBarrier(); workgroupBarrier();`
                    //
                    // When emitting both, we put `storageBarrier()` first so the device-memory
                    // ordering is established before the final workgroup barrier.
                    let uav = (flags & crate::sm4::opcode::SYNC_FLAG_UAV_MEMORY) != 0;
                    let tgsm =
                        (flags & crate::sm4::opcode::SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY) != 0;
                    match (uav, tgsm) {
                        (true, true) => {
                            w.line("storageBarrier();");
                            w.line("workgroupBarrier();");
                        }
                        (true, false) => {
                            w.line("storageBarrier();");
                        }
                        (false, _) => {
                            // Includes TGSM-only barriers and the (rare) pure group-sync form.
                            w.line("workgroupBarrier();");
                        }
                    }
                } else {
                    // Fence-only variants (no `THREAD_GROUP_SYNC`) do not require all threads to
                    // participate in D3D; emitting a WGSL `workgroupBarrier()` would introduce a
                    // workgroup execution barrier that can deadlock if used in divergent control
                    // flow.
                    //
                    // For UAV/storage memory ordering we currently use `storageBarrier()` as the
                    // closest available WGSL mapping. Note that WGSL barrier built-ins (including
                    // `storageBarrier()`) still come with uniformity requirements in WebGPU/Naga, so
                    // this is not a perfect representation of DXBC's fence-only `sync` semantics.
                    //
                    // If the shader only requests TGSM/workgroup-memory ordering semantics, WGSL has
                    // no fence-only equivalent today, so we currently emit nothing as an
                    // approximation.
                    let uav_fence = (flags & crate::sm4::opcode::SYNC_FLAG_UAV_MEMORY) != 0;
                    if uav_fence {
                        // If this `sync` occurs within potentially divergent structured control
                        // flow, we currently refuse to translate it: WebGPU/Naga lower
                        // `storageBarrier()` as a workgroup-level barrier, which can deadlock when
                        // some invocations do not reach it.
                        //
                        // DXBC fence-only `sync` is allowed in divergent control flow, so there is
                        // no generally-correct lowering here without a true per-invocation memory
                        // fence primitive.
                        let in_structured_cf = !blocks.is_empty() || !cf_stack.is_empty();
                        if in_structured_cf {
                            return Err(ShaderTranslateError::UnsupportedInstruction {
                                inst_index,
                                opcode: "sync_fence_only_in_control_flow".to_owned(),
                            });
                        }
                        if has_conditional_return {
                            return Err(ShaderTranslateError::UnsupportedInstruction {
                                inst_index,
                                opcode: "sync_fence_only_after_conditional_return".to_owned(),
                            });
                        }
                        w.line("storageBarrier();");
                    } else if (flags & crate::sm4::opcode::SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY)
                        != 0
                    {
                        // NOTE: No-op approximation. Once we support TGSM/workgroup memory, we may be
                        // able to translate this more accurately (but we still must not emit a full
                        // `workgroupBarrier()` without `THREAD_GROUP_SYNC`).
                    }
                }
            }
            Sm4Inst::LdUavRaw { dst, addr, uav } => {
                // Raw UAV buffer loads operate on byte offsets. Model UAV buffers as a storage
                // `array<u32>` and derive a word index from the byte address.
                let addr_u32 = emit_src_scalar_u32_addr(addr, inst_index, "ld_uav_raw", ctx)?;
                let base_name = format!("ld_uav_raw_base{inst_index}");
                w.line(&format!("let {base_name}: u32 = ({addr_u32}) / 4u;"));

                let mask_bits = dst.mask.0 & 0xF;
                let load_lane = |bit: u8, offset: u32| {
                    if (mask_bits & bit) != 0 {
                        format!("u{}.data[{base_name} + {offset}u]", uav.slot)
                    } else {
                        "0u".to_owned()
                    }
                };

                let u_name = format!("ld_uav_raw_u{inst_index}");
                w.line(&format!(
                    "let {u_name}: vec4<u32> = vec4<u32>({}, {}, {}, {});",
                    load_lane(1, 0),
                    load_lane(2, 1),
                    load_lane(4, 2),
                    load_lane(8, 3),
                ));
                let f_name = format!("ld_uav_raw_f{inst_index}");
                w.line(&format!(
                    "let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"
                ));

                let expr = maybe_saturate(dst, f_name);
                emit_write_masked(w, dst.reg, dst.mask, expr, inst_index, "ld_uav_raw", ctx)?;
            }
            Sm4Inst::AtomicAdd {
                dst,
                uav,
                addr,
                value,
            } => {
                let addr_u32 = emit_src_scalar_u32_addr(addr, inst_index, "atomic_add", ctx)?;
                let value_u32 = emit_src_scalar_u32(value, inst_index, "atomic_add", ctx)?;
                let ptr = format!("&u{}.data[{addr_u32}]", uav.slot);

                match dst {
                    Some(dst) => {
                        let tmp = format!("atomic_old_{inst_index}");
                        w.line(&format!("let {tmp}: u32 = atomicAdd({ptr}, {value_u32});"));
                        let expr = format!("vec4<f32>(bitcast<f32>({tmp}))");
                        emit_write_masked(
                            w,
                            dst.reg,
                            dst.mask,
                            expr,
                            inst_index,
                            "atomic_add",
                            ctx,
                        )?;
                    }
                    None => {
                        w.line(&format!("atomicAdd({ptr}, {value_u32});"));
                    }
                }
            }
            Sm4Inst::StoreUavTyped {
                uav,
                coord,
                value,
                mask,
            } => {
                let format =
                    ctx.resources.uav_textures.get(&uav.slot).copied().ok_or(
                        ShaderTranslateError::MissingUavTypedDeclaration { slot: uav.slot },
                    )?;

                // DXBC `store_uav_typed` carries a write mask on the `u#` operand. WebGPU/WGSL
                // `textureStore()` always writes a whole texel, so partial component stores would
                // require a read-modify-write sequence (not supported yet).
                //
                // Many typed UAV formats have fewer than 4 channels (`r32*`, `rg32*`). For those,
                // ignore writes to unused components and require that all meaningful components be
                // present in the mask.
                let required_mask = match format {
                    StorageTextureFormat::R32Float
                    | StorageTextureFormat::R32Uint
                    | StorageTextureFormat::R32Sint => WriteMask::X.0,
                    StorageTextureFormat::Rg32Float
                    | StorageTextureFormat::Rg32Uint
                    | StorageTextureFormat::Rg32Sint => WriteMask::X.0 | WriteMask::Y.0,
                    _ => WriteMask::XYZW.0,
                };

                let mask_bits = mask.0 & 0xF;
                let effective_mask = mask_bits & required_mask;
                if effective_mask != 0 && effective_mask != required_mask {
                    return Err(ShaderTranslateError::UnsupportedWriteMask {
                        inst_index,
                        opcode: "store_uav_typed",
                        mask: *mask,
                    });
                }

                // Typed UAV stores use integer texel coordinates, similar to `ld`.
                //
                // DXBC registers are untyped; interpret the coordinate lanes strictly as integer
                // bits (bitcast `f32` -> `i32`) with no float-to-int heuristics.
                let coord_i = emit_src_vec4_i32(coord, inst_index, "store_uav_typed", ctx)?;
                let x = format!("({coord_i}).x");
                let y = format!("({coord_i}).y");

                let value = match format.store_value_type() {
                    StorageTextureValueType::F32 => {
                        emit_src_vec4(value, inst_index, "store_uav_typed", ctx)?
                    }
                    StorageTextureValueType::U32 => {
                        emit_src_vec4_u32(value, inst_index, "store_uav_typed", ctx)?
                    }
                    StorageTextureValueType::I32 => {
                        emit_src_vec4_i32(value, inst_index, "store_uav_typed", ctx)?
                    }
                };

                // Only emit the store when at least one meaningful component is enabled.
                if effective_mask != 0 {
                    w.line(&format!(
                        "textureStore(u{}, vec2<i32>({x}, {y}), {value});",
                        uav.slot
                    ));
                }
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
            Sm4Inst::EmitThenCut { stream } => {
                let opcode = if *stream == 0 {
                    "emitthen_cut".to_owned()
                } else {
                    format!("emitthen_cut_stream({stream})")
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
                has_conditional_return = true;

                match ctx.stage {
                    ShaderStage::Vertex | ShaderStage::Domain => ctx.io.emit_vs_return(w)?,
                    ShaderStage::Pixel => ctx.io.emit_ps_return(w)?,
                    ShaderStage::Compute => {
                        // Compute entry points return `()`.
                        w.line("return;");
                    }
                    ShaderStage::Hull => {
                        // Hull shaders are executed via compute emulation. Ensure we commit output
                        // registers into the stage interface buffers before returning early from a
                        // structured control-flow block.
                        ctx.io.emit_hs_commit_outputs(w);
                        w.line("return;");
                    }
                    other => {
                        return Err(ShaderTranslateError::UnsupportedStage(other));
                    }
                }
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => {
                unreachable!("switch label instructions handled at top of loop")
            }
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
        SrcKind::Register(reg) => match reg.file {
            RegFile::Temp => format!("r{}", reg.index),
            RegFile::Output => format!("o{}", reg.index),
            RegFile::OutputDepth => ctx.io.ps_depth_var()?,
            RegFile::Input => ctx.io.read_input_vec4(ctx.stage, reg.index)?,
        },
        SrcKind::GsInput { reg, vertex } => match ctx.stage {
            ShaderStage::Domain => format!(
                "ds_in_cp.data[(ds_patch_id * DS_MAX_CONTROL_POINTS + {vertex}u) * DS_CP_IN_STRIDE + {reg}u]"
            ),
            ShaderStage::Hull => {
                // Hull shaders can index both input patch control points and output patch control
                // points (when using `OutputPatch` in HLSL). Both are encoded as 2D-indexed input
                // operands; disambiguate by comparing against the HS input/output signature
                // register sets.
                let base = if ctx.io.hs_cp_output_regs.contains(reg)
                    && !ctx.io.hs_input_regs.contains(reg)
                {
                    "hs_load_out_cp"
                } else {
                    "hs_load_in"
                };
                let stride = if base == "hs_load_out_cp" {
                    "HS_CP_OUT_STRIDE"
                } else {
                    "HS_IN_STRIDE"
                };
                format!(
                    "{base}((hs_primitive_id * HS_CONTROL_POINTS_PER_PATCH + {vertex}u) * {stride} + {reg}u)"
                )
            }
            _ => return Err(ShaderTranslateError::UnsupportedStage(ctx.stage)),
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
        SrcKind::ComputeBuiltin(builtin) => {
            if ctx.stage != ShaderStage::Compute {
                return Err(ShaderTranslateError::UnsupportedStage(ctx.stage));
            }
            ComputeSysValue::from_compute_builtin(*builtin).expand_to_vec4()
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

/// Emits a WGSL boolean expression for SM4/SM5 "test bool" instructions (e.g. `if_z`, `discard_nz`).
///
/// SM4/SM5 registers are untyped 32-bit lanes. For consistency across all "zero/nonzero" tests we
/// interpret the test as a raw-bit check on the selected scalar lane *after* swizzle/modifiers are
/// applied:
/// - `Zero`: `bitcast<u32>(lane) == 0u`
/// - `NonZero`: `bitcast<u32>(lane) != 0u`
fn emit_test_bool_scalar(
    src: &crate::sm4_ir::SrcOperand,
    test: crate::sm4_ir::Sm4TestBool,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    let vec = emit_src_vec4(src, inst_index, opcode, ctx)?;
    let lane = format!("({vec}).x");
    let bits = format!("bitcast<u32>({lane})");
    Ok(match test {
        crate::sm4_ir::Sm4TestBool::Zero => format!("{bits} == 0u"),
        crate::sm4_ir::Sm4TestBool::NonZero => format!("{bits} != 0u"),
    })
}

/// Like [`emit_test_bool_scalar`], but returns a per-component `vec4<bool>` expression.
fn emit_test_bool_vec4(
    src: &crate::sm4_ir::SrcOperand,
    test: crate::sm4_ir::Sm4TestBool,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    let vec = emit_src_vec4(src, inst_index, opcode, ctx)?;
    let bits = format!("bitcast<vec4<u32>>({vec})");
    Ok(match test {
        crate::sm4_ir::Sm4TestBool::Zero => format!("{bits} == vec4<u32>(0u)"),
        crate::sm4_ir::Sm4TestBool::NonZero => format!("{bits} != vec4<u32>(0u)"),
    })
}

fn emit_test_predicate_scalar(pred: &crate::sm4_ir::PredicateOperand) -> String {
    let base = format!("p{}.{}", pred.reg.index, component_char(pred.component));
    if pred.invert {
        format!("!({base})")
    } else {
        base
    }
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
                RegFile::OutputDepth => ctx.io.ps_depth_var()?,
                RegFile::Input => ctx.io.read_input_vec4(ctx.stage, reg.index)?,
            };
            format!("bitcast<vec4<u32>>({expr})")
        }
        SrcKind::GsInput { reg, vertex } => match ctx.stage {
            ShaderStage::Domain => format!(
                "bitcast<vec4<u32>>(ds_in_cp.data[(ds_patch_id * DS_MAX_CONTROL_POINTS + {vertex}u) * DS_CP_IN_STRIDE + {reg}u])"
            ),
            ShaderStage::Hull => {
                let base = if ctx.io.hs_cp_output_regs.contains(reg)
                    && !ctx.io.hs_input_regs.contains(reg)
                {
                    "hs_load_out_cp"
                } else {
                    "hs_load_in"
                };
                let stride = if base == "hs_load_out_cp" {
                    "HS_CP_OUT_STRIDE"
                } else {
                    "HS_IN_STRIDE"
                };
                format!(
                    "bitcast<vec4<u32>>({base}((hs_primitive_id * HS_CONTROL_POINTS_PER_PATCH + {vertex}u) * {stride} + {reg}u))"
                )
            }
            _ => return Err(ShaderTranslateError::UnsupportedStage(ctx.stage)),
        },
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
        SrcKind::ComputeBuiltin(builtin) => {
            if ctx.stage != ShaderStage::Compute {
                return Err(ShaderTranslateError::UnsupportedStage(ctx.stage));
            }
            let f = ComputeSysValue::from_compute_builtin(*builtin).expand_to_vec4();
            format!("bitcast<vec4<u32>>({f})")
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

// Integer operations consume raw bits from the untyped register file via `emit_src_vec4_u32` /
// `emit_src_vec4_i32`. Any float→int numeric conversion must be expressed explicitly in DXBC via
// `ftoi`/`ftou` opcodes, not inferred heuristically.
fn emit_src_vec4_u32_int(
    src: &crate::sm4_ir::SrcOperand,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    emit_src_vec4_u32(src, inst_index, opcode, ctx)
}

/// Emits a scalar `u32` source operand for buffer addressing/indexing.
///
/// DXBC register files are untyped 32-bit lanes. Address-like operands are consumed as raw integer
/// bits; numeric float→int conversion must be expressed explicitly (e.g. via `ftou`), not inferred
/// heuristically.
fn emit_src_scalar_u32_addr(
    src: &crate::sm4_ir::SrcOperand,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    let u = emit_src_vec4_u32(src, inst_index, opcode, ctx)?;
    Ok(format!("({u}).x"))
}

/// Emits a `vec4<i32>` source for signed integer operations.
///
/// This is equivalent to `emit_src_vec4_i32`; it exists to mirror the `*_u32_int` helper and make
/// call sites for signed integer instructions clearer.
fn emit_src_vec4_i32_int(
    src: &crate::sm4_ir::SrcOperand,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    emit_src_vec4_i32(src, inst_index, opcode, ctx)
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
                RegFile::OutputDepth => ctx.io.ps_depth_var()?,
                RegFile::Input => ctx.io.read_input_vec4(ctx.stage, reg.index)?,
            };
            format!("bitcast<vec4<i32>>({expr})")
        }
        SrcKind::GsInput { reg, vertex } => match ctx.stage {
            ShaderStage::Domain => format!(
                "bitcast<vec4<i32>>(ds_in_cp.data[(ds_patch_id * DS_MAX_CONTROL_POINTS + {vertex}u) * DS_CP_IN_STRIDE + {reg}u])"
            ),
            ShaderStage::Hull => {
                let base = if ctx.io.hs_cp_output_regs.contains(reg)
                    && !ctx.io.hs_input_regs.contains(reg)
                {
                    "hs_load_out_cp"
                } else {
                    "hs_load_in"
                };
                let stride = if base == "hs_load_out_cp" {
                    "HS_CP_OUT_STRIDE"
                } else {
                    "HS_IN_STRIDE"
                };
                format!(
                    "bitcast<vec4<i32>>({base}((hs_primitive_id * HS_CONTROL_POINTS_PER_PATCH + {vertex}u) * {stride} + {reg}u))"
                )
            }
            _ => return Err(ShaderTranslateError::UnsupportedStage(ctx.stage)),
        },
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
        SrcKind::ComputeBuiltin(builtin) => {
            if ctx.stage != ShaderStage::Compute {
                return Err(ShaderTranslateError::UnsupportedStage(ctx.stage));
            }
            let f = ComputeSysValue::from_compute_builtin(*builtin).expand_to_vec4();
            format!("bitcast<vec4<i32>>({f})")
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

#[allow(clippy::too_many_arguments)]
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
    let a_expr = emit_src_vec4_u32(a, inst_index, opcode, ctx)?;
    let b_expr = emit_src_vec4_u32(b, inst_index, opcode, ctx)?;

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

#[allow(clippy::too_many_arguments)]
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
    let a_expr = emit_src_vec4_u32(a, inst_index, opcode, ctx)?;
    let b_expr = emit_src_vec4_u32(b, inst_index, opcode, ctx)?;

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

#[allow(clippy::too_many_arguments)]
fn emit_sub_with_carry(
    w: &mut WgslWriter,
    opcode: &'static str,
    inst_index: usize,
    dst_diff: &crate::sm4_ir::DstOperand,
    dst_carry: &crate::sm4_ir::DstOperand,
    a: &crate::sm4_ir::SrcOperand,
    b: &crate::sm4_ir::SrcOperand,
    ctx: &EmitCtx<'_>,
) -> Result<(), ShaderTranslateError> {
    // Treat sources as raw 32-bit integer lanes in the untyped register file.
    let a_expr = emit_src_vec4_u32(a, inst_index, opcode, ctx)?;
    let b_expr = emit_src_vec4_u32(b, inst_index, opcode, ctx)?;

    let a_var = format!("{opcode}_a_{inst_index}");
    let b_var = format!("{opcode}_b_{inst_index}");
    let diff_var = format!("{opcode}_diff_{inst_index}");
    let carry_var = format!("{opcode}_carry_{inst_index}");

    w.line(&format!("let {a_var} = {a_expr};"));
    w.line(&format!("let {b_var} = {b_expr};"));
    w.line(&format!("let {diff_var} = {a_var} - {b_var};"));
    // Carry flag for subtraction is the inverse of borrow: 1 when no borrow occurred.
    w.line(&format!(
        "let {carry_var} = select(vec4<u32>(0u), vec4<u32>(1u), {a_var} >= {b_var});"
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

fn emit_src_scalar_u32(
    src: &crate::sm4_ir::SrcOperand,
    inst_index: usize,
    opcode: &'static str,
    ctx: &EmitCtx<'_>,
) -> Result<String, ShaderTranslateError> {
    Ok(format!(
        "({}).x",
        emit_src_vec4_u32(src, inst_index, opcode, ctx)?
    ))
}

fn emit_u32_mul_hi(a: &str, b: &str) -> String {
    let lanes =
        ['x', 'y', 'z', 'w'].map(|c| format!("u32((u64(({a}).{c}) * u64(({b}).{c})) >> 32u)"));
    format!(
        "vec4<u32>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
}

fn emit_u32_mad_hi(a: &str, b: &str, c: &str) -> String {
    let lanes = ['x', 'y', 'z', 'w'].map(|lane| {
        format!("u32((u64(({a}).{lane}) * u64(({b}).{lane}) + u64(({c}).{lane})) >> 32u)")
    });
    format!(
        "vec4<u32>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
}

fn emit_i32_mul_hi(a: &str, b: &str) -> String {
    let lanes =
        ['x', 'y', 'z', 'w'].map(|c| format!("i32((i64(({a}).{c}) * i64(({b}).{c})) >> 32u)"));
    format!(
        "vec4<i32>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
}

fn emit_i32_mad_hi(a: &str, b: &str, c: &str) -> String {
    let lanes = ['x', 'y', 'z', 'w'].map(|lane| {
        format!("i32((i64(({a}).{lane}) * i64(({b}).{lane}) + i64(({c}).{lane})) >> 32u)")
    });
    format!(
        "vec4<i32>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
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
        RegFile::OutputDepth => ctx.io.ps_depth_var()?,
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

fn emit_write_masked_bool(
    w: &mut WgslWriter,
    dst: PredicateDstOperand,
    rhs: String,
    inst_index: usize,
    opcode: &'static str,
) -> Result<(), ShaderTranslateError> {
    let dst_expr = format!("p{}", dst.reg.index);

    // Mask is 4 bits.
    let mask_bits = dst.mask.0 & 0xF;
    if mask_bits == 0 {
        return Err(ShaderTranslateError::UnsupportedWriteMask {
            inst_index,
            opcode,
            mask: dst.mask,
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

fn emit_sm4_cmp_op_scalar_bool(op: Sm4CmpOp, a: &str, b: &str) -> String {
    match op {
        // Ordered comparisons.
        Sm4CmpOp::Eq => format!("({a}) == ({b})"),
        // NOTE: D3D encodes ordered and unordered variants separately; ordered "not equal" is
        // false when either operand is NaN. WGSL doesn't expose a NaN test in the standard library
        // (in the naga/WGSL version we target), so use the IEEE property that `NaN != NaN`.
        Sm4CmpOp::Ne => format!("((({a}) != ({b})) && (({a}) == ({a})) && (({b}) == ({b})))"),
        Sm4CmpOp::Lt => format!("({a}) < ({b})"),
        Sm4CmpOp::Ge => format!("({a}) >= ({b})"),
        Sm4CmpOp::Le => format!("({a}) <= ({b})"),
        Sm4CmpOp::Gt => format!("({a}) > ({b})"),
        // Unordered comparisons (`*_U`) are true if either operand is NaN.
        //
        // Use `x != x` to test for NaN (true iff NaN).
        Sm4CmpOp::EqU => format!("((({a}) == ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::NeU => format!("((({a}) != ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::LtU => format!("((({a}) < ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::GeU => format!("((({a}) >= ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::LeU => format!("((({a}) <= ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::GtU => format!("((({a}) > ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
    }
}

fn emit_sm4_cmp_op_vec4_bool(op: Sm4CmpOp, a_vec4: &str, b_vec4: &str) -> String {
    let comps = ['x', 'y', 'z', 'w'];
    let mut lanes = Vec::with_capacity(4);
    for c in comps {
        let a = format!("({a_vec4}).{c}");
        let b = format!("({b_vec4}).{c}");
        lanes.push(emit_sm4_cmp_op_scalar_bool(op, &a, &b));
    }
    format!(
        "vec4<bool>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
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
    use crate::sm4_ir::{PredicateOperand, PredicateRef};
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
            textures_2d: BTreeSet::new(),
            textures_2d_array: BTreeSet::new(),
            srv_buffers: BTreeSet::new(),
            samplers: BTreeSet::new(),
            uav_buffers,
            uav_textures: BTreeMap::new(),
            uavs_atomic: BTreeSet::new(),
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
    fn hs_resource_bindings_use_group_3_with_compute_visibility() {
        // Hull/domain/geometry shaders are bound via the executor's `stage_ex` buckets and are
        // executed through compute-based emulation. Their resources must live in @group(3) and be
        // visible to the compute stage.
        let module = Sm4Module {
            stage: ShaderStage::Hull,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![Sm4Decl::ConstantBuffer {
                slot: 0,
                reg_count: 1,
            }],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: dummy_dst(),
                    src: crate::sm4_ir::SrcOperand {
                        kind: SrcKind::ConstantBuffer { slot: 0, reg: 0 },
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let bindings = reflect_resource_bindings(&module).unwrap();
        let cbuf = bindings
            .into_iter()
            .find(|b| {
                matches!(
                    b.kind,
                    BindingKind::ConstantBuffer {
                        slot: 0,
                        reg_count: 1
                    }
                )
            })
            .expect("expected cbuffer binding");

        assert_eq!(cbuf.group, 3);
        assert_eq!(cbuf.binding, BINDING_BASE_CBUFFER);
        assert_eq!(cbuf.visibility, wgpu::ShaderStages::COMPUTE);

        // Ensure WGSL declaration emission uses the same group mapping (prevents collisions with
        // VS/PS resources and internal emulation groups).
        let resources = scan_resources(&module, None).expect("scan resources");
        let mut w = WgslWriter::new();
        resources
            .emit_decls(&mut w, ShaderStage::Hull)
            .expect("emit decls");
        let decls = w.finish();
        assert!(
            decls.contains("@group(3) @binding(0) var<uniform> cb0: Cb0;"),
            "expected HS cbuffer decl in group 3, got:\n{decls}"
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

    fn sig_param(
        semantic_name: &str,
        semantic_index: u32,
        register: u32,
    ) -> DxbcSignatureParameter {
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
    fn texture2d_array_sampling_uses_rdef_dimension_and_coord_z_as_slice() {
        use aero_dxbc::rdef::RdefResourceBinding;

        let coord_reg = crate::sm4_ir::SrcOperand {
            kind: SrcKind::Register(RegisterRef {
                file: RegFile::Temp,
                index: 1,
            }),
            swizzle: Swizzle::XYZW,
            modifier: OperandModifier::None,
        };

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
            instructions: vec![
                // Populate r1 so the generated WGSL has a stable `(r1).z` slice expression.
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 1,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: dummy_coord(),
                },
                Sm4Inst::SampleL {
                    dst: dummy_dst(),
                    coord: coord_reg,
                    texture: crate::sm4_ir::TextureRef { slot: 0 },
                    sampler: crate::sm4_ir::SamplerRef { slot: 0 },
                    lod: dummy_coord(),
                },
                Sm4Inst::Ret,
            ],
        };

        let rdef = RdefChunk {
            target: 0,
            flags: 0,
            creator: None,
            constant_buffers: Vec::new(),
            bound_resources: vec![RdefResourceBinding {
                name: "t0".to_owned(),
                // D3D_SIT_TEXTURE
                input_type: 2,
                return_type: 0,
                // D3D_SRV_DIMENSION_TEXTURE2DARRAY
                dimension: 5,
                sample_count: 0,
                bind_point: 0,
                bind_count: 1,
                flags: 0,
            }],
        };

        let translated = translate_cs(&module, Some(rdef)).expect("translate");
        assert_wgsl_validates(&translated.wgsl);
        assert!(
            translated.wgsl.contains("texture_2d_array<f32>"),
            "{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("i32((r1).z)"),
            "{}",
            translated.wgsl
        );

        assert!(
            translated
                .reflection
                .bindings
                .iter()
                .any(|b| b.kind == BindingKind::Texture2DArray { slot: 0 }),
            "expected reflection to include BindingKind::Texture2DArray, got: {:?}",
            translated.reflection.bindings
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
            textures_2d: BTreeSet::new(),
            textures_2d_array: BTreeSet::new(),
            srv_buffers: BTreeSet::new(),
            samplers: BTreeSet::new(),
            uav_buffers: BTreeSet::new(),
            uav_textures: BTreeMap::new(),
            uavs_atomic: BTreeSet::new(),
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
    fn semantic_to_d3d_name_recognizes_compute_builtins() {
        assert_eq!(
            semantic_to_d3d_name("SV_DispatchThreadID"),
            Some(D3D_NAME_DISPATCH_THREAD_ID)
        );
        assert_eq!(
            semantic_to_d3d_name("sv_groupthreadid"),
            Some(D3D_NAME_GROUP_THREAD_ID)
        );
        assert_eq!(semantic_to_d3d_name("SV_GROUPID"), Some(D3D_NAME_GROUP_ID));
        assert_eq!(
            semantic_to_d3d_name("sv_groupindex"),
            Some(D3D_NAME_GROUP_INDEX)
        );
    }

    #[test]
    fn builtin_from_d3d_name_maps_compute_builtins() {
        assert_eq!(
            builtin_from_d3d_name(D3D_NAME_DISPATCH_THREAD_ID),
            Some(Builtin::GlobalInvocationId)
        );
        assert_eq!(
            builtin_from_d3d_name(D3D_NAME_GROUP_THREAD_ID),
            Some(Builtin::LocalInvocationId)
        );
        assert_eq!(
            builtin_from_d3d_name(D3D_NAME_GROUP_ID),
            Some(Builtin::WorkgroupId)
        );
        assert_eq!(
            builtin_from_d3d_name(D3D_NAME_GROUP_INDEX),
            Some(Builtin::LocalInvocationIndex)
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
    fn vertex_shader_uint_inputs_use_u32_wgsl_types_and_bitcast_to_internal_register_bits() {
        // `D3D_REGISTER_COMPONENT_UINT32` in signature tables.
        const D3D_REGISTER_COMPONENT_UINT32: u32 = 1;
        const D3D_REGISTER_COMPONENT_FLOAT32: u32 = 3;

        // Minimal VS: `mov o0, v0; ret`.
        let module = Sm4Module {
            stage: ShaderStage::Vertex,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                Sm4Inst::Mov {
                    dst: crate::sm4_ir::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
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

        let isgn = DxbcSignature {
            parameters: vec![DxbcSignatureParameter {
                semantic_name: "TEXCOORD".to_owned(),
                semantic_index: 0,
                system_value_type: 0,
                component_type: D3D_REGISTER_COMPONENT_UINT32,
                register: 0,
                mask: 0b0011, // xy
                read_write_mask: 0b1111,
                stream: 0,
                min_precision: 0,
            }],
        };

        let osgn = DxbcSignature {
            parameters: vec![DxbcSignatureParameter {
                semantic_name: "SV_Position".to_owned(),
                semantic_index: 0,
                system_value_type: 0,
                component_type: D3D_REGISTER_COMPONENT_FLOAT32,
                register: 0,
                mask: 0b1111,
                read_write_mask: 0b1111,
                stream: 0,
                min_precision: 0,
            }],
        };

        let translated = translate_vs(&module, &isgn, &osgn, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);

        // The vertex input struct should use integer types for UINT32 signature parameters.
        assert!(
            translated.wgsl.contains("a0: vec2<u32>"),
            "{}",
            translated.wgsl
        );

        // Reading the input register should preserve raw bits by bitcasting to f32.
        assert!(
            translated.wgsl.contains("bitcast<f32>(input.a0.x)"),
            "{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("bitcast<f32>(input.a0.y)"),
            "{}",
            translated.wgsl
        );
    }

    #[test]
    fn malformed_control_flow_endif_without_if_triggers_error() {
        let isgn = DxbcSignature {
            parameters: Vec::new(),
        };
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

    #[test]
    fn malformed_control_flow_continuec_without_loop_triggers_error() {
        let isgn = DxbcSignature {
            parameters: Vec::new(),
        };
        let osgn = DxbcSignature {
            parameters: vec![sig_param("SV_Target", 0, 0)],
        };

        let module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                Sm4Inst::ContinueC {
                    op: Sm4CmpOp::Eq,
                    a: dummy_coord(),
                    b: dummy_coord(),
                },
                Sm4Inst::Ret,
            ],
        };

        let err = translate_ps(&module, &isgn, &osgn, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::MalformedControlFlow { inst_index: 0, .. }
        ));
    }

    #[test]
    fn malformed_control_flow_breakc_without_loop_or_switch_triggers_error() {
        let isgn = DxbcSignature {
            parameters: Vec::new(),
        };
        let osgn = DxbcSignature {
            parameters: vec![sig_param("SV_Target", 0, 0)],
        };

        let module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                Sm4Inst::BreakC {
                    op: Sm4CmpOp::Eq,
                    a: dummy_coord(),
                    b: dummy_coord(),
                },
                Sm4Inst::Ret,
            ],
        };

        let err = translate_ps(&module, &isgn, &osgn, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::MalformedControlFlow { inst_index: 0, .. }
        ));
    }

    #[test]
    fn predicated_breakc_is_translated() {
        let isgn = DxbcSignature {
            parameters: Vec::new(),
        };
        let osgn = DxbcSignature {
            parameters: vec![sig_param("SV_Target", 0, 0)],
        };

        let module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                Sm4Inst::Loop,
                Sm4Inst::Predicated {
                    pred: PredicateOperand {
                        reg: PredicateRef { index: 0 },
                        component: 0,
                        invert: false,
                    },
                    inner: Box::new(Sm4Inst::BreakC {
                        op: Sm4CmpOp::Eq,
                        a: dummy_coord(),
                        b: dummy_coord(),
                    }),
                },
                Sm4Inst::EndLoop,
                Sm4Inst::Ret,
            ],
        };

        let translated = translate_ps(&module, &isgn, &osgn, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("loop {"), "{}", translated.wgsl);
        assert!(
            translated.wgsl.contains("if (p0.x &&"),
            "{}",
            translated.wgsl
        );
        assert!(translated.wgsl.contains("break;"), "{}", translated.wgsl);
    }

    #[test]
    fn predicated_continuec_is_translated() {
        let isgn = DxbcSignature {
            parameters: Vec::new(),
        };
        let osgn = DxbcSignature {
            parameters: vec![sig_param("SV_Target", 0, 0)],
        };

        let module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 4, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                Sm4Inst::Loop,
                Sm4Inst::Predicated {
                    pred: PredicateOperand {
                        reg: PredicateRef { index: 0 },
                        component: 0,
                        invert: false,
                    },
                    inner: Box::new(Sm4Inst::ContinueC {
                        op: Sm4CmpOp::Eq,
                        a: dummy_coord(),
                        b: dummy_coord(),
                    }),
                },
                Sm4Inst::EndLoop,
                Sm4Inst::Ret,
            ],
        };

        let translated = translate_ps(&module, &isgn, &osgn, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);
        assert!(translated.wgsl.contains("loop {"), "{}", translated.wgsl);
        assert!(
            translated.wgsl.contains("if (p0.x &&"),
            "{}",
            translated.wgsl
        );
        assert!(translated.wgsl.contains("continue;"), "{}", translated.wgsl);
    }

    #[test]
    fn uav_unsupported_format_triggers_error() {
        let module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![Sm4Decl::UavTyped2D {
                slot: 0,
                format: 9999,
            }],
            instructions: vec![Sm4Inst::StoreUavTyped {
                uav: crate::sm4_ir::UavRef { slot: 0 },
                coord: dummy_coord(),
                value: dummy_coord(),
                mask: WriteMask::XYZW,
            }],
        };

        let err = scan_resources(&module, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::UnsupportedUavTextureFormat {
                slot: 0,
                format: 9999
            }
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("unsupported") && msg.contains("DXGI format"),
            "expected actionable unsupported format message, got: {msg}"
        );
    }

    #[test]
    fn uav_slot_used_as_buffer_and_texture_triggers_error() {
        let module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![Sm4Decl::UavTyped2D {
                slot: 0,
                // DXGI_FORMAT_R8G8B8A8_UNORM
                format: 28,
            }],
            instructions: vec![
                Sm4Inst::StoreRaw {
                    uav: crate::sm4_ir::UavRef { slot: 0 },
                    addr: dummy_coord(),
                    value: dummy_coord(),
                    mask: WriteMask::X,
                },
                Sm4Inst::StoreUavTyped {
                    uav: crate::sm4_ir::UavRef { slot: 0 },
                    coord: dummy_coord(),
                    value: dummy_coord(),
                    mask: WriteMask::XYZW,
                },
            ],
        };

        let err = scan_resources(&module, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::UavSlotUsedAsBufferAndTexture { slot: 0 }
        ));
    }

    #[test]
    fn srv_slot_used_as_buffer_and_texture_triggers_error() {
        let module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                Sm4Inst::LdRaw {
                    dst: dummy_dst(),
                    addr: dummy_coord(),
                    buffer: crate::sm4_ir::BufferRef { slot: 0 },
                },
                Sm4Inst::Sample {
                    dst: dummy_dst(),
                    coord: dummy_coord(),
                    texture: crate::sm4_ir::TextureRef { slot: 0 },
                    sampler: crate::sm4_ir::SamplerRef { slot: 0 },
                },
            ],
        };

        let err = scan_resources(&module, None).unwrap_err();
        assert!(matches!(
            err,
            ShaderTranslateError::TextureSlotUsedAsBufferAndTexture { slot: 0 }
        ));
    }

    #[test]
    fn hs_control_point_count_emits_control_points_per_patch_constant() {
        let module = Sm4Module {
            stage: ShaderStage::Hull,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::InputControlPointCount { count: 3 },
                Sm4Decl::HsOutputControlPointCount { count: 3 },
            ],
            instructions: vec![Sm4Inst::Ret],
        };
        let isgn = DxbcSignature { parameters: vec![] };
        let osgn = DxbcSignature { parameters: vec![] };
        let pcsg = DxbcSignature { parameters: vec![] };

        let translated = translate_hs(&module, &isgn, &osgn, &pcsg, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);
        assert!(
            translated
                .wgsl
                .contains("const HS_INPUT_CONTROL_POINTS: u32 = 3u;"),
            "{}",
            translated.wgsl
        );
        assert!(
            translated
                .wgsl
                .contains("const HS_OUTPUT_CONTROL_POINTS: u32 = 3u;"),
            "{}",
            translated.wgsl
        );
        assert!(
            translated
                .wgsl
                .contains("const HS_CONTROL_POINTS_PER_PATCH: u32 = HS_OUTPUT_CONTROL_POINTS;"),
            "{}",
            translated.wgsl
        );
        assert!(
            translated
                .wgsl
                .contains("hs_primitive_id * HS_CONTROL_POINTS_PER_PATCH + hs_output_control_point_id"),
            "{}",
            translated.wgsl
        );
    }

    #[test]
    fn hs_control_point_count_falls_back_to_input_count_when_output_count_missing() {
        let module = Sm4Module {
            stage: ShaderStage::Hull,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![Sm4Decl::InputControlPointCount { count: 7 }],
            instructions: vec![Sm4Inst::Ret],
        };
        let isgn = DxbcSignature { parameters: vec![] };
        let osgn = DxbcSignature { parameters: vec![] };
        let pcsg = DxbcSignature { parameters: vec![] };

        let translated = translate_hs(&module, &isgn, &osgn, &pcsg, None).expect("translate");
        assert_wgsl_validates(&translated.wgsl);
        assert!(
            translated
                .wgsl
                .contains("const HS_INPUT_CONTROL_POINTS: u32 = 7u;"),
            "{}",
            translated.wgsl
        );
        assert!(
            translated
                .wgsl
                .contains("const HS_CONTROL_POINTS_PER_PATCH: u32 = HS_INPUT_CONTROL_POINTS;"),
            "{}",
            translated.wgsl
        );
    }

    #[test]
    fn hs_control_point_count_mismatch_is_rejected() {
        let module = Sm4Module {
            stage: ShaderStage::Hull,
            model: crate::sm4::ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::InputControlPointCount { count: 4 },
                Sm4Decl::HsOutputControlPointCount { count: 3 },
            ],
            instructions: vec![Sm4Inst::Ret],
        };
        let isgn = DxbcSignature { parameters: vec![] };
        let osgn = DxbcSignature { parameters: vec![] };
        let pcsg = DxbcSignature { parameters: vec![] };

        let err = translate_hs(&module, &isgn, &osgn, &pcsg, None).unwrap_err();
        match err {
            ShaderTranslateError::UnsupportedInstruction { inst_index, opcode } => {
                assert_eq!(inst_index, 0);
                assert!(opcode.contains("hs_input_control_points_4_output_control_points_3"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
