//! Minimal SM4/SM5 intermediate representation for DXBC → WGSL translation.
//!
//! This IR is intentionally small: it is only meant to cover the handful of
//! instruction/resource features required for FL10_0 bring-up. The decoder that
//! produces this IR lives elsewhere (see Task 454).

use crate::sm4::{ShaderModel, ShaderStage};

/// A decoded SM4/SM5 module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sm4Module {
    /// Shader stage declared by the DXBC version token.
    pub stage: ShaderStage,
    /// Shader model declared by the DXBC version token.
    pub model: ShaderModel,
    /// Non-executable declarations and metadata.
    ///
    /// This includes traditional SM4/SM5 declarations that typically appear before the
    /// instruction stream, as well as non-executable `customdata` blocks (comments, debug data,
    /// immediate constant buffers) which may legally appear both before and within the
    /// instruction stream.
    pub decls: Vec<Sm4Decl>,
    /// Linear instruction stream in execution order.
    pub instructions: Vec<Sm4Inst>,
}

/// Boolean test mode used by structured control-flow instructions (e.g. `if`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sm4TestBool {
    /// Execute the block when the condition is exactly zero (`if_z`).
    Zero,
    /// Execute the block when the condition is non-zero (`if_nz`).
    NonZero,
}

/// Geometry-shader input primitive type.
///
/// This corresponds to `dcl_inputprimitive` in SM4/SM5 assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GsInputPrimitive {
    /// Point input primitive.
    ///
    /// The payload value is preserved so downstream stages can distinguish tokenized enums from
    /// D3D primitive-topology constants when necessary.
    Point(u32),
    /// Line (line list) input primitive.
    Line(u32),
    /// Triangle input primitive.
    ///
    /// Some toolchains encode this as `3` (tokenized enum) or `4` (D3D topology constant for
    /// triangle list). Both map to this variant with the original token value preserved.
    Triangle(u32),
    /// Line adjacency input primitive.
    ///
    /// Some toolchains encode this as `6` (tokenized enum) or `10`/`11` (D3D topology constants for
    /// line list/strip adjacency). The original token value is preserved.
    LineAdjacency(u32),
    /// Triangle adjacency input primitive.
    ///
    /// Some toolchains encode this as `7` (tokenized enum) or `12`/`13` (D3D topology constants for
    /// triangle list/strip adjacency). The original token value is preserved.
    TriangleAdjacency(u32),
    /// Unknown/unsupported input primitive encoding.
    Unknown(u32),
}

/// Geometry-shader output topology.
///
/// This corresponds to `dcl_outputtopology` in SM4/SM5 assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GsOutputTopology {
    /// Point list output topology.
    Point(u32),
    /// Line strip output topology.
    LineStrip(u32),
    /// Triangle strip output topology.
    ///
    /// Some toolchains encode this as `3` (tokenized enum) or `5` (D3D topology constant for
    /// triangle strip). The original token value is preserved.
    TriangleStrip(u32),
    /// Unknown/unsupported output topology encoding.
    Unknown(u32),
}

/// A single SM4/SM5 declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sm4Decl {
    Input {
        reg: u32,
        mask: WriteMask,
    },
    InputSiv {
        reg: u32,
        mask: WriteMask,
        sys_value: u32,
    },
    Output {
        reg: u32,
        mask: WriteMask,
    },
    OutputSiv {
        reg: u32,
        mask: WriteMask,
        sys_value: u32,
    },
    /// Geometry-shader input primitive.
    GsInputPrimitive {
        primitive: GsInputPrimitive,
    },
    /// Geometry-shader output topology.
    GsOutputTopology {
        topology: GsOutputTopology,
    },
    /// Geometry-shader maximum number of vertices that can be emitted per invocation.
    GsMaxOutputVertexCount {
        max: u32,
    },
    /// Geometry-shader instance count.
    ///
    /// This declaration is optional; if omitted, the instance count is implicitly 1.
    GsInstanceCount {
        count: u32,
    },
    ConstantBuffer {
        slot: u32,
        reg_count: u32,
    },
    Sampler {
        slot: u32,
    },
    ResourceTexture2D {
        slot: u32,
    },
    /// `t#` buffer SRV declaration (raw or structured).
    ///
    /// `stride` is in bytes and is meaningful for [`BufferKind::Structured`]. For
    /// raw buffers it is typically 0.
    ResourceBuffer {
        slot: u32,
        stride: u32,
        kind: BufferKind,
    },
    /// `u#` buffer UAV declaration (raw or structured).
    ///
    /// `stride` is in bytes and is meaningful for [`BufferKind::Structured`]. For
    /// raw buffers it is typically 0.
    UavBuffer {
        slot: u32,
        stride: u32,
        kind: BufferKind,
    },
    /// `dcl_uav_typed u#, <dxgi_format>`
    ///
    /// Only a subset of DXGI formats are currently supported by the WGSL backend; the raw DXGI
    /// format value is preserved here so the translator can report actionable errors.
    UavTyped2D {
        slot: u32,
        /// DXGI_FORMAT value.
        format: u32,
    },
    /// Hull shader tessellator domain (e.g. `tri`, `quad`, `isoline`).
    ///
    /// Encoded by `dcl_tessellator_domain` in SM5 token streams.
    HsDomain {
        domain: HsDomain,
    },
    /// Hull shader tessellator partitioning mode.
    ///
    /// Encoded by `dcl_tessellator_partitioning` in SM5 token streams.
    HsPartitioning {
        partitioning: HsPartitioning,
    },
    /// Hull shader tessellator output topology (e.g. `triangle_cw`).
    ///
    /// Encoded by `dcl_tessellator_output_primitive` / `dcl_outputtopology` in SM5 token streams.
    HsOutputTopology {
        topology: HsOutputTopology,
    },
    /// Hull shader output control point count (`outputcontrolpoints(N)`).
    ///
    /// Encoded by `dcl_output_control_point_count` / `dcl_outputcontrolpoints` in SM5 token streams.
    HsOutputControlPointCount {
        count: u32,
    },
    /// Hull shader max tess factor (`dcl_hs_max_tessfactor`).
    ///
    /// Stored as raw IEEE-754 `f32` bits.
    HsMaxTessFactor {
        factor: u32,
    },
    /// Domain shader tessellator domain (`dcl_ds_domain`).
    DsDomain {
        domain: HsDomain,
    },
    /// Compute shader thread group size (`dcl_thread_group x, y, z`).
    ///
    /// WGSL requires this information to emit `@workgroup_size(x, y, z)` on the
    /// compute entry point.
    ThreadGroupSize {
        x: u32,
        y: u32,
        z: u32,
    },
    /// Hull/domain shader input patch control point count (`dcl_inputcontrolpoints N`).
    ///
    /// This declares the number of control points per *input* patch consumed by the shader:
    /// - For HS, it must match the IA patchlist primitive topology (`PatchListN`).
    /// - For DS, it is expected to match the HS output control point count.
    InputControlPointCount {
        count: u32,
    },
    /// Non-executable `customdata` block.
    ///
    /// This is emitted by the SM4/SM5 encoder for comments, debug data, immediate constant
    /// buffers, etc. The decoder currently treats all custom data blocks as non-executable.
    CustomData {
        class: u32,
        /// Total block length in DWORDs (including opcode + class DWORDs).
        len_dwords: u32,
    },
    /// Embedded immediate constant buffer (`customdata` class 3).
    ///
    /// FXC uses this to embed `dcl_immediateConstantBuffer { ... }` data into the token stream.
    /// The payload is stored as raw DWORDs (typically 4 DWORDs per constant register).
    ImmediateConstantBuffer {
        /// Payload DWORDs after the customdata class token.
        dwords: Vec<u32>,
    },
    /// Hull shader phase marker (control-point/fork/join).
    ///
    /// Hull shaders contain multiple "phases" in a single token stream. Phase marker opcodes
    /// (e.g. `hs_control_point_phase`) are non-executable and are used to delimit which
    /// instructions belong to the control-point vs patch-constant paths.
    ///
    /// `inst_index` refers to the index in [`Sm4Module::instructions`] of the first instruction
    /// that belongs to this phase.
    HsPhase {
        phase: HullShaderPhase,
        inst_index: usize,
    },
    Unknown {
        opcode: u32,
    },
}

/// Tessellation domain (tri/quad/isoline) used by hull/domain shaders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HsDomain {
    Tri,
    Quad,
    Isoline,
}

/// Hull shader tessellation partitioning mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HsPartitioning {
    Integer,
    Pow2,
    FractionalOdd,
    FractionalEven,
}

/// Hull shader output topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HsOutputTopology {
    Point,
    Line,
    TriangleCw,
    TriangleCcw,
}

/// Hull shader phase marker emitted in the instruction stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HullShaderPhase {
    ControlPoint,
    Fork,
    Join,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferKind {
    Raw,
    Structured,
}

/// Compute-shader system values exposed as special SM5 operand types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComputeBuiltin {
    /// `SV_DispatchThreadID` (`@builtin(global_invocation_id)`).
    DispatchThreadId,
    /// `SV_GroupThreadID` (`@builtin(local_invocation_id)`).
    GroupThreadId,
    /// `SV_GroupID` (`@builtin(workgroup_id)`).
    GroupId,
    /// `SV_GroupIndex` (`@builtin(local_invocation_index)`).
    GroupIndex,
}

/// A single SM4/SM5 instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sm4Inst {
    /// Predicated instruction.
    ///
    /// Encodes SM4/SM5 instruction-level predication (e.g. `(+p0.x) mov ...`).
    Predicated {
        pred: PredicateOperand,
        inner: Box<Sm4Inst>,
    },
    /// Structured `if` (`if_z` / `if_nz`).
    ///
    /// The source operand is scalar; the current IR still models everything as a `vec4`, so it is
    /// typically represented via a replicated swizzle (e.g. `.xxxx`).
    If {
        cond: SrcOperand,
        test: Sm4TestBool,
    },
    /// Compare-based structured `if` (`ifc_*`).
    ///
    /// `a` and `b` are scalar operands (but still carried as `vec4` in our untyped register
    /// model).
    IfC {
        op: Sm4CmpOp,
        a: SrcOperand,
        b: SrcOperand,
    },
    Else,
    EndIf,
    /// Begin a `loop` block.
    Loop,
    /// `endloop` token for the innermost open `loop`.
    EndLoop,
    Mov {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `movc dst, cond, a, b` (per-component conditional select).
    Movc {
        dst: DstOperand,
        cond: SrcOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// `setp p#, a, b, <cmp>`
    ///
    /// Writes a predicate register based on the result of a comparison.
    Setp {
        dst: PredicateDstOperand,
        op: Sm4CmpOp,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// `utof dest, src`
    ///
    /// Converts the unsigned integer bit pattern in `src` into a float numeric value.
    ///
    /// Note: DXBC registers are untyped. In our WGSL backend the register file is modeled as
    /// `vec4<f32>`, so the input to this instruction is expected to be carried as raw integer bits
    /// inside an `f32` lane (e.g. produced by `bitcast<f32>(...)`).
    Utof {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `itof dest, src`
    ///
    /// Converts the signed integer bit pattern in `src` into a float numeric value.
    Itof {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `ftoi dest, src`
    ///
    /// Converts a float numeric value into a signed integer and writes the resulting integer bit
    /// pattern into the untyped register file.
    Ftoi {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `ftou dest, src`
    ///
    /// Converts a float numeric value into an unsigned integer and writes the resulting integer
    /// bit pattern into the untyped register file.
    Ftou {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// Bitwise AND (`and dest, a, b`).
    ///
    /// SM4/SM5 register files are untyped; integer values are typically represented as raw bits in
    /// the same registers that also carry float values. The translator therefore preserves the raw
    /// bit patterns by bitcasting to integer vectors in WGSL, performing the `&`, then bitcasting
    /// back into the internal `vec4<f32>` register model.
    And {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Signed 32-bit integer multiply.
    IMul {
        dst_lo: DstOperand,
        dst_hi: Option<DstOperand>,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Unsigned 32-bit integer multiply.
    UMul {
        dst_lo: DstOperand,
        dst_hi: Option<DstOperand>,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Signed 32-bit integer multiply-add (`a * b + c`) with wrap-around semantics.
    IMad {
        dst_lo: DstOperand,
        dst_hi: Option<DstOperand>,
        a: SrcOperand,
        b: SrcOperand,
        c: SrcOperand,
    },
    /// Unsigned 32-bit integer multiply-add (`a * b + c`) with wrap-around semantics.
    UMad {
        dst_lo: DstOperand,
        dst_hi: Option<DstOperand>,
        a: SrcOperand,
        b: SrcOperand,
        c: SrcOperand,
    },
    Add {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Signed integer add with carry.
    ///
    /// DXBC encodes integer values as raw 32-bit bits in the untyped register file. The
    /// translator models temporaries/outputs as `vec4<f32>`, so integer math is performed by
    /// bitcasting source lanes to `u32`, then writing the resulting bits back via `bitcast<f32>`.
    IAddC {
        dst_sum: DstOperand,
        dst_carry: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Unsigned integer add with carry.
    UAddC {
        dst_sum: DstOperand,
        dst_carry: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Signed integer subtract with borrow.
    ISubC {
        dst_diff: DstOperand,
        /// Carry/no-borrow flag output.
        ///
        /// Note: despite the "sub" mnemonic, D3D's `isubc` instruction exposes a carry-style flag
        /// (1 when `a >= b`, 0 when a borrow occurred).
        dst_carry: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Unsigned integer subtract with borrow.
    USubB {
        dst_diff: DstOperand,
        dst_borrow: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Mul {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Mad {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
        c: SrcOperand,
    },
    Dp3 {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Dp4 {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Min {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Max {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Integer add (wrap-around).
    IAdd {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Integer subtract (wrap-around).
    ISub {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Bitwise OR.
    Or {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Bitwise XOR.
    Xor {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Bitwise NOT.
    Not {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// Shift left (`a << b`).
    IShl {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Arithmetic shift right (`a >> b` in `i32` space).
    IShr {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Logical shift right (`a >> b` in `u32` space).
    UShr {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Component-wise comparison.
    ///
    /// DXBC comparisons produce *predicate masks* (`0xffffffff` for true, `0x00000000` for false)
    /// in the destination register, not `1.0`/`0.0`.
    ///
    /// The operand interpretation is selected via [`CmpType`], but the result is always a
    /// predicate mask stored into the untyped register file (which this IR models as
    /// `vec4<f32>` bits).
    Cmp {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
        op: CmpOp,
        ty: CmpType,
    },
    /// Signed integer minimum: `imin dst, a, b`
    IMin {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Signed integer maximum: `imax dst, a, b`
    IMax {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Unsigned integer minimum: `umin dst, a, b`
    UMin {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Unsigned integer maximum: `umax dst, a, b`
    UMax {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Signed integer absolute value: `iabs dst, src`
    IAbs {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// Signed integer negation: `ineg dst, src`
    INeg {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// Unsigned integer division.
    ///
    /// DXBC encodes `udiv` with two destination operands:
    /// - `dst_quot`: quotient
    /// - `dst_rem`: remainder
    ///
    /// Both sources and destinations are represented as raw 32-bit register bits; the WGSL
    /// backend bitcasts through `u32`/`i32` and writes results back as raw bits.
    UDiv {
        dst_quot: DstOperand,
        dst_rem: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Signed integer division (truncating toward zero) with a remainder.
    ///
    /// Like [`Sm4Inst::UDiv`], `idiv` has two destination operands.
    IDiv {
        dst_quot: DstOperand,
        dst_rem: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Rcp {
        dst: DstOperand,
        src: SrcOperand,
    },
    Rsq {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `bfi dst, width, offset, insert, base`
    ///
    /// Inserts `width` bits from `insert` into `base` starting at bit `offset`.
    Bfi {
        dst: DstOperand,
        width: SrcOperand,
        offset: SrcOperand,
        insert: SrcOperand,
        base: SrcOperand,
    },
    /// `ubfe dst, width, offset, src`
    ///
    /// Extracts `width` bits from `src` starting at bit `offset`, zero-extending the result.
    Ubfe {
        dst: DstOperand,
        width: SrcOperand,
        offset: SrcOperand,
        src: SrcOperand,
    },
    /// `ibfe dst, width, offset, src`
    ///
    /// Extracts `width` bits from `src` starting at bit `offset`, sign-extending the result.
    Ibfe {
        dst: DstOperand,
        width: SrcOperand,
        offset: SrcOperand,
        src: SrcOperand,
    },
    /// `bfrev dest, src` (bit reverse).
    Bfrev {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `countbits dest, src` (population count).
    CountBits {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `firstbit_hi dest, src` (find MSB set, unsigned).
    FirstbitHi {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `firstbit_lo dest, src` (find LSB set, unsigned).
    FirstbitLo {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `firstbit_shi dest, src` (find MSB differing from sign bit, signed).
    FirstbitShi {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `discard_z` / `discard_nz` (pixel shader only).
    ///
    /// The condition is evaluated by testing the *raw 32-bit value* of the first lane
    /// (`.x`) against zero, matching how DXBC treats untyped register contents.
    Discard {
        cond: SrcOperand,
        test: Sm4TestBool,
    },
    /// `clip` (pixel shader only).
    ///
    /// Discards the pixel if **any** component of `src` is `< 0.0`.
    Clip {
        src: SrcOperand,
    },
    /// `f32tof16 dest, src`
    ///
    /// Converts each `f32` component into an IEEE 754 binary16 bit-pattern stored in the low 16
    /// bits of the destination lane.
    ///
    /// Note: The DXBC register file is untyped; downstream translation stores these integer bits in
    /// our `vec4<f32>` register model via bitcasts.
    F32ToF16 {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `f16tof32 dest, src`
    ///
    /// Converts each IEEE 754 binary16 bit-pattern stored in the low 16 bits of the source lane
    /// into a numeric `f32`.
    F16ToF32 {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `sample dest, coord, t#, s#`
    Sample {
        dst: DstOperand,
        coord: SrcOperand,
        texture: TextureRef,
        sampler: SamplerRef,
    },
    /// `sample_l dest, coord, t#, s#, lod`
    SampleL {
        dst: DstOperand,
        coord: SrcOperand,
        texture: TextureRef,
        sampler: SamplerRef,
        lod: SrcOperand,
    },
    /// `resinfo dst, mip_level, t#` (e.g. `Texture2D.GetDimensions`).
    ///
    /// Output packing (for `Texture2D`):
    /// - `dst.x` = width in texels
    /// - `dst.y` = height in texels
    /// - `dst.z` = depth (always 1 for 2D textures)
    /// - `dst.w` = total mip level count
    ///
    /// Note: `mip_level` is integer-typed in SM4/SM5.
    ResInfo {
        dst: DstOperand,
        mip_level: SrcOperand,
        texture: TextureRef,
    },
    /// `ld dest, coord, t#` (e.g. `Texture2D.Load`).
    ///
    /// Note: `coord` and `lod` are integer-typed in SM4/SM5.
    ///
    /// DXBC register files are untyped; in the WGSL backend we model the register file as
    /// `vec4<f32>` where each lane is treated as an untyped 32-bit payload.
    ///
    /// When emitting WGSL `textureLoad`, the translator must recover integer texel coordinates and
    /// an integer mip level from those untyped lanes. In practice DXBC shaders may provide these
    /// values either as:
    /// - raw integer bit patterns (common for compiler-generated DXBC), or
    /// - numeric float values that happen to be exact integers (common in hand-authored /
    ///   test-generated DXBC token streams).
    ///
    /// The translator therefore uses a small heuristic: when a lane looks like an exact integer
    /// float (finite, in range, and with zero fractional part), it uses numeric `i32(f32)`
    /// conversion; otherwise it falls back to interpreting the lane as raw integer bits via
    /// `bitcast<i32>(f32)`.
    Ld {
        dst: DstOperand,
        /// Texel coordinate (x/y in `.xy`).
        coord: SrcOperand,
        texture: TextureRef,
        /// Mip level. For common `Texture2D.Load(int3(x,y,mip))` forms this is derived
        /// from the third component of `coord`.
        lod: SrcOperand,
    },
    /// `ld_raw dst, addr, t#`
    ///
    /// `addr` is a byte offset into the raw buffer.
    LdRaw {
        dst: DstOperand,
        addr: SrcOperand,
        buffer: BufferRef,
    },
    /// `ld_uav_raw dest, addr, u#` (SM5 UAV raw buffer load; e.g. `RWByteAddressBuffer.Load*`).
    ///
    /// `addr` is a byte address; the instruction returns 1–4 consecutive `u32` words starting at
    /// `addr / 4` and writes them into the destination register's `.xyzw` lanes.
    LdUavRaw {
        dst: DstOperand,
        /// Byte address.
        addr: SrcOperand,
        uav: UavRef,
    },
    /// `store_raw u#, addr, value` (mask comes from the `u#` operand write mask).
    StoreRaw {
        uav: UavRef,
        addr: SrcOperand,
        value: SrcOperand,
        mask: WriteMask,
    },
    /// `ld_structured dst, index, offset, t#`
    ///
    /// `index` is the structured element index and `offset` is the byte offset
    /// within the element. Stride comes from the corresponding declaration.
    LdStructured {
        dst: DstOperand,
        index: SrcOperand,
        offset: SrcOperand,
        buffer: BufferRef,
    },
    /// `ld_structured dst, index, offset, u#` (structured UAV buffer load; `RWStructuredBuffer.Load`).
    ///
    /// Like [`Sm4Inst::LdStructured`], but reads from an unordered access view (`u#`). Stride comes
    /// from the corresponding UAV declaration.
    LdStructuredUav {
        dst: DstOperand,
        index: SrcOperand,
        offset: SrcOperand,
        uav: UavRef,
    },
    /// `store_structured u#, index, offset, value`
    ///
    /// `index` is the structured element index and `offset` is the byte offset
    /// within the element. Stride comes from the corresponding declaration.
    StoreStructured {
        uav: UavRef,
        index: SrcOperand,
        offset: SrcOperand,
        value: SrcOperand,
        /// Write mask from the `u#` operand (x/y/z/w).
        mask: WriteMask,
    },
    /// `store_uav_typed u#, coord, value`
    StoreUavTyped {
        uav: UavRef,
        coord: SrcOperand,
        value: SrcOperand,
        mask: WriteMask,
    },
    /// SM5 `sync` (barrier / memory fence) instruction.
    ///
    /// DXBC encodes a set of `D3D11_SB_SYNC_FLAGS` in the opcode token's "opcode-specific control"
    /// field. This is used to represent HLSL intrinsics such as:
    /// - `GroupMemoryBarrierWithGroupSync()`
    /// - `DeviceMemoryBarrierWithGroupSync()`
    /// - `AllMemoryBarrierWithGroupSync()`
    /// - `DeviceMemoryBarrier()` / `AllMemoryBarrier()` (fence-only; no group sync)
    ///
    /// The translator interprets `flags` using the `SYNC_FLAG_*` constants in
    /// [`crate::sm4::opcode`].
    Sync {
        flags: u32,
    },
    /// Atomic add on a UAV buffer word address (SM5 `InterlockedAdd` family).
    ///
    /// This models the subset needed for `RWByteAddressBuffer` / `RWStructuredBuffer<uint>`
    /// patterns where the UAV is treated as an array of 32-bit words.
    ///
    /// If `dst` is `None`, the returned original value is discarded (DXBC typically encodes this
    /// via a `null` destination operand).
    AtomicAdd {
        dst: Option<DstOperand>,
        uav: UavRef,
        addr: SrcOperand,
        value: SrcOperand,
    },
    /// Geometry-shader `emit` / `emit_stream`.
    ///
    /// `stream` selects the output stream; for plain `emit`, the decoder should use `stream = 0`.
    Emit {
        stream: u8,
    },
    /// Geometry-shader `cut` / `cut_stream`.
    ///
    /// `stream` selects the output stream; for plain `cut`, the decoder should use `stream = 0`.
    Cut {
        stream: u8,
    },
    /// Geometry-shader `emit_then_cut` / `emit_then_cut_stream`.
    ///
    /// This is a compact encoding equivalent to `emit` followed by `cut` on the same stream.
    ///
    /// `stream` selects the output stream; for plain `emit_then_cut`, the decoder should use
    /// `stream = 0`.
    EmitThenCut {
        stream: u8,
    },
    /// A decoded instruction that the IR producer does not model yet.
    ///
    /// This allows the WGSL backend to fail with a precise opcode + instruction
    /// index, instead of the decoder having to reject the entire shader up
    /// front.
    Unknown {
        opcode: u32,
    },
    /// Structured `switch` statement.
    ///
    /// The SM4/SM5 token stream encodes structured control flow as a linear stream of opcodes
    /// (`switch`, `case`, `default`, `endswitch`) that is later reconstructed into nested blocks by
    /// the backend.
    Switch {
        /// Switch selector value (expected to be scalar).
        selector: SrcOperand,
    },
    /// Case label inside a `switch`.
    ///
    /// The operand is encoded as an immediate 32-bit integer value.
    Case {
        /// Raw 32-bit case value.
        value: u32,
    },
    /// Default label inside a `switch`.
    Default,
    /// End of structured `switch`.
    EndSwitch,
    /// Structured `break` instruction (break out of `loop`/`switch`).
    Break,
    /// Structured `breakc` instruction (conditional `break` with an embedded comparison operator).
    BreakC {
        op: Sm4CmpOp,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// Structured `continue` instruction (continue the innermost `loop`).
    Continue,
    /// Structured `continuec` instruction (conditional `continue` with an embedded comparison operator).
    ContinueC {
        op: Sm4CmpOp,
        a: SrcOperand,
        b: SrcOperand,
    },
    /// `bufinfo` on a raw SRV buffer (e.g. `ByteAddressBuffer.GetDimensions`).
    ///
    /// Output packing:
    /// - `dst.x` = total buffer size in **bytes**.
    /// - other lanes are 0 (but still respect `dst.mask`).
    BufInfoRaw {
        dst: DstOperand,
        buffer: BufferRef,
    },
    /// `bufinfo` on a structured SRV buffer (e.g. `StructuredBuffer.GetDimensions`).
    ///
    /// Output packing:
    /// - `dst.x` = element count
    /// - `dst.y` = stride in bytes
    /// - other lanes are 0 (but still respect `dst.mask`).
    BufInfoStructured {
        dst: DstOperand,
        buffer: BufferRef,
        stride_bytes: u32,
    },
    /// `bufinfo` on a raw UAV buffer (e.g. `RWByteAddressBuffer.GetDimensions`).
    ///
    /// Output packing:
    /// - `dst.x` = total buffer size in **bytes**.
    /// - other lanes are 0 (but still respect `dst.mask`).
    BufInfoRawUav {
        dst: DstOperand,
        uav: UavRef,
    },
    /// `bufinfo` on a structured UAV buffer (e.g. `RWStructuredBuffer.GetDimensions`).
    ///
    /// Output packing:
    /// - `dst.x` = element count
    /// - `dst.y` = stride in bytes
    /// - other lanes are 0 (but still respect `dst.mask`).
    BufInfoStructuredUav {
        dst: DstOperand,
        uav: UavRef,
        stride_bytes: u32,
    },
    Ret,
}

impl GsInputPrimitive {
    /// Decodes a `dcl_inputprimitive` payload value.
    pub fn from_token(value: u32) -> Self {
        // In the tokenized SM4 format this is documented as `D3D10_SB_PRIMITIVE` (see
        // `d3d10tokenizedprogramformat.h`), but in practice some toolchains appear to emit
        // `D3D10_PRIMITIVE_TOPOLOGY` values instead (e.g. triangle list as 4 instead of 3).
        //
        // To avoid decoding failures across fixtures, accept both encodings when unambiguous.
        match value {
            1 => Self::Point(value),
            2 => Self::Line(value),
            3 | 4 => Self::Triangle(value),
            6 | 10 | 11 => Self::LineAdjacency(value),
            7 | 12 | 13 => Self::TriangleAdjacency(value),
            other => Self::Unknown(other),
        }
    }
}

impl GsOutputTopology {
    /// Decodes a `dcl_outputtopology` payload value.
    pub fn from_token(value: u32) -> Self {
        // In the tokenized SM4 format this is documented as `D3D10_SB_PRIMITIVE_TOPOLOGY` (see
        // `d3d10tokenizedprogramformat.h`). Some toolchains appear to use
        // `D3D10_PRIMITIVE_TOPOLOGY` values instead (e.g. trianglestrip=5).
        //
        // The only topology currently required by our GS compute-emulation path is triangle strip,
        // so we accept both common encodings.
        match value {
            1 => Self::Point(value),
            2 => Self::LineStrip(value),
            3 | 5 => Self::TriangleStrip(value),
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpType {
    /// Compare operands as IEEE 754 floats (`f32`).
    F32,
    I32,
    U32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegFile {
    Temp,
    Input,
    Output,
    /// Pixel shader depth output (`oDepth` / `SV_Depth`).
    ///
    /// DXBC uses a distinct operand type (`D3D10_SB_OPERAND_TYPE_OUTPUT_DEPTH`) for depth output
    /// which does not carry an `o#` index in the instruction stream. We preserve it as a distinct
    /// register file in the IR so the WGSL backend can map it to the correct `SV_Depth` output
    /// register declared by the signature.
    OutputDepth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegisterRef {
    pub file: RegFile,
    pub index: u32,
}

/// Predicate register reference (`p#`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PredicateRef {
    pub index: u32,
}

/// Scalar predicate operand (used for instruction-level predication).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredicateOperand {
    pub reg: PredicateRef,
    /// Component selector (0=x, 1=y, 2=z, 3=w).
    pub component: u8,
    /// Invert the predicate value.
    pub invert: bool,
}

/// Predicate destination operand (used by `setp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredicateDstOperand {
    pub reg: PredicateRef,
    pub mask: WriteMask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sm4CmpOp {
    Eq,
    Ne,
    Lt,
    Ge,
    Le,
    Gt,
    /// Unordered floating-point compare variants (`*_U` in the D3D10/11 tokenized program format).
    ///
    /// In `d3d10tokenizedprogramformat.h` / `d3d11tokenizedprogramformat.h` this corresponds to
    /// `D3D10_SB_INSTRUCTION_COMPARISON` / `D3D11_SB_INSTRUCTION_COMPARISON` values like
    /// `D3D10_SB_COMPARISON_EQ_U`. The `_U` suffix means **unordered** (NaN-aware) float
    /// comparisons: the result is true if either operand is NaN.
    ///
    /// Unsigned integer comparisons are encoded separately in DXBC (e.g. `ult`/`uge` opcodes) and
    /// are not represented by this enum.
    EqU,
    NeU,
    LtU,
    GeU,
    LeU,
    GtU,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteMask(pub u8);

impl WriteMask {
    pub const XYZW: Self = Self(0b1111);
    pub const X: Self = Self(0b0001);
    pub const Y: Self = Self(0b0010);
    pub const Z: Self = Self(0b0100);
    pub const W: Self = Self(0b1000);

    pub fn contains(self, component: u8) -> bool {
        (self.0 & component) != 0
    }
}

/// 4-component swizzle.
///
/// Each lane is 0..=3 for x/y/z/w.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swizzle(pub [u8; 4]);

impl Swizzle {
    pub const XYZW: Self = Self([0, 1, 2, 3]);
    pub const XXXX: Self = Self([0, 0, 0, 0]);
    pub const YYYY: Self = Self([1, 1, 1, 1]);
    pub const ZZZZ: Self = Self([2, 2, 2, 2]);
    pub const WWWW: Self = Self([3, 3, 3, 3]);

    pub fn is_identity(self) -> bool {
        self == Self::XYZW
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandModifier {
    None,
    Neg,
    Abs,
    AbsNeg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstOperand {
    pub reg: RegisterRef,
    pub mask: WriteMask,
    pub saturate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrcOperand {
    pub kind: SrcKind,
    pub swizzle: Swizzle,
    pub modifier: OperandModifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrcKind {
    Register(RegisterRef),
    /// Geometry-shader per-vertex input operand (`v#[]`), indexed by vertex within the input
    /// primitive (e.g. `v0[2]`).
    ///
    /// In DXBC this is encoded as an `OPERAND_TYPE_INPUT` operand with
    /// `OPERAND_INDEX_DIMENSION_2D`:
    /// - The first index selects the input register (`v#`).
    /// - The second index selects the vertex within the input primitive.
    GsInput {
        reg: u32,
        vertex: u32,
    },
    ConstantBuffer {
        slot: u32,
        reg: u32,
    },
    /// Immediate 32-bit floats (IEEE bits).
    ImmediateF32([u32; 4]),
    ComputeBuiltin(ComputeBuiltin),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureRef {
    pub slot: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerRef {
    pub slot: u32,
}

/// A `t#` shader resource bound as a buffer (e.g. `ByteAddressBuffer` / SRV buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferRef {
    pub slot: u32,
}

/// A `u#` unordered access view (UAV) bound to the shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UavRef {
    pub slot: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gs_ir_nodes_are_debug_and_eq_roundtrippable() {
        let decls = [
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Unknown(5),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::Unknown(2),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 42 },
            Sm4Decl::GsInstanceCount { count: 3 },
        ];
        for decl in decls {
            let cloned = decl.clone();
            assert_eq!(decl, cloned);
            assert_eq!(format!("{decl:?}"), format!("{cloned:?}"));
        }

        let insts = [
            Sm4Inst::Emit { stream: 0 },
            Sm4Inst::Cut { stream: 0 },
            Sm4Inst::Emit { stream: 1 },
            Sm4Inst::Cut { stream: 2 },
        ];
        for inst in insts {
            let cloned = inst.clone();
            assert_eq!(inst, cloned);
            assert_eq!(format!("{inst:?}"), format!("{cloned:?}"));
        }

        let op = SrcOperand {
            kind: SrcKind::GsInput { reg: 2, vertex: 1 },
            swizzle: Swizzle::XYZW,
            modifier: OperandModifier::None,
        };
        let cloned = op.clone();
        assert_eq!(op, cloned);
        assert_eq!(format!("{op:?}"), format!("{cloned:?}"));
    }
}
