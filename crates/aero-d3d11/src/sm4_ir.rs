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
    /// Geometry shader input primitive type (e.g. point/line/triangle).
    ///
    /// Encoded by `dcl_inputprimitive` in SM4/SM5 token streams.
    GsInputPrimitive {
        primitive: u32,
    },
    /// Geometry shader output topology (e.g. point/line/triangle_strip).
    ///
    /// Encoded by `dcl_outputtopology` in SM4/SM5 token streams.
    GsOutputTopology {
        topology: u32,
    },
    /// Geometry shader maximum number of vertices that can be emitted.
    ///
    /// Encoded by `dcl_maxout` / `dcl_maxvertexcount` in SM4/SM5 token streams.
    GsMaxOutputVertexCount {
        max: u32,
    },
    /// Geometry shader instance count (SM5: `[instance(n)]` / `dcl_gsinstancecount`).
    ///
    /// The corresponding `SV_GSInstanceID` declaration is represented via
    /// [`Sm4Decl::InputSiv`], since it uses the same operand+sysvalue form as
    /// other input system-value declarations.
    GsInstanceCount {
        count: u32,
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
    /// Compute shader thread group size (`dcl_thread_group x, y, z`).
    ///
    /// WGSL requires this information to emit `@workgroup_size(x, y, z)` on the
    /// compute entry point.
    ThreadGroupSize {
        x: u32,
        y: u32,
        z: u32,
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
    Unknown {
        opcode: u32,
    },
}

/// Hull shader tessellation domain.
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
    TriangleCw,
    TriangleCcw,
    Line,
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
    /// Structured `if` (`if_z` / `if_nz`).
    ///
    /// The source operand is scalar; the current IR still models everything as a `vec4`, so it is
    /// typically represented via a replicated swizzle (e.g. `.xxxx`).
    If {
        cond: SrcOperand,
        test: Sm4TestBool,
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
    /// Integer multiply (wrap-around).
    IMul {
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
    Itof {
        dst: DstOperand,
        src: SrcOperand,
    },
    Ftoi {
        dst: DstOperand,
        src: SrcOperand,
    },
    Ftou {
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
    /// Integer compare (`ieq/ine/ilt/ige/ult/uge`).
    ///
    /// Produces a per-component predicate mask: `0xffffffff` for true, `0x00000000` for false.
    /// The result is stored in the untyped register file, which the current IR models as
    /// `vec4<f32>` (bit patterns preserved).
    Cmp {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
        op: CmpOp,
        ty: CmpType,
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
    /// `ld dest, coord, t#` (e.g. `Texture2D.Load`).
    ///
    /// Note: `coord` and `lod` are integer-typed in SM4/SM5.
    ///
    /// DXBC register files are untyped; in the WGSL backend we model the register file as
    /// `vec4<f32>` where each lane is treated as an untyped 32-bit payload.
    ///
    /// When emitting WGSL `textureLoad`, the translator interprets the source lanes strictly as
    /// integer bits (i.e. `bitcast<i32>(f32)`) to recover an `i32` coordinate/LOD.
    ///
    /// Any numeric float→int conversion must be performed explicitly by the DXBC instruction
    /// stream (e.g. via the `ftoi`/`round` family), not inferred heuristically during translation.
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
    /// Workgroup barrier + thread-group synchronization (`sync_*_t` in SM5).
    ///
    /// This corresponds to HLSL intrinsics such as `GroupMemoryBarrierWithGroupSync()` and
    /// `DeviceMemoryBarrierWithGroupSync()`.
    WorkgroupBarrier,
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
    /// A decoded instruction that the IR producer does not model yet.
    ///
    /// This allows the WGSL backend to fail with a precise opcode + instruction
    /// index, instead of the decoder having to reject the entire shader up
    /// front.
    Unknown {
        opcode: u32,
    },
    /// Geometry shader `emit` / `emit_stream`.
    ///
    /// `stream` is always 0 for `emit`, and 0..=3 for `emit_stream`.
    Emit {
        stream: u32,
    },
    /// Geometry shader `cut` / `cut_stream`.
    ///
    /// `stream` is always 0 for `cut`, and 0..=3 for `cut_stream`.
    Cut {
        stream: u32,
    },
    /// Geometry shader `emit_then_cut` / `emit_then_cut_stream`.
    ///
    /// This is a compact encoding equivalent to `emit` followed by `cut` on the same stream.
    ///
    /// `stream` is always 0 for `emit_then_cut`, and 0..=3 for `emit_then_cut_stream`.
    EmitThenCut {
        stream: u32,
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
    /// Geometry shader per-vertex input operand (`v#[]`), e.g. `v0[2]`.
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
            Sm4Decl::GsInputPrimitive { primitive: 5 },
            Sm4Decl::GsOutputTopology { topology: 2 },
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
