// AeroGPU command stream layouts.
//
// Source of truth: `drivers/aerogpu/protocol/aerogpu_cmd.h`.
//
// Keep this file in lockstep with the C header above; ABI is validated by:
// - `cargo test --locked -p aero-protocol`
// - `npm run test:protocol`

import { AEROGPU_ABI_VERSION_U32, parseAndValidateAbiVersionU32 } from "./aerogpu_pci.ts";

const UTF8 = Object.freeze({ encoding: "utf-8" as const });
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder(UTF8.encoding);

function coerceString(input: unknown): string {
  try {
    return String(input ?? "");
  } catch {
    return "";
  }
}

function formatOneLineUtf8(input: unknown, maxBytes: number): string {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  if (maxBytes === 0) return "";

  const buf = new Uint8Array(maxBytes);
  let written = 0;
  let pendingSpace = false;
  for (const ch of coerceString(input)) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden =
      code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = written > 0;
      continue;
    }

    if (pendingSpace) {
      const spaceRes = textEncoder.encodeInto(" ", buf.subarray(written));
      if (spaceRes.written === 0) break;
      written += spaceRes.written;
      pendingSpace = false;
      if (written >= maxBytes) break;
    }

    const res = textEncoder.encodeInto(ch, buf.subarray(written));
    if (res.written === 0) break;
    written += res.written;
    if (written >= maxBytes) break;
  }
  return written === 0 ? "" : textDecoder.decode(buf.subarray(0, written));
}

function safeErrorMessageInput(err: unknown): string {
  if (err === null) return "null";
  switch (typeof err) {
    case "string":
      return err;
    case "number":
    case "boolean":
    case "bigint":
    case "symbol":
    case "undefined":
      return String(err);
    case "object": {
      try {
        const msg =
          err && typeof (err as { message?: unknown }).message === "string" ? (err as { message: string }).message : "";
        if (msg) return msg;
      } catch {
        // ignore hostile getters
      }
      break;
    }
    default:
      break;
  }
  return "Error";
}

function formatDecodeErrorCause(err: unknown, maxBytes = 512): string {
  const raw = safeErrorMessageInput(err);
  return formatOneLineUtf8(raw, maxBytes) || "Error";
}

export type AerogpuHandle = number;

export const AEROGPU_CMD_STREAM_MAGIC = 0x444d4341; // "ACMD" LE
export const AEROGPU_CMD_STREAM_FLAG_NONE = 0;

export const AerogpuCmdStreamFlags = {
  None: 0,
} as const;

export type AerogpuCmdStreamFlags = (typeof AerogpuCmdStreamFlags)[keyof typeof AerogpuCmdStreamFlags];

export const AEROGPU_CMD_STREAM_HEADER_SIZE = 24;
export const AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC = 0;
export const AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION = 4;
export const AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES = 8;
export const AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS = 12;

export interface AerogpuCmdStreamHeader {
  abiVersion: number;
  sizeBytes: number;
  flags: number;
}

export function decodeCmdStreamHeader(view: DataView, byteOffset = 0): AerogpuCmdStreamHeader {
  if (view.byteLength < byteOffset + AEROGPU_CMD_STREAM_HEADER_SIZE) {
    throw new Error("Buffer too small for aerogpu_cmd_stream_header");
  }

  const magic = view.getUint32(byteOffset + AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC, true);
  if (magic !== AEROGPU_CMD_STREAM_MAGIC) {
    throw new Error(`Bad command stream magic: 0x${magic.toString(16)}`);
  }

  const abiVersion = view.getUint32(byteOffset + AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION, true);
  parseAndValidateAbiVersionU32(abiVersion);

  const sizeBytes = view.getUint32(byteOffset + AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, true);
  if (sizeBytes < AEROGPU_CMD_STREAM_HEADER_SIZE) {
    throw new Error(`cmd_stream.size_bytes too small: ${sizeBytes}`);
  }
  if (sizeBytes % 4 !== 0) {
    throw new Error(`cmd_stream.size_bytes is not 4-byte aligned: ${sizeBytes}`);
  }

  return {
    abiVersion,
    sizeBytes,
    flags: view.getUint32(byteOffset + AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS, true),
  };
}

export const AEROGPU_CMD_HDR_SIZE = 8;
export const AEROGPU_CMD_HDR_OFF_OPCODE = 0;
export const AEROGPU_CMD_HDR_OFF_SIZE_BYTES = 4;

export interface AerogpuCmdHdr {
  opcode: number;
  sizeBytes: number;
}

export function decodeCmdHdr(view: DataView, byteOffset = 0): AerogpuCmdHdr {
  if (view.byteLength < byteOffset + AEROGPU_CMD_HDR_SIZE) {
    throw new Error("Buffer too small for aerogpu_cmd_hdr");
  }

  const opcode = view.getUint32(byteOffset + AEROGPU_CMD_HDR_OFF_OPCODE, true);
  const sizeBytes = view.getUint32(byteOffset + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
  if (sizeBytes < AEROGPU_CMD_HDR_SIZE) {
    throw new Error(`cmd.size_bytes too small: ${sizeBytes}`);
  }
  if (sizeBytes % 4 !== 0) {
    throw new Error(`cmd.size_bytes is not 4-byte aligned: ${sizeBytes}`);
  }

  return { opcode, sizeBytes };
}

export interface AerogpuCmdPacket {
  opcode: number;
  sizeBytes: number;
  /**
   * Packet payload bytes after `aerogpu_cmd_hdr` (including any trailing padding).
   *
   * Matches Rust `AerogpuCmdPacket.payload`.
   */
  payload: Uint8Array;
}

// Extended packet view returned by `AerogpuCmdStreamIter`.
export interface AerogpuCmdPacketView extends AerogpuCmdPacket {
  offsetBytes: number;
  endBytes: number;
  hdr: AerogpuCmdHdr;
}

export class AerogpuCmdStreamIter implements IterableIterator<AerogpuCmdPacketView> {
  readonly bytes: Uint8Array;
  readonly view: DataView;
  readonly header: AerogpuCmdStreamHeader;

  private cursor: number;
  private end: number;

  constructor(bytes: ArrayBuffer | Uint8Array) {
    this.bytes = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    this.view = new DataView(this.bytes.buffer, this.bytes.byteOffset, this.bytes.byteLength);

    this.header = decodeCmdStreamHeader(this.view, 0);
    this.end = this.header.sizeBytes >>> 0;

    if (this.end > this.view.byteLength) {
      throw new Error(
        `Buffer too small for aerogpu_cmd_stream: need ${this.end} bytes, have ${this.view.byteLength}`,
      );
    }

    this.cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;
  }

  [Symbol.iterator](): IterableIterator<AerogpuCmdPacketView> {
    return this;
  }

  next(): IteratorResult<AerogpuCmdPacketView> {
    if (this.cursor >= this.end) {
      return { done: true, value: undefined as unknown as AerogpuCmdPacketView };
    }

    if (this.end - this.cursor < AEROGPU_CMD_HDR_SIZE) {
      throw new Error(`truncated aerogpu_cmd_hdr at offset ${this.cursor}`);
    }

    let hdr: AerogpuCmdHdr;
    try {
      hdr = decodeCmdHdr(this.view, this.cursor);
    } catch (err) {
      const msg = formatDecodeErrorCause(err, 512);
      throw new Error(`invalid aerogpu_cmd_hdr at offset ${this.cursor}: ${msg}`);
    }

    const end = this.cursor + hdr.sizeBytes;
    if (end > this.end) {
      throw new Error(
        `aerogpu cmd packet at offset ${this.cursor} overruns stream (end=${end}, stream_size=${this.end})`,
      );
    }

    const payload = this.bytes.subarray(this.cursor + AEROGPU_CMD_HDR_SIZE, end);
    const packet: AerogpuCmdPacketView = {
      offsetBytes: this.cursor,
      endBytes: end,
      hdr,
      opcode: hdr.opcode,
      sizeBytes: hdr.sizeBytes,
      payload,
    };

    this.cursor = end;
    return { done: false, value: packet };
  }
}

export function* iterCmdStream(bytes: Uint8Array): Generator<AerogpuCmdPacket> {
  yield* new AerogpuCmdStreamIter(bytes);
}

export interface AerogpuCmdStreamView {
  header: AerogpuCmdStreamHeader;
  packets: AerogpuCmdPacketView[];
}

/**
 * Decode a command stream into an eagerly collected view (header + packet list).
 *
 * This mirrors Rust `AerogpuCmdStreamView::decode_from_le_bytes` and is useful for tests and tooling
 * where a one-shot parse is more convenient than manual iteration.
 */
export function decodeCmdStreamView(bytes: ArrayBuffer | Uint8Array): AerogpuCmdStreamView {
  const iter = new AerogpuCmdStreamIter(bytes);
  return { header: iter.header, packets: Array.from(iter) };
}

/**
 * Returns whether a command packet is a vsync-paced PRESENT/PRESENT_EX.
 *
 * This mirrors Rust `cmd_stream_has_vsync_present_*` helpers and is used by host runtimes to
 * implement the Win7 timing contract: vsync'd PRESENT fences must not complete before the next
 * vblank edge.
 *
 * Throws when a PRESENT packet is too small to contain the `{scanout_id, flags}` fields.
 */
export function cmdPacketHasVsyncPresent(packet: AerogpuCmdPacket): boolean {
  if (packet.opcode !== AerogpuCmdOpcode.Present && packet.opcode !== AerogpuCmdOpcode.PresentEx) {
    return false;
  }

  // `payload` starts after `aerogpu_cmd_hdr`. PRESENT payload begins with:
  //   u32 scanout_id; u32 flags;
  if (packet.payload.byteLength < 8) {
    throw new Error(
      `PRESENT packet too small to contain flags (need 8 bytes, have ${packet.payload.byteLength})`,
    );
  }
  const payload = packet.payload;
  // Read flags (little-endian) without constructing a DataView (hot path in some runtimes).
  const flags = (payload[4]! | (payload[5]! << 8) | (payload[6]! << 16) | (payload[7]! << 24)) >>> 0;
  return (flags & AEROGPU_PRESENT_FLAG_VSYNC) !== 0;
}

/**
 * Returns whether the command stream contains a vsync-paced PRESENT/PRESENT_EX.
 *
 * Prefer `cmdPacketHasVsyncPresent` when the caller already iterates the stream for other reasons.
 */
export function cmdStreamHasVsyncPresent(bytes: ArrayBuffer | Uint8Array): boolean {
  for (const packet of new AerogpuCmdStreamIter(bytes)) {
    if (cmdPacketHasVsyncPresent(packet)) return true;
  }
  return false;
}

export const AerogpuCmdOpcode = {
  Nop: 0,
  // Packet payload is UTF-8 bytes (no NUL terminator); padded to 4-byte alignment.
  DebugMarker: 1,

  CreateBuffer: 0x100,
  CreateTexture2d: 0x101,
  DestroyResource: 0x102,
  ResourceDirtyRange: 0x103,
  UploadResource: 0x104,
  CopyBuffer: 0x105,
  CopyTexture2d: 0x106,
  CreateTextureView: 0x107,
  DestroyTextureView: 0x108,

  CreateShaderDxbc: 0x200,
  DestroyShader: 0x201,
  BindShaders: 0x202,
  SetShaderConstantsF: 0x203,
  CreateInputLayout: 0x204,
  DestroyInputLayout: 0x205,
  SetInputLayout: 0x206,
  SetShaderConstantsI: 0x207,
  SetShaderConstantsB: 0x208,

  SetBlendState: 0x300,
  SetDepthStencilState: 0x301,
  SetRasterizerState: 0x302,

  SetRenderTargets: 0x400,
  SetViewport: 0x401,
  SetScissor: 0x402,

  SetVertexBuffers: 0x500,
  SetIndexBuffer: 0x501,
  SetPrimitiveTopology: 0x502,

  SetTexture: 0x510,
  SetSamplerState: 0x511,
  SetRenderState: 0x512,

  CreateSampler: 0x520,
  DestroySampler: 0x521,
  SetSamplers: 0x522,
  SetConstantBuffers: 0x523,
  SetShaderResourceBuffers: 0x524,
  SetUnorderedAccessBuffers: 0x525,

  Clear: 0x600,
  Draw: 0x601,
  DrawIndexed: 0x602,
  Dispatch: 0x603,

  Present: 0x700,
  PresentEx: 0x701,

  ExportSharedSurface: 0x710,
  ImportSharedSurface: 0x711,
  ReleaseSharedSurface: 0x712,

  Flush: 0x720,
} as const;

export type AerogpuCmdOpcode = (typeof AerogpuCmdOpcode)[keyof typeof AerogpuCmdOpcode];
export const AerogpuShaderStage = {
  Vertex: 0,
  Pixel: 1,
  Compute: 2,
  Geometry: 3,
} as const;

export type AerogpuShaderStage = (typeof AerogpuShaderStage)[keyof typeof AerogpuShaderStage];

// Extended shader stage encoding (`stage_ex`).
//
// Some packets contain a `shader_stage` (or `stage`) field whose base enum supports VS/PS/CS (+ GS).
// To represent additional D3D10+ stages (HS/DS) without changing packet layouts, when
// `shader_stage == AerogpuShaderStage.Compute` the packet's `reserved0` field is repurposed as a
// `stage_ex` override. If `shader_stage != Compute`, `reserved0` MUST be 0 and is ignored.
//
// Canonical rules:
// - `reserved0 == 0` means "no stage_ex override" and MUST be interpreted as the legacy Compute stage
//   (older guests always wrote 0 into reserved fields).
// - Non-zero `reserved0` values are interpreted as `AerogpuShaderStageEx`.
//
// Note: Geometry is also representable directly via `shaderStage=Geometry` in the legacy stage enum;
// `stage_ex` is primarily needed for HS/DS (and as a compatibility encoding for GS).
//
// Numeric values intentionally match the D3D DXBC "program type" numbers used in the shader version token:
//   Pixel=0, Vertex=1, Geometry=2, Hull=3, Domain=4, Compute=5.
//
// Because `reserved0 == 0` is reserved for "no override", `stage_ex` cannot encode Pixel (0). This is not a
// limitation in practice because Pixel/Vertex shaders are already expressible via `AerogpuShaderStage`.
//
// `AerogpuShaderStageEx.Compute` (5) is accepted by `resolveStageEx()` and treated the same as "no override"
// (Compute). Writers should emit 0 for Compute to preserve legacy packet semantics.
export const AerogpuShaderStageEx = {
  // 0 = no stage_ex override (legacy Compute).
  None: 0,
  // DXBC program type 1 is Vertex, but Vertex must be encoded via `shader_stage` (not stage_ex).
  Geometry: 2,
  Hull: 3,
  Domain: 4,
  Compute: 5,
} as const;

export type AerogpuShaderStageEx = (typeof AerogpuShaderStageEx)[keyof typeof AerogpuShaderStageEx];

export type AerogpuResolvedShaderStage = "vertex" | "pixel" | "geometry" | "hull" | "domain" | "compute";

/**
 * Decode the extended shader stage ("stage_ex") from a `(shaderStage, reserved0)` pair.
 *
 * The "stage_ex" ABI extension overloads the `reserved0` field of certain commands that already
 * include a legacy `shaderStage`/`stage` field (e.g. `SET_TEXTURE`, `SET_SAMPLERS`,
 * `SET_CONSTANT_BUFFERS`, `SET_SHADER_CONSTANTS_F`, `SET_SHADER_CONSTANTS_I`, `SET_SHADER_CONSTANTS_B`,
 * `SET_SHADER_RESOURCE_BUFFERS`, `SET_UNORDERED_ACCESS_BUFFERS`, `CREATE_SHADER_DXBC`).
 *
 * The overload is only active when `shaderStage == AEROGPU_SHADER_STAGE_COMPUTE`.
 */
export function decodeStageEx(shaderStage: number, reserved0: number): AerogpuShaderStageEx | undefined {
  if ((shaderStage >>> 0) !== AerogpuShaderStage.Compute) return undefined;
  switch (reserved0 >>> 0) {
    case AerogpuShaderStageEx.None:
      return AerogpuShaderStageEx.None;
    case AerogpuShaderStageEx.Geometry:
      return AerogpuShaderStageEx.Geometry;
    case AerogpuShaderStageEx.Hull:
      return AerogpuShaderStageEx.Hull;
    case AerogpuShaderStageEx.Domain:
      return AerogpuShaderStageEx.Domain;
    case AerogpuShaderStageEx.Compute:
      return AerogpuShaderStageEx.Compute;
    default:
      return undefined;
  }
}

/**
 * ABI minor version that introduced the `stage_ex` encoding in `reserved0`.
 *
 * Older guests (command stream ABI minor < this value) may not reliably zero `reserved0`, so
 * hosts/tooling must ignore it to avoid misinterpreting garbage as a stage selector.
 */
export const AEROGPU_STAGE_EX_MIN_ABI_MINOR = 3;

/**
 * Decode an extended shader stage encoded in a packet's `reserved0` field, gated by ABI minor.
 *
 * For command streams older than {@link AEROGPU_STAGE_EX_MIN_ABI_MINOR}, this always returns
 * {@link AerogpuShaderStageEx.None} when `shaderStage == Compute` (legacy behavior).
 */
export function decodeStageExGated(
  abiMinor: number,
  shaderStage: number,
  reserved0: number,
): AerogpuShaderStageEx | undefined {
  if ((abiMinor >>> 0) < AEROGPU_STAGE_EX_MIN_ABI_MINOR && (shaderStage >>> 0) === AerogpuShaderStage.Compute) {
    return decodeStageEx(shaderStage, 0);
  }
  return decodeStageEx(shaderStage, reserved0);
}

/**
 * Encode the extended shader stage ("stage_ex") into `(shaderStage, reserved0)`.
 */
export function encodeStageEx(stageEx: AerogpuShaderStageEx): [shaderStage: number, reserved0: number] {
  return [AerogpuShaderStage.Compute, normalizeStageExForWrite(AerogpuShaderStage.Compute, stageEx)];
}

export function encodeStageExReserved0(
  shaderStage: AerogpuShaderStage,
  stageEx?: AerogpuShaderStageEx | null,
): number {
  // `stageEx == 0` is ambiguous: it could mean either "no override" (the ABI meaning of reserved0=0)
  // or the DXBC Pixel program-type (also 0). To avoid accidentally encoding Pixel via stage_ex,
  // the writer API treats passing `stageEx=0` explicitly as an error; callers should omit the
  // optional parameter (or pass null/undefined) to request the legacy encoding.
  if (stageEx !== null && stageEx !== undefined && (stageEx >>> 0) === AerogpuShaderStageEx.None) {
    throw new Error("stageEx=0 is reserved for legacy/no-override; omit stageEx instead (Pixel cannot be encoded via stage_ex)");
  }
  const ex = stageEx === null || stageEx === undefined ? AerogpuShaderStageEx.None : stageEx;
  return normalizeStageExForWrite(shaderStage, ex);
}

export function resolveStageEx(shaderStage: number, reserved0: number): AerogpuResolvedShaderStage {
  switch (shaderStage) {
    case AerogpuShaderStage.Vertex:
      return "vertex";
    case AerogpuShaderStage.Pixel:
      return "pixel";
    case AerogpuShaderStage.Geometry:
      return "geometry";
    case AerogpuShaderStage.Compute:
      break;
    default:
      throw new Error(`unknown shader_stage value ${shaderStage}`);
  }

  // shaderStage == Compute: apply stage_ex override rules.
  switch (reserved0) {
    case AerogpuShaderStageEx.None:
    case AerogpuShaderStageEx.Compute:
      return "compute";
    case AerogpuShaderStageEx.Geometry:
      return "geometry";
    case AerogpuShaderStageEx.Hull:
      return "hull";
    case AerogpuShaderStageEx.Domain:
      return "domain";
    case 1:
      // DXBC program type 1 is Vertex, but Vertex must be encoded via AerogpuShaderStage for clarity.
      throw new Error(`invalid stage_ex value ${reserved0} (Pixel/Vertex must be encoded via shader_stage)`);
    default:
      throw new Error(`unknown stage_ex value ${reserved0}`);
  }
}

function normalizeStageExForWrite(shaderStage: AerogpuShaderStage, stageEx: AerogpuShaderStageEx): number {
  if (shaderStage !== AerogpuShaderStage.Compute) {
    if (stageEx !== AerogpuShaderStageEx.None) {
      throw new Error(`stage_ex is only valid when shader_stage==COMPUTE (got shader_stage=${shaderStage}, stage_ex=${stageEx})`);
    }
    return 0;
  }
  const resolved = resolveStageEx(shaderStage, stageEx);
  // Canonicalize Compute to legacy encoding (`reserved0==0`).
  if (resolved === "compute") {
    return 0;
  }
  return stageEx;
}

export type AerogpuShaderStageResolved =
  | { kind: "Vertex" }
  | { kind: "Pixel" }
  | { kind: "Compute" }
  | { kind: "Geometry" }
  | { kind: "Hull" }
  | { kind: "Domain" }
  | { kind: "Unknown"; shaderStage: number; stageEx: number };

/**
 * Resolve the effective shader stage from a legacy `(shaderStage, reserved0)` pair.
 *
 * Encoding rules:
 * - If `shaderStage != Compute`, then `reserved0` is reserved/ignored.
 * - If `shaderStage == Compute` and `reserved0 == 0`, then this is legacy Compute.
 * - If `shaderStage == Compute` and `reserved0 != 0`, then `reserved0` is a `stage_ex` discriminator.
 */
export function resolveShaderStageWithEx(shaderStage: number, reserved0: number): AerogpuShaderStageResolved {
  switch (shaderStage >>> 0) {
    case AerogpuShaderStage.Vertex:
      return { kind: "Vertex" };
    case AerogpuShaderStage.Pixel:
      return { kind: "Pixel" };
    case AerogpuShaderStage.Geometry:
      return { kind: "Geometry" };
    case AerogpuShaderStage.Compute: {
      if ((reserved0 >>> 0) === 0) return { kind: "Compute" };
      switch (reserved0 >>> 0) {
        case AerogpuShaderStageEx.Geometry:
          return { kind: "Geometry" };
        case AerogpuShaderStageEx.Hull:
          return { kind: "Hull" };
        case AerogpuShaderStageEx.Domain:
          return { kind: "Domain" };
        case AerogpuShaderStageEx.Compute:
          return { kind: "Compute" };
        case AerogpuShaderStageEx.None:
          return { kind: "Compute" };
        default:
          return { kind: "Unknown", shaderStage: shaderStage >>> 0, stageEx: reserved0 >>> 0 };
      }
    }
    default:
      return { kind: "Unknown", shaderStage: shaderStage >>> 0, stageEx: reserved0 >>> 0 };
  }
}

/**
 * Resolve the effective shader stage from `(shaderStage, reserved0)`, gated by command stream ABI minor.
 *
 * For command streams older than {@link AEROGPU_STAGE_EX_MIN_ABI_MINOR}, `reserved0` is ignored when
 * `shaderStage == Compute` to preserve legacy behavior.
 */
export function resolveShaderStageWithExGated(
  abiMinor: number,
  shaderStage: number,
  reserved0: number,
): AerogpuShaderStageResolved {
  const minorU32 = abiMinor >>> 0;
  const stageU32 = shaderStage >>> 0;
  const reserved0U32 = reserved0 >>> 0;
  if (minorU32 < AEROGPU_STAGE_EX_MIN_ABI_MINOR && stageU32 === AerogpuShaderStage.Compute) {
    return resolveShaderStageWithEx(shaderStage, 0);
  }
  return resolveShaderStageWithEx(shaderStage, reserved0U32);
}

/**
 * Decode a legacy `(shaderStage, reserved0)` pair into a single extended stage enum.
 *
 * Encoding rules (mirrors `drivers/aerogpu/protocol/aerogpu_cmd.h`):
 * - If `shaderStage` is Vertex, Pixel, or Geometry, `reserved0` MUST be 0.
 * - If `shaderStage` is Compute:
 *   - `reserved0 == 0` means legacy compute stage (no stage_ex specified).
 *   - `reserved0 != 0` is interpreted as `AerogpuShaderStageEx` (DXBC program type ID).
 *
 * Returns `null` if the pair violates the encoding rules.
 *
 * Note: `AerogpuShaderStageEx` cannot represent Pixel/Vertex; those must be represented via the
 * legacy `AerogpuShaderStage` enum. Use `resolveShaderStageWithEx` if you need a representation
 * that includes Pixel/Vertex.
 */
export function decodeShaderStageEx(shaderStage: number, reserved0: number): AerogpuShaderStageEx | null {
  if (shaderStage === AerogpuShaderStage.Vertex) return null;
  if (shaderStage === AerogpuShaderStage.Pixel) return null;
  if (shaderStage === AerogpuShaderStage.Geometry) return reserved0 === 0 ? AerogpuShaderStageEx.Geometry : null;
  if (shaderStage === AerogpuShaderStage.Compute) {
    if (reserved0 === 0) return AerogpuShaderStageEx.Compute;
    switch (reserved0 >>> 0) {
      case AerogpuShaderStageEx.Geometry:
      case AerogpuShaderStageEx.Hull:
      case AerogpuShaderStageEx.Domain:
      case AerogpuShaderStageEx.Compute:
        return reserved0 as AerogpuShaderStageEx;
      default:
        return null;
    }
  }
  return null;
}
export const AerogpuIndexFormat = {
  Uint16: 0,
  Uint32: 1,
} as const;

export type AerogpuIndexFormat = (typeof AerogpuIndexFormat)[keyof typeof AerogpuIndexFormat];

export const AerogpuSamplerFilter = {
  Nearest: 0,
  Linear: 1,
} as const;

export type AerogpuSamplerFilter = (typeof AerogpuSamplerFilter)[keyof typeof AerogpuSamplerFilter];

export const AerogpuSamplerAddressMode = {
  ClampToEdge: 0,
  Repeat: 1,
  MirrorRepeat: 2,
} as const;

export type AerogpuSamplerAddressMode =
  (typeof AerogpuSamplerAddressMode)[keyof typeof AerogpuSamplerAddressMode];

export const AerogpuBlendFactor = {
  Zero: 0,
  One: 1,
  SrcAlpha: 2,
  InvSrcAlpha: 3,
  DestAlpha: 4,
  InvDestAlpha: 5,
  Constant: 6,
  InvConstant: 7,
} as const;

export type AerogpuBlendFactor = (typeof AerogpuBlendFactor)[keyof typeof AerogpuBlendFactor];

// `SET_BLEND_STATE` grew over time. Older guests may still send the legacy 28-byte packet:
//   hdr (8) + enable/src/dst/op (16) + color_write_mask+padding (4).
// Decoders should accept both layouts and default missing fields.
export const AEROGPU_CMD_SET_BLEND_STATE_SIZE_MIN = 28;

export interface AerogpuCmdSetBlendStateDecoded {
  enable: boolean;
  srcFactor: number;
  dstFactor: number;
  blendOp: number;
  colorWriteMask: number;
  srcFactorAlpha: number;
  dstFactorAlpha: number;
  blendOpAlpha: number;
  blendConstantRgba: [number, number, number, number];
  sampleMask: number;
}

export function decodeCmdSetBlendState(view: DataView, cmdByteOffset = 0): AerogpuCmdSetBlendStateDecoded {
  const hdr = decodeCmdHdr(view, cmdByteOffset);
  const end = cmdByteOffset + hdr.sizeBytes;
  if (end > view.byteLength) {
    throw new Error(`SET_BLEND_STATE packet overruns buffer (end=${end}, buffer_len=${view.byteLength})`);
  }
  if (hdr.sizeBytes < AEROGPU_CMD_SET_BLEND_STATE_SIZE_MIN) {
    throw new Error(`SET_BLEND_STATE packet too small (size_bytes=${hdr.sizeBytes})`);
  }

  const srcFactor = view.getUint32(cmdByteOffset + 12, true);
  const dstFactor = view.getUint32(cmdByteOffset + 16, true);
  const blendOp = view.getUint32(cmdByteOffset + 20, true);

  const srcFactorAlpha = hdr.sizeBytes >= 32 ? view.getUint32(cmdByteOffset + 28, true) : srcFactor;
  const dstFactorAlpha = hdr.sizeBytes >= 36 ? view.getUint32(cmdByteOffset + 32, true) : dstFactor;
  const blendOpAlpha = hdr.sizeBytes >= 40 ? view.getUint32(cmdByteOffset + 36, true) : blendOp;

  const blendConstantRgba: [number, number, number, number] = [1, 1, 1, 1];
  for (let i = 0; i < 4; i++) {
    const off = cmdByteOffset + 40 + i * 4;
    const needed = off + 4 - cmdByteOffset;
    if (hdr.sizeBytes >= needed) {
      blendConstantRgba[i] = view.getFloat32(off, true);
    }
  }
  const sampleMask = hdr.sizeBytes >= AEROGPU_CMD_SET_BLEND_STATE_SIZE ? view.getUint32(cmdByteOffset + 56, true) : 0xffffffff;

  return {
    enable: view.getUint32(cmdByteOffset + 8, true) !== 0,
    srcFactor,
    dstFactor,
    blendOp,
    colorWriteMask: view.getUint8(cmdByteOffset + 24),
    srcFactorAlpha,
    dstFactorAlpha,
    blendOpAlpha,
    blendConstantRgba,
    sampleMask,
  };
}

export const AerogpuBlendOp = {
  Add: 0,
  Subtract: 1,
  RevSubtract: 2,
  Min: 3,
  Max: 4,
} as const;

export type AerogpuBlendOp = (typeof AerogpuBlendOp)[keyof typeof AerogpuBlendOp];

export const AerogpuCompareFunc = {
  Never: 0,
  Less: 1,
  Equal: 2,
  LessEqual: 3,
  Greater: 4,
  NotEqual: 5,
  GreaterEqual: 6,
  Always: 7,
} as const;

export type AerogpuCompareFunc = (typeof AerogpuCompareFunc)[keyof typeof AerogpuCompareFunc];

export const AerogpuFillMode = {
  Solid: 0,
  Wireframe: 1,
} as const;

export type AerogpuFillMode = (typeof AerogpuFillMode)[keyof typeof AerogpuFillMode];

export const AerogpuCullMode = {
  None: 0,
  Front: 1,
  Back: 2,
} as const;

export type AerogpuCullMode = (typeof AerogpuCullMode)[keyof typeof AerogpuCullMode];

// AerogpuRasterizerState.flags bits.
//
// Default value 0 corresponds to D3D11 defaults:
// - DepthClipEnable = TRUE
export const AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE = 1 << 0;
export const AerogpuPrimitiveTopology = {
  PointList: 1,
  LineList: 2,
  LineStrip: 3,
  TriangleList: 4,
  TriangleStrip: 5,
  TriangleFan: 6,

  LineListAdj: 10,
  LineStripAdj: 11,
  TriangleListAdj: 12,
  TriangleStripAdj: 13,

  PatchList1: 33,
  PatchList2: 34,
  PatchList3: 35,
  PatchList4: 36,
  PatchList5: 37,
  PatchList6: 38,
  PatchList7: 39,
  PatchList8: 40,
  PatchList9: 41,
  PatchList10: 42,
  PatchList11: 43,
  PatchList12: 44,
  PatchList13: 45,
  PatchList14: 46,
  PatchList15: 47,
  PatchList16: 48,
  PatchList17: 49,
  PatchList18: 50,
  PatchList19: 51,
  PatchList20: 52,
  PatchList21: 53,
  PatchList22: 54,
  PatchList23: 55,
  PatchList24: 56,
  PatchList25: 57,
  PatchList26: 58,
  PatchList27: 59,
  PatchList28: 60,
  PatchList29: 61,
  PatchList30: 62,
  PatchList31: 63,
  PatchList32: 64,
} as const;

export type AerogpuPrimitiveTopology =
  (typeof AerogpuPrimitiveTopology)[keyof typeof AerogpuPrimitiveTopology];

export const AEROGPU_RESOURCE_USAGE_NONE = 0;
export const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER = 1 << 0;
export const AEROGPU_RESOURCE_USAGE_INDEX_BUFFER = 1 << 1;
export const AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER = 1 << 2;
export const AEROGPU_RESOURCE_USAGE_TEXTURE = 1 << 3;
export const AEROGPU_RESOURCE_USAGE_RENDER_TARGET = 1 << 4;
export const AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL = 1 << 5;
export const AEROGPU_RESOURCE_USAGE_SCANOUT = 1 << 6;
export const AEROGPU_RESOURCE_USAGE_STORAGE = 1 << 7;

export const AEROGPU_COPY_FLAG_NONE = 0;
export const AEROGPU_COPY_FLAG_WRITEBACK_DST = 1 << 0;

export const AEROGPU_MAX_RENDER_TARGETS = 8;

export const AEROGPU_INPUT_LAYOUT_BLOB_MAGIC = 0x59414c49; // "ILAY" LE
export const AEROGPU_INPUT_LAYOUT_BLOB_VERSION = 1;

export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_MAGIC = 0;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_VERSION = 4;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_ELEMENT_COUNT = 8;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_RESERVED0 = 12;

// D3D10/11 semantics are case-insensitive; guest UMDs hash the semantic name after
// canonicalizing it to ASCII uppercase (FNV-1a 32-bit), so the host can match ILAY
// elements to the vertex shader input signature.
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_SEMANTIC_NAME_HASH = 0;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_SEMANTIC_INDEX = 4;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_DXGI_FORMAT = 8;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INPUT_SLOT = 12;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_ALIGNED_BYTE_OFFSET = 16;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INPUT_SLOT_CLASS = 20;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INSTANCE_DATA_STEP_RATE = 24;

export const AEROGPU_CLEAR_COLOR = 1 << 0;
export const AEROGPU_CLEAR_DEPTH = 1 << 1;
export const AEROGPU_CLEAR_STENCIL = 1 << 2;

export const AEROGPU_PRESENT_FLAG_NONE = 0;
export const AEROGPU_PRESENT_FLAG_VSYNC = 1 << 0;

// Packet/struct sizes (in bytes) from the C header for ABI conformance tests.
export const AEROGPU_CMD_CREATE_BUFFER_SIZE = 40;
export const AEROGPU_CMD_CREATE_TEXTURE2D_SIZE = 56;
export const AEROGPU_CMD_CREATE_TEXTURE_VIEW_SIZE = 44;
export const AEROGPU_CMD_DESTROY_RESOURCE_SIZE = 16;
export const AEROGPU_CMD_DESTROY_TEXTURE_VIEW_SIZE = 16;
export const AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE = 32;
// Payload: aerogpu_cmd_upload_resource + data[size_bytes] + 4-byte alignment padding.
export const AEROGPU_CMD_UPLOAD_RESOURCE_SIZE = 32;
export const AEROGPU_CMD_COPY_BUFFER_SIZE = 48;
export const AEROGPU_CMD_COPY_TEXTURE2D_SIZE = 64;
// Payload: aerogpu_cmd_create_shader_dxbc + dxbc_bytes[dxbc_size_bytes] + 4-byte alignment padding.
export const AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE = 24;
export const AEROGPU_CMD_DESTROY_SHADER_SIZE = 16;
export const AEROGPU_CMD_BIND_SHADERS_SIZE = 24;
// Extended BIND_SHADERS packet appends 3x u32 shader handles: {gs, hs, ds}.
export const AEROGPU_CMD_BIND_SHADERS_EX_SIZE = AEROGPU_CMD_BIND_SHADERS_SIZE + 12;
// Extended BIND_SHADERS payload size (excluding the 8-byte `aerogpu_cmd_hdr`).
export const AEROGPU_CMD_BIND_SHADERS_EX_PAYLOAD_SIZE = AEROGPU_CMD_BIND_SHADERS_EX_SIZE - AEROGPU_CMD_HDR_SIZE;
// Payload: aerogpu_cmd_set_shader_constants_f + float data[vec4_count * 4] + 4-byte alignment padding.
export const AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE = 24;
// Payload: aerogpu_cmd_set_shader_constants_i + int32 data[vec4_count * 4] + 4-byte alignment padding.
export const AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE = 24;
// Payload: aerogpu_cmd_set_shader_constants_b + uint32_t data[bool_count] + 4-byte alignment padding.
export const AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE = 24;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_SIZE = 16;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_SIZE = 28;
// Payload: aerogpu_cmd_create_input_layout + blob[blob_size_bytes] + 4-byte alignment padding.
export const AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE = 20;
export const AEROGPU_CMD_DESTROY_INPUT_LAYOUT_SIZE = 16;
export const AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE = 16;
export const AEROGPU_BLEND_STATE_SIZE = 52;
export const AEROGPU_CMD_SET_BLEND_STATE_SIZE = 60;
export const AEROGPU_DEPTH_STENCIL_STATE_SIZE = 20;
export const AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE = 28;
export const AEROGPU_RASTERIZER_STATE_SIZE = 24;
export const AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE = 32;
export const AEROGPU_CMD_SET_RENDER_TARGETS_SIZE = 48;
export const AEROGPU_CMD_SET_VIEWPORT_SIZE = 32;
export const AEROGPU_CMD_SET_SCISSOR_SIZE = 24;
export const AEROGPU_VERTEX_BUFFER_BINDING_SIZE = 16;
// Payload: aerogpu_cmd_set_vertex_buffers + aerogpu_vertex_buffer_binding[buffer_count].
export const AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE = 16;
export const AEROGPU_CMD_SET_INDEX_BUFFER_SIZE = 24;
export const AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE = 16;
export const AEROGPU_CMD_SET_TEXTURE_SIZE = 24;
export const AEROGPU_CMD_SET_SAMPLER_STATE_SIZE = 24;
export const AEROGPU_CMD_SET_RENDER_STATE_SIZE = 16;
export const AEROGPU_CMD_CREATE_SAMPLER_SIZE = 28;
export const AEROGPU_CMD_DESTROY_SAMPLER_SIZE = 16;
export const AEROGPU_CMD_SET_SAMPLERS_SIZE = 24;
export const AEROGPU_CONSTANT_BUFFER_BINDING_SIZE = 16;
export const AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE = 24;
export const AEROGPU_SHADER_RESOURCE_BUFFER_BINDING_SIZE = 16;
export const AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE = 24;
export const AEROGPU_UNORDERED_ACCESS_BUFFER_BINDING_SIZE = 16;
export const AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE = 24;
export const AEROGPU_CMD_CLEAR_SIZE = 36;
export const AEROGPU_CMD_DRAW_SIZE = 24;
export const AEROGPU_CMD_DRAW_INDEXED_SIZE = 28;
export const AEROGPU_CMD_DISPATCH_SIZE = 24;
export const AEROGPU_CMD_PRESENT_SIZE = 16;
export const AEROGPU_CMD_PRESENT_EX_SIZE = 24;
export const AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE = 24;
export const AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE = 24;
export const AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE = 24;
export const AEROGPU_CMD_FLUSH_SIZE = 16;
export interface AerogpuVertexBufferBinding {
  buffer: AerogpuHandle;
  strideBytes: number;
  offsetBytes: number;
}

export interface AerogpuConstantBufferBinding {
  buffer: AerogpuHandle;
  offsetBytes: number;
  sizeBytes: number;
}

export interface AerogpuShaderResourceBufferBinding {
  buffer: AerogpuHandle;
  offsetBytes: number;
  sizeBytes: number;
}

export interface AerogpuUnorderedAccessBufferBinding {
  buffer: AerogpuHandle;
  offsetBytes: number;
  sizeBytes: number;
  initialCount: number;
}

function isPowerOfTwo(v: number): boolean {
  if (!Number.isSafeInteger(v) || v <= 0) return false;
  let x = v;
  while (x % 2 === 0) x /= 2;
  return x === 1;
}

export function alignUp(v: number, a: number): number {
  if (!Number.isSafeInteger(v) || v < 0) {
    throw new Error(`alignUp: value must be a non-negative safe integer (got ${v})`);
  }
  if (!isPowerOfTwo(a)) {
    throw new Error(`alignUp: alignment must be a positive power-of-two safe integer (got ${a})`);
  }
  const rem = v % a;
  const aligned = rem === 0 ? v : v + (a - rem);
  if (!Number.isSafeInteger(aligned)) {
    throw new Error(`alignUp: result not a safe integer (v=${v}, a=${a})`);
  }
  return aligned;
}

function u64ToSafeNumber(v: bigint, label: string): number {
  const n = Number(v);
  if (!Number.isFinite(n) || !Number.isSafeInteger(n)) {
    throw new Error(`u64 out of JS safe integer range for ${label}: ${v.toString()}`);
  }
  return n;
}

function alignUp4U32(v: number): number {
  // `v` is expected to be a u32, so this stays within JS safe integers.
  return alignUp(v, 4);
}

function decodePacketFromBytes(bytes: Uint8Array, packetOffset: number): AerogpuCmdPacket {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const hdr = decodeCmdHdr(view, packetOffset);
  const packetEnd = packetOffset + hdr.sizeBytes;
  if (packetEnd > bytes.byteLength) {
    throw new Error("Buffer too small for command packet");
  }
  return {
    opcode: hdr.opcode,
    sizeBytes: hdr.sizeBytes,
    payload: bytes.subarray(packetOffset + AEROGPU_CMD_HDR_SIZE, packetEnd),
  };
}

function validatePacketPayloadLen(packet: AerogpuCmdPacket): void {
  if (!Number.isSafeInteger(packet.sizeBytes) || packet.sizeBytes < AEROGPU_CMD_HDR_SIZE) {
    throw new Error(`cmd.size_bytes too small: ${packet.sizeBytes}`);
  }
  if (packet.sizeBytes > 0xffff_ffff) {
    throw new Error(`cmd.size_bytes too large for u32: ${packet.sizeBytes}`);
  }
  if (packet.sizeBytes % 4 !== 0) {
    throw new Error(`cmd.size_bytes is not 4-byte aligned: ${packet.sizeBytes}`);
  }
  const expectedPayloadLen = packet.sizeBytes - AEROGPU_CMD_HDR_SIZE;
  if (packet.payload.byteLength !== expectedPayloadLen) {
    throw new Error(
      `cmd payload length mismatch: expected ${expectedPayloadLen}, got ${packet.payload.byteLength}`,
    );
  }
}

export interface AerogpuCmdDebugMarkerPayload {
  markerBytes: Uint8Array;
  marker: string;
}

export function decodeCmdDebugMarkerPayload(bytes: Uint8Array, packetOffset: number): AerogpuCmdDebugMarkerPayload {
  return decodeCmdDebugMarkerPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdDebugMarkerPayloadFromPacket(packet: AerogpuCmdPacket): AerogpuCmdDebugMarkerPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.DebugMarker) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected DEBUG_MARKER)`);
  }

  const payload = packet.payload;
  // Packet size is 4-byte aligned, so padding can only be 0-3 bytes.
  let trimmedLen = payload.byteLength;
  for (let i = 0; i < 3 && trimmedLen > 0 && payload[trimmedLen - 1] === 0; i++) {
    trimmedLen--;
  }
  const markerBytes = payload.subarray(0, trimmedLen);
  const marker = new TextDecoder("utf-8", { fatal: true }).decode(markerBytes);
  return { markerBytes, marker };
}

export interface AerogpuCmdCreateShaderDxbcPayload {
  shaderHandle: AerogpuHandle;
  stage: number;
  dxbcSizeBytes: number;
  reserved0: number;
  dxbcBytes: Uint8Array;
}

export function decodeCmdCreateShaderDxbcPayload(
  bytes: Uint8Array,
  packetOffset: number,
): AerogpuCmdCreateShaderDxbcPayload {
  return decodeCmdCreateShaderDxbcPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdCreateShaderDxbcPayloadFromPacket(packet: AerogpuCmdPacket): AerogpuCmdCreateShaderDxbcPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.CreateShaderDxbc) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected CREATE_SHADER_DXBC)`);
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for CREATE_SHADER_DXBC payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const shaderHandle = view.getUint32(0, true);
  const stage = view.getUint32(4, true);
  const dxbcSizeBytes = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  const expected = AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE + alignUp4U32(dxbcSizeBytes);
  // Forward-compat: treat this as a minimum size so packets can be extended by appending new fields.
  if (packet.sizeBytes < expected) {
    throw new Error(
      `CREATE_SHADER_DXBC payload size mismatch: expected at least ${expected}, got ${packet.sizeBytes}`,
    );
  }

  const dxbcStart = 16;
  const dxbcEnd = dxbcStart + dxbcSizeBytes;
  return {
    shaderHandle,
    stage,
    dxbcSizeBytes,
    reserved0,
    dxbcBytes: packet.payload.subarray(dxbcStart, dxbcEnd),
  };
}

export interface BindShadersEx {
  gs: AerogpuHandle;
  hs: AerogpuHandle;
  ds: AerogpuHandle;
}
export interface AerogpuCmdBindShadersPayload {
  vs: AerogpuHandle;
  ps: AerogpuHandle;
  cs: AerogpuHandle;
  /**
   * Reserved for ABI forward-compat.
   *
   * Legacy behavior (24-byte packet):
   * - If `sizeBytes == 24` and `reserved0 != 0`, `reserved0` is interpreted as the geometry shader
   *   handle (`gs`).
   *
   * Append-only extension (>= 36-byte packet):
   * - When {@link ex} is present, the trailing `{gs, hs, ds}` handles are authoritative and
   *   `reserved0` is reserved/ignored (emitters should set it to 0, but may also mirror `gs` here
   *   for best-effort support on hosts that only understand the 24-byte packet; if mirrored, it
   *   should match {@link ex}.gs).
   */
  reserved0: number;
  /**
   * Append-only BIND_SHADERS extension: when present, these trailing handles take precedence over
   * `reserved0`. Any additional trailing bytes are ignored for forward-compat.
   */
  ex?: BindShadersEx;
}

export function decodeCmdBindShadersPayload(
  bytes: Uint8Array,
  packetOffset = 0,
): AerogpuCmdBindShadersPayload {
  return decodeCmdBindShadersPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdBindShadersPayloadFromPacket(packet: AerogpuCmdPacket): AerogpuCmdBindShadersPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.BindShaders) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected BIND_SHADERS)`);
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for BIND_SHADERS payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const vs = view.getUint32(0, true);
  const ps = view.getUint32(4, true);
  const cs = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  // Extended BIND_SHADERS appends `{gs, hs, ds}` after the base 16-byte payload.
  // Forward-compat: ignore extra bytes beyond the known extension fields.
  if (packet.payload.byteLength >= AEROGPU_CMD_BIND_SHADERS_EX_PAYLOAD_SIZE) {
    return {
      vs,
      ps,
      cs,
      reserved0,
      ex: {
        gs: view.getUint32(16, true),
        hs: view.getUint32(20, true),
        ds: view.getUint32(24, true),
      },
    };
  }
  return { vs, ps, cs, reserved0 };
}

export interface AerogpuCmdCreateInputLayoutBlobPayload {
  inputLayoutHandle: AerogpuHandle;
  blobSizeBytes: number;
  reserved0: number;
  blobBytes: Uint8Array;
}

export function decodeCmdCreateInputLayoutBlob(
  bytes: Uint8Array,
  packetOffset: number,
): AerogpuCmdCreateInputLayoutBlobPayload {
  return decodeCmdCreateInputLayoutBlobFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdCreateInputLayoutBlobFromPacket(
  packet: AerogpuCmdPacket,
): AerogpuCmdCreateInputLayoutBlobPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.CreateInputLayout) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected CREATE_INPUT_LAYOUT)`);
  }
  if (packet.payload.byteLength < 12) {
    throw new Error("Buffer too small for CREATE_INPUT_LAYOUT payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const inputLayoutHandle = view.getUint32(0, true);
  const blobSizeBytes = view.getUint32(4, true);
  const reserved0 = view.getUint32(8, true);

  const expected = AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE + alignUp4U32(blobSizeBytes);
  // Forward-compat: treat this as a minimum size so packets can be extended by appending new fields.
  if (packet.sizeBytes < expected) {
    throw new Error(
      `CREATE_INPUT_LAYOUT payload size mismatch: expected at least ${expected}, got ${packet.sizeBytes}`,
    );
  }

  const blobStart = 12;
  const blobEnd = blobStart + blobSizeBytes;
  return {
    inputLayoutHandle,
    blobSizeBytes,
    reserved0,
    blobBytes: packet.payload.subarray(blobStart, blobEnd),
  };
}

export interface AerogpuCmdUploadResourcePayload {
  resourceHandle: AerogpuHandle;
  reserved0: number;
  offsetBytes: bigint;
  sizeBytes: bigint;
  dataBytes: Uint8Array;
}

export function decodeCmdUploadResourcePayload(bytes: Uint8Array, packetOffset: number): AerogpuCmdUploadResourcePayload {
  return decodeCmdUploadResourcePayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdUploadResourcePayloadFromPacket(packet: AerogpuCmdPacket): AerogpuCmdUploadResourcePayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.UploadResource) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected UPLOAD_RESOURCE)`);
  }
  if (packet.payload.byteLength < 24) {
    throw new Error("Buffer too small for UPLOAD_RESOURCE payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const resourceHandle = view.getUint32(0, true);
  const reserved0 = view.getUint32(4, true);
  const offsetBytes = view.getBigUint64(8, true);
  const sizeBytes = view.getBigUint64(16, true);

  const dataSize = u64ToSafeNumber(sizeBytes, "upload_resource.size_bytes");
  const expected = AEROGPU_CMD_UPLOAD_RESOURCE_SIZE + alignUp4U32(dataSize);
  // Forward-compat: treat this as a minimum size so packets can be extended by appending new fields.
  if (packet.sizeBytes < expected) {
    throw new Error(`UPLOAD_RESOURCE payload size mismatch: expected at least ${expected}, got ${packet.sizeBytes}`);
  }

  const dataStart = 24;
  const dataEnd = dataStart + dataSize;
  return {
    resourceHandle,
    reserved0,
    offsetBytes,
    sizeBytes,
    dataBytes: packet.payload.subarray(dataStart, dataEnd),
  };
}

export interface AerogpuCmdCopyBufferPayload {
  dstBuffer: AerogpuHandle;
  srcBuffer: AerogpuHandle;
  dstOffsetBytes: bigint;
  srcOffsetBytes: bigint;
  sizeBytes: bigint;
  flags: number;
  reserved0: number;
}

export function decodeCmdCopyBufferPayload(bytes: Uint8Array, packetOffset = 0): AerogpuCmdCopyBufferPayload {
  return decodeCmdCopyBufferPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdCopyBufferPayloadFromPacket(packet: AerogpuCmdPacket): AerogpuCmdCopyBufferPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.CopyBuffer) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected COPY_BUFFER)`);
  }
  const expectedPayloadLen = AEROGPU_CMD_COPY_BUFFER_SIZE - AEROGPU_CMD_HDR_SIZE;
  if (packet.payload.byteLength < expectedPayloadLen) {
    throw new Error("Buffer too small for COPY_BUFFER payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const dstBuffer = view.getUint32(0, true);
  const srcBuffer = view.getUint32(4, true);
  const dstOffsetBytes = view.getBigUint64(8, true);
  const srcOffsetBytes = view.getBigUint64(16, true);
  const sizeBytes = view.getBigUint64(24, true);
  const flags = view.getUint32(32, true);
  const reserved0 = view.getUint32(36, true);
  return { dstBuffer, srcBuffer, dstOffsetBytes, srcOffsetBytes, sizeBytes, flags, reserved0 };
}

export interface AerogpuCmdCopyTexture2dPayload {
  dstTexture: AerogpuHandle;
  srcTexture: AerogpuHandle;
  dstMipLevel: number;
  dstArrayLayer: number;
  srcMipLevel: number;
  srcArrayLayer: number;
  dstX: number;
  dstY: number;
  srcX: number;
  srcY: number;
  width: number;
  height: number;
  flags: number;
  reserved0: number;
}

export function decodeCmdCopyTexture2dPayload(bytes: Uint8Array, packetOffset = 0): AerogpuCmdCopyTexture2dPayload {
  return decodeCmdCopyTexture2dPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdCopyTexture2dPayloadFromPacket(packet: AerogpuCmdPacket): AerogpuCmdCopyTexture2dPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.CopyTexture2d) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected COPY_TEXTURE2D)`);
  }
  const expectedPayloadLen = AEROGPU_CMD_COPY_TEXTURE2D_SIZE - AEROGPU_CMD_HDR_SIZE;
  if (packet.payload.byteLength < expectedPayloadLen) {
    throw new Error("Buffer too small for COPY_TEXTURE2D payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const dstTexture = view.getUint32(0, true);
  const srcTexture = view.getUint32(4, true);
  const dstMipLevel = view.getUint32(8, true);
  const dstArrayLayer = view.getUint32(12, true);
  const srcMipLevel = view.getUint32(16, true);
  const srcArrayLayer = view.getUint32(20, true);
  const dstX = view.getUint32(24, true);
  const dstY = view.getUint32(28, true);
  const srcX = view.getUint32(32, true);
  const srcY = view.getUint32(36, true);
  const width = view.getUint32(40, true);
  const height = view.getUint32(44, true);
  const flags = view.getUint32(48, true);
  const reserved0 = view.getUint32(52, true);
  return {
    dstTexture,
    srcTexture,
    dstMipLevel,
    dstArrayLayer,
    srcMipLevel,
    srcArrayLayer,
    dstX,
    dstY,
    srcX,
    srcY,
    width,
    height,
    flags,
    reserved0,
  };
}

export interface AerogpuCmdSetVertexBuffersBindingsPayload {
  startSlot: number;
  bufferCount: number;
  bindings: AerogpuVertexBufferBinding[];
}

export function decodeCmdSetVertexBuffersBindings(
  bytes: Uint8Array,
  packetOffset: number,
): AerogpuCmdSetVertexBuffersBindingsPayload {
  return decodeCmdSetVertexBuffersBindingsFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdSetVertexBuffersBindingsFromPacket(
  packet: AerogpuCmdPacket,
): AerogpuCmdSetVertexBuffersBindingsPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.SetVertexBuffers) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected SET_VERTEX_BUFFERS)`);
  }
  if (packet.payload.byteLength < 8) {
    throw new Error("Buffer too small for SET_VERTEX_BUFFERS payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const startSlot = view.getUint32(0, true);
  const bufferCount = view.getUint32(4, true);

  const bindingsSize = BigInt(bufferCount) * 16n;
  const expected = BigInt(AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE) + bindingsSize;
  // Forward-compat: treat this as a minimum size so packets can be extended by appending new fields.
  if (BigInt(packet.sizeBytes) < expected) {
    throw new Error(`SET_VERTEX_BUFFERS payload size mismatch: expected at least ${expected}, got ${packet.sizeBytes}`);
  }
  if (bindingsSize > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`SET_VERTEX_BUFFERS bindings too large: ${bufferCount}`);
  }

  const bindings: AerogpuVertexBufferBinding[] = [];
  const bindingsStart = 8;
  for (let i = 0; i < bufferCount; i++) {
    const off = bindingsStart + i * 16;
    bindings.push({
      buffer: view.getUint32(off + 0, true),
      strideBytes: view.getUint32(off + 4, true),
      offsetBytes: view.getUint32(off + 8, true),
    });
  }

  return { startSlot, bufferCount, bindings };
}

export interface AerogpuCmdSetSamplersPayload {
  shaderStage: number;
  startSlot: number;
  samplerCount: number;
  reserved0: number;
  /**
   * View of `aerogpu_handle_t samplers[sampler_count]`.
   *
   * The command stream is little-endian; JS runtimes supported by Aero are little-endian,
   * so `Uint32Array` provides an allocation-free view of the handle table.
   */
  samplers: Uint32Array;
}

export function decodeCmdSetSamplersPayload(bytes: Uint8Array, packetOffset = 0): AerogpuCmdSetSamplersPayload {
  return decodeCmdSetSamplersPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdSetSamplersPayloadFromPacket(packet: AerogpuCmdPacket): AerogpuCmdSetSamplersPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.SetSamplers) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected SET_SAMPLERS)`);
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for SET_SAMPLERS payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const shaderStage = view.getUint32(0, true);
  const startSlot = view.getUint32(4, true);
  const samplerCount = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  const handlesSizeBig = BigInt(samplerCount) * 4n;
  const handlesStart = 16;
  const handlesEndBig = BigInt(handlesStart) + handlesSizeBig;
  if (handlesEndBig > BigInt(packet.payload.byteLength)) {
    throw new Error(`SET_SAMPLERS packet too small for sampler_count=${samplerCount}`);
  }
  if (handlesSizeBig > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`SET_SAMPLERS handles too large: sampler_count=${samplerCount}`);
  }

  const handlesByteOffset = packet.payload.byteOffset + handlesStart;
  if (handlesByteOffset % 4 !== 0) {
    throw new Error(`SET_SAMPLERS handles not 4-byte aligned (byteOffset=${handlesByteOffset})`);
  }

  return {
    shaderStage,
    startSlot,
    samplerCount,
    reserved0,
    samplers: new Uint32Array(packet.payload.buffer, handlesByteOffset, samplerCount),
  };
}

export interface AerogpuCmdSetConstantBuffersPayload {
  shaderStage: number;
  startSlot: number;
  bufferCount: number;
  reserved0: number;
  /**
   * View of `aerogpu_constant_buffer_binding bindings[buffer_count]`.
   *
   * Each element is 16 bytes: `{buffer:u32, offset_bytes:u32, size_bytes:u32, reserved0:u32}`.
   */
  bindings: DataView;
}

export function decodeCmdSetConstantBuffersPayload(
  bytes: Uint8Array,
  packetOffset = 0,
): AerogpuCmdSetConstantBuffersPayload {
  return decodeCmdSetConstantBuffersPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdSetConstantBuffersPayloadFromPacket(
  packet: AerogpuCmdPacket,
): AerogpuCmdSetConstantBuffersPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.SetConstantBuffers) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected SET_CONSTANT_BUFFERS)`);
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for SET_CONSTANT_BUFFERS payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const shaderStage = view.getUint32(0, true);
  const startSlot = view.getUint32(4, true);
  const bufferCount = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  const bindingsSizeBig = BigInt(bufferCount) * 16n;
  const bindingsStart = 16;
  const bindingsEndBig = BigInt(bindingsStart) + bindingsSizeBig;
  if (bindingsEndBig > BigInt(packet.payload.byteLength)) {
    throw new Error(`SET_CONSTANT_BUFFERS packet too small for buffer_count=${bufferCount}`);
  }
  if (bindingsSizeBig > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`SET_CONSTANT_BUFFERS bindings too large: buffer_count=${bufferCount}`);
  }

  return {
    shaderStage,
    startSlot,
    bufferCount,
    reserved0,
    bindings: new DataView(
      packet.payload.buffer,
      packet.payload.byteOffset + bindingsStart,
      Number(bindingsSizeBig),
    ),
  };
}

export interface AerogpuCmdSetShaderResourceBuffersPayload {
  shaderStage: number;
  startSlot: number;
  bufferCount: number;
  reserved0: number;
  /**
   * View of `aerogpu_shader_resource_buffer_binding bindings[buffer_count]`.
   *
   * Each element is 16 bytes: `{buffer:u32, offset_bytes:u32, size_bytes:u32, reserved0:u32}`.
   */
  bindings: DataView;
}

export function decodeCmdSetShaderResourceBuffersPayload(
  bytes: Uint8Array,
  packetOffset = 0,
): AerogpuCmdSetShaderResourceBuffersPayload {
  return decodeCmdSetShaderResourceBuffersPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdSetShaderResourceBuffersPayloadFromPacket(
  packet: AerogpuCmdPacket,
): AerogpuCmdSetShaderResourceBuffersPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.SetShaderResourceBuffers) {
    throw new Error(
      `Unexpected opcode: 0x${packet.opcode.toString(16)} (expected SET_SHADER_RESOURCE_BUFFERS)`,
    );
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for SET_SHADER_RESOURCE_BUFFERS payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const shaderStage = view.getUint32(0, true);
  const startSlot = view.getUint32(4, true);
  const bufferCount = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  const bindingsSizeBig = BigInt(bufferCount) * 16n;
  const bindingsStart = 16;
  const bindingsEndBig = BigInt(bindingsStart) + bindingsSizeBig;
  if (bindingsEndBig > BigInt(packet.payload.byteLength)) {
    throw new Error(`SET_SHADER_RESOURCE_BUFFERS packet too small for buffer_count=${bufferCount}`);
  }
  if (bindingsSizeBig > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`SET_SHADER_RESOURCE_BUFFERS bindings too large: buffer_count=${bufferCount}`);
  }

  return {
    shaderStage,
    startSlot,
    bufferCount,
    reserved0,
    bindings: new DataView(
      packet.payload.buffer,
      packet.payload.byteOffset + bindingsStart,
      Number(bindingsSizeBig),
    ),
  };
}

export interface AerogpuCmdSetUnorderedAccessBuffersPayload {
  shaderStage: number;
  startSlot: number;
  uavCount: number;
  reserved0: number;
  /**
   * View of `aerogpu_unordered_access_buffer_binding bindings[uav_count]`.
   *
   * Each element is 16 bytes: `{buffer:u32, offset_bytes:u32, size_bytes:u32, initial_count:u32}`.
   */
  bindings: DataView;
}

export function decodeCmdSetUnorderedAccessBuffersPayload(
  bytes: Uint8Array,
  packetOffset = 0,
): AerogpuCmdSetUnorderedAccessBuffersPayload {
  return decodeCmdSetUnorderedAccessBuffersPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdSetUnorderedAccessBuffersPayloadFromPacket(
  packet: AerogpuCmdPacket,
): AerogpuCmdSetUnorderedAccessBuffersPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.SetUnorderedAccessBuffers) {
    throw new Error(
      `Unexpected opcode: 0x${packet.opcode.toString(16)} (expected SET_UNORDERED_ACCESS_BUFFERS)`,
    );
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for SET_UNORDERED_ACCESS_BUFFERS payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const shaderStage = view.getUint32(0, true);
  const startSlot = view.getUint32(4, true);
  const uavCount = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  const bindingsSizeBig = BigInt(uavCount) * 16n;
  const bindingsStart = 16;
  const bindingsEndBig = BigInt(bindingsStart) + bindingsSizeBig;
  if (bindingsEndBig > BigInt(packet.payload.byteLength)) {
    throw new Error(`SET_UNORDERED_ACCESS_BUFFERS packet too small for uav_count=${uavCount}`);
  }
  if (bindingsSizeBig > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`SET_UNORDERED_ACCESS_BUFFERS bindings too large: uav_count=${uavCount}`);
  }

  return {
    shaderStage,
    startSlot,
    uavCount,
    reserved0,
    bindings: new DataView(
      packet.payload.buffer,
      packet.payload.byteOffset + bindingsStart,
      Number(bindingsSizeBig),
    ),
  };
}

export interface AerogpuCmdDispatchPayload {
  groupCountX: number;
  groupCountY: number;
  groupCountZ: number;
  reserved0: number;
}

export function decodeCmdDispatchPayload(bytes: Uint8Array, packetOffset = 0): AerogpuCmdDispatchPayload {
  return decodeCmdDispatchPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdDispatchPayloadFromPacket(packet: AerogpuCmdPacket): AerogpuCmdDispatchPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.Dispatch) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected DISPATCH)`);
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for DISPATCH payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const groupCountX = view.getUint32(0, true);
  const groupCountY = view.getUint32(4, true);
  const groupCountZ = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);
  return { groupCountX, groupCountY, groupCountZ, reserved0 };
}

export interface AerogpuCmdSetShaderConstantsFPayload {
  stage: number;
  startRegister: number;
  vec4Count: number;
  reserved0: number;
  data: Float32Array;
}

export function decodeCmdSetShaderConstantsFPayload(
  bytes: Uint8Array,
  packetOffset: number,
): AerogpuCmdSetShaderConstantsFPayload {
  return decodeCmdSetShaderConstantsFPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdSetShaderConstantsFPayloadFromPacket(
  packet: AerogpuCmdPacket,
): AerogpuCmdSetShaderConstantsFPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.SetShaderConstantsF) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected SET_SHADER_CONSTANTS_F)`);
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for SET_SHADER_CONSTANTS_F payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const stage = view.getUint32(0, true);
  const startRegister = view.getUint32(4, true);
  const vec4Count = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  const floatCountBig = BigInt(vec4Count) * 4n;
  const payloadBytesBig = floatCountBig * 4n;
  const payloadStart = 16;
  const payloadEndBig = BigInt(payloadStart) + payloadBytesBig;
  if (payloadEndBig > BigInt(packet.payload.byteLength)) {
    throw new Error(`SET_SHADER_CONSTANTS_F packet too small for vec4_count=${vec4Count}`);
  }
  if (floatCountBig > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`SET_SHADER_CONSTANTS_F data too large: vec4_count=${vec4Count}`);
  }

  const floatCount = Number(floatCountBig);
  const data = new Float32Array(floatCount);
  for (let i = 0; i < floatCount; i++) {
    data[i] = view.getFloat32(payloadStart + i * 4, true);
  }

  return { stage, startRegister, vec4Count, reserved0, data };
}

export interface AerogpuCmdSetShaderConstantsIPayload {
  stage: number;
  startRegister: number;
  vec4Count: number;
  reserved0: number;
  data: Int32Array;
}

export function decodeCmdSetShaderConstantsIPayload(
  bytes: Uint8Array,
  packetOffset: number,
): AerogpuCmdSetShaderConstantsIPayload {
  return decodeCmdSetShaderConstantsIPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdSetShaderConstantsIPayloadFromPacket(
  packet: AerogpuCmdPacket,
): AerogpuCmdSetShaderConstantsIPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.SetShaderConstantsI) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected SET_SHADER_CONSTANTS_I)`);
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for SET_SHADER_CONSTANTS_I payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const stage = view.getUint32(0, true);
  const startRegister = view.getUint32(4, true);
  const vec4Count = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  const i32CountBig = BigInt(vec4Count) * 4n;
  const payloadBytesBig = i32CountBig * 4n;
  const payloadStart = 16;
  const payloadEndBig = BigInt(payloadStart) + payloadBytesBig;
  if (payloadEndBig > BigInt(packet.payload.byteLength)) {
    throw new Error(`SET_SHADER_CONSTANTS_I packet too small for vec4_count=${vec4Count}`);
  }
  if (i32CountBig > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`SET_SHADER_CONSTANTS_I data too large: vec4_count=${vec4Count}`);
  }

  const i32Count = Number(i32CountBig);
  const data = new Int32Array(i32Count);
  for (let i = 0; i < i32Count; i++) {
    data[i] = view.getInt32(payloadStart + i * 4, true);
  }

  return { stage, startRegister, vec4Count, reserved0, data };
}

export interface AerogpuCmdSetShaderConstantsBPayload {
  stage: number;
  startRegister: number;
  boolCount: number;
  reserved0: number;
  data: Uint32Array;
}

export function decodeCmdSetShaderConstantsBPayload(
  bytes: Uint8Array,
  packetOffset: number,
): AerogpuCmdSetShaderConstantsBPayload {
  return decodeCmdSetShaderConstantsBPayloadFromPacket(decodePacketFromBytes(bytes, packetOffset));
}

export function decodeCmdSetShaderConstantsBPayloadFromPacket(
  packet: AerogpuCmdPacket,
): AerogpuCmdSetShaderConstantsBPayload {
  validatePacketPayloadLen(packet);
  if (packet.opcode !== AerogpuCmdOpcode.SetShaderConstantsB) {
    throw new Error(`Unexpected opcode: 0x${packet.opcode.toString(16)} (expected SET_SHADER_CONSTANTS_B)`);
  }
  if (packet.payload.byteLength < 16) {
    throw new Error("Buffer too small for SET_SHADER_CONSTANTS_B payload");
  }

  const view = new DataView(packet.payload.buffer, packet.payload.byteOffset, packet.payload.byteLength);
  const stage = view.getUint32(0, true);
  const startRegister = view.getUint32(4, true);
  const boolCount = view.getUint32(8, true);
  const reserved0 = view.getUint32(12, true);

  // Payload encoding: `uint32_t data[bool_count]` (one scalar u32 per bool register).
  const payloadBytesBig = BigInt(boolCount) * 4n;
  const payloadStart = 16;
  const payloadEndBig = BigInt(payloadStart) + payloadBytesBig;
  if (payloadEndBig > BigInt(packet.payload.byteLength)) {
    throw new Error(`SET_SHADER_CONSTANTS_B packet too small for bool_count=${boolCount}`);
  }
  if (BigInt(boolCount) > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`SET_SHADER_CONSTANTS_B data too large: bool_count=${boolCount}`);
  }

  const data = new Uint32Array(boolCount);
  for (let i = 0; i < boolCount; i++) {
    data[i] = view.getUint32(payloadStart + i * 4, true);
  }

  return { stage, startRegister, boolCount, reserved0, data };
}

/**
 * Safe command stream builder for `aerogpu_cmd.h`.
 *
 * Primarily intended for tests/fixtures and host-side tooling.
 */
export class AerogpuCmdWriter {
  private buf: ArrayBuffer = new ArrayBuffer(0);
  private view: DataView = new DataView(this.buf);
  private len = 0;

  private static _alignUp(v: number, a: number): number {
    return alignUp(v, a);
  }

  constructor() {
    this.reset();
  }

  reset(): void {
    this.buf = new ArrayBuffer(AEROGPU_CMD_STREAM_HEADER_SIZE);
    this.view = new DataView(this.buf);
    this.len = AEROGPU_CMD_STREAM_HEADER_SIZE;

    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC, AEROGPU_CMD_STREAM_MAGIC, true);
    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION, AEROGPU_ABI_VERSION_U32, true);
    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, this.len, true);
    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS, 0, true);
  }

  finish(): Uint8Array<ArrayBuffer> {
    if (this.len > 0xffffffff) {
      throw new Error(`command stream too large for u32 sizeBytes: ${this.len}`);
    }
    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, this.len, true);
    // The command writer always produces a fresh `ArrayBuffer`-backed byte array so callers can
    // transfer the `.buffer` directly across worker boundaries.
    return new Uint8Array(this.buf, 0, this.len).slice() as Uint8Array<ArrayBuffer>;
  }

  private ensureCapacity(capacity: number): void {
    if (this.buf.byteLength >= capacity) return;
    let newCap = this.buf.byteLength;
    while (newCap < capacity) newCap = Math.max(64, newCap * 2);
    const next = new ArrayBuffer(newCap);
    new Uint8Array(next).set(new Uint8Array(this.buf, 0, this.len));
    this.buf = next;
    this.view = new DataView(this.buf);
  }

  private appendRaw(opcode: AerogpuCmdOpcode, cmdSize: number): number {
    const alignedSize = AerogpuCmdWriter._alignUp(cmdSize, 4);
    if (alignedSize > 0xffffffff) {
      throw new Error(`command packet too large for u32 sizeBytes: ${alignedSize}`);
    }
    const offset = this.len;
    if (offset + alignedSize > 0xffffffff) {
      throw new Error(`command stream too large for u32 sizeBytes: ${offset + alignedSize}`);
    }
    this.ensureCapacity(offset + alignedSize);
    new Uint8Array(this.buf, offset, alignedSize).fill(0);
    this.view.setUint32(offset + AEROGPU_CMD_HDR_OFF_OPCODE, opcode, true);
    this.view.setUint32(offset + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, alignedSize, true);
    this.len += alignedSize;
    return offset;
  }

  debugMarker(marker: string | Uint8Array): void {
    const markerBytes = typeof marker === "string" ? new TextEncoder().encode(marker) : marker;
    const unpadded = AEROGPU_CMD_HDR_SIZE + markerBytes.byteLength;
    const base = this.appendRaw(AerogpuCmdOpcode.DebugMarker, unpadded);
    new Uint8Array(this.buf, base + AEROGPU_CMD_HDR_SIZE, markerBytes.byteLength).set(markerBytes);
  }

  createBuffer(
    bufferHandle: AerogpuHandle,
    usageFlags: number,
    sizeBytes: bigint,
    backingAllocId: number,
    backingOffsetBytes: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.CreateBuffer, AEROGPU_CMD_CREATE_BUFFER_SIZE);
    this.view.setUint32(base + 8, bufferHandle, true);
    this.view.setUint32(base + 12, usageFlags, true);
    this.view.setBigUint64(base + 16, sizeBytes, true);
    this.view.setUint32(base + 24, backingAllocId, true);
    this.view.setUint32(base + 28, backingOffsetBytes, true);
  }

  createTexture2d(
    textureHandle: AerogpuHandle,
    usageFlags: number,
    format: number,
    width: number,
    height: number,
    mipLevels: number,
    arrayLayers: number,
    rowPitchBytes: number,
    backingAllocId: number,
    backingOffsetBytes: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.CreateTexture2d, AEROGPU_CMD_CREATE_TEXTURE2D_SIZE);
    this.view.setUint32(base + 8, textureHandle, true);
    this.view.setUint32(base + 12, usageFlags, true);
    this.view.setUint32(base + 16, format, true);
    this.view.setUint32(base + 20, width, true);
    this.view.setUint32(base + 24, height, true);
    this.view.setUint32(base + 28, mipLevels, true);
    this.view.setUint32(base + 32, arrayLayers, true);
    this.view.setUint32(base + 36, rowPitchBytes, true);
    this.view.setUint32(base + 40, backingAllocId, true);
    this.view.setUint32(base + 44, backingOffsetBytes, true);
  }

  destroyResource(resourceHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DestroyResource, AEROGPU_CMD_DESTROY_RESOURCE_SIZE);
    this.view.setUint32(base + 8, resourceHandle, true);
  }

  resourceDirtyRange(resourceHandle: AerogpuHandle, offsetBytes: bigint, sizeBytes: bigint): void {
    const base = this.appendRaw(AerogpuCmdOpcode.ResourceDirtyRange, AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE);
    this.view.setUint32(base + 8, resourceHandle, true);
    this.view.setBigUint64(base + 16, offsetBytes, true);
    this.view.setBigUint64(base + 24, sizeBytes, true);
  }

  uploadResource(resourceHandle: AerogpuHandle, offsetBytes: bigint, data: Uint8Array): void {
    const unpadded = AEROGPU_CMD_UPLOAD_RESOURCE_SIZE + data.byteLength;
    const base = this.appendRaw(AerogpuCmdOpcode.UploadResource, unpadded);
    this.view.setUint32(base + 8, resourceHandle, true);
    this.view.setBigUint64(base + 16, offsetBytes, true);
    this.view.setBigUint64(base + 24, BigInt(data.byteLength), true);
    new Uint8Array(this.buf, base + AEROGPU_CMD_UPLOAD_RESOURCE_SIZE, data.byteLength).set(data);
  }

  copyBuffer(
    dstBuffer: AerogpuHandle,
    srcBuffer: AerogpuHandle,
    dstOffsetBytes: bigint,
    srcOffsetBytes: bigint,
    sizeBytes: bigint,
    flags: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.CopyBuffer, AEROGPU_CMD_COPY_BUFFER_SIZE);
    this.view.setUint32(base + 8, dstBuffer, true);
    this.view.setUint32(base + 12, srcBuffer, true);
    this.view.setBigUint64(base + 16, dstOffsetBytes, true);
    this.view.setBigUint64(base + 24, srcOffsetBytes, true);
    this.view.setBigUint64(base + 32, sizeBytes, true);
    this.view.setUint32(base + 40, flags, true);
  }

  copyTexture2d(
    dstTexture: AerogpuHandle,
    srcTexture: AerogpuHandle,
    dstMipLevel: number,
    dstArrayLayer: number,
    srcMipLevel: number,
    srcArrayLayer: number,
    dstX: number,
    dstY: number,
    srcX: number,
    srcY: number,
    width: number,
    height: number,
    flags: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.CopyTexture2d, AEROGPU_CMD_COPY_TEXTURE2D_SIZE);
    this.view.setUint32(base + 8, dstTexture, true);
    this.view.setUint32(base + 12, srcTexture, true);
    this.view.setUint32(base + 16, dstMipLevel, true);
    this.view.setUint32(base + 20, dstArrayLayer, true);
    this.view.setUint32(base + 24, srcMipLevel, true);
    this.view.setUint32(base + 28, srcArrayLayer, true);
    this.view.setUint32(base + 32, dstX, true);
    this.view.setUint32(base + 36, dstY, true);
    this.view.setUint32(base + 40, srcX, true);
    this.view.setUint32(base + 44, srcY, true);
    this.view.setUint32(base + 48, width, true);
    this.view.setUint32(base + 52, height, true);
    this.view.setUint32(base + 56, flags, true);
  }

  createShaderDxbc(
    shaderHandle: AerogpuHandle,
    stage: AerogpuShaderStage,
    dxbcBytes: Uint8Array,
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    // Validate `stageEx` invariants before mutating the stream buffer so callers can safely catch
    // encoding errors without leaving a partially-written packet in the command stream.
    const reserved0 = encodeStageExReserved0(stage, stageEx);
    const unpadded = AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE + dxbcBytes.byteLength;
    const base = this.appendRaw(AerogpuCmdOpcode.CreateShaderDxbc, unpadded);
    this.view.setUint32(base + 8, shaderHandle, true);
    this.view.setUint32(base + 12, stage, true);
    this.view.setUint32(base + 16, dxbcBytes.byteLength, true);
    this.view.setUint32(base + 20, reserved0, true);
    new Uint8Array(this.buf, base + AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE, dxbcBytes.byteLength).set(dxbcBytes);
  }

  /**
   * Stage-ex aware variant of {@link createShaderDxbc}.
   *
   * Encodes `stageEx` using the ABI `stage_ex` mechanism (see `drivers/aerogpu/protocol/aerogpu_cmd.h`):
   * - The legacy `stage` field is forced to `COMPUTE`.
   * - The `reserved0` field carries the non-zero `stageEx` discriminator.
   *
   * Note: DXBC program type `0` (Pixel) is intentionally not representable here because `reserved0 == 0`
   * must remain the legacy/default encoding (old guests always write 0 into reserved fields). Pixel
   * shaders should be created via {@link createShaderDxbc} with `stage = AerogpuShaderStage.Pixel`
   * (and no stageEx).
   */
  createShaderDxbcEx(shaderHandle: AerogpuHandle, stageEx: AerogpuShaderStageEx, dxbcBytes: Uint8Array): void {
    // CREATE_SHADER_DXBC uses `reserved0` for the stage_ex ABI extension.
    //
    // `stageEx == 0` (DXBC Pixel program type) is not representable because `reserved0 == 0` is
    // reserved for the legacy/default encoding (old guests always wrote 0 into reserved fields).
    if ((stageEx >>> 0) === 0) {
      throw new Error(
        "CREATE_SHADER_DXBC stage_ex cannot encode DXBC Pixel program type (0); use createShaderDxbc(stage=Pixel) instead",
      );
    }
    // Delegate to the stageEx-optional variant with `stage = COMPUTE` to avoid duplicating the
    // variable-length packet encoding logic.
    this.createShaderDxbc(shaderHandle, AerogpuShaderStage.Compute, dxbcBytes, stageEx);
  }

  destroyShader(shaderHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DestroyShader, AEROGPU_CMD_DESTROY_SHADER_SIZE);
    this.view.setUint32(base + 8, shaderHandle, true);
  }

  /**
   * Legacy BIND_SHADERS variant that can encode an optional geometry shader via `reserved0`.
   *
   * ABI note:
   * - Base packet size is 24 bytes (hdr + vs/ps/cs/reserved0).
   * - Legacy behavior: when `sizeBytes == 24` and `reserved0 != 0`, `reserved0` is interpreted as
   *   the geometry shader (`gs`) handle.
   * - Newer hosts support an append-only extension (see {@link bindShadersEx}): if `sizeBytes >= 36`,
   *   the packet appends trailing `{gs, hs, ds}` handles. In the extended form, the trailing handles
   *   are authoritative and `reserved0` should be 0 unless mirroring `gs` for best-effort
   *   compatibility.
   */
  bindShadersWithGs(vs: AerogpuHandle, gs: AerogpuHandle, ps: AerogpuHandle, cs: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.BindShaders, AEROGPU_CMD_BIND_SHADERS_SIZE);
    this.view.setUint32(base + 8, vs, true);
    this.view.setUint32(base + 12, ps, true);
    this.view.setUint32(base + 16, cs, true);
    this.view.setUint32(base + 20, gs, true);
  }

  /**
   * BIND_SHADERS using the canonical append-only ABI extension: trailing `{gs, hs, ds}` handles.
   *
   * ABI note: the base packet layout remains unchanged; the extended handles are appended after the
   * `aerogpu_cmd_bind_shaders` struct. By default we keep `reserved0=0` to preserve strict
   * append-only semantics, but callers may optionally mirror `gs` into `reserved0` for best-effort
   * compatibility with legacy decoders.
   */
  bindShadersEx(
    vs: AerogpuHandle,
    ps: AerogpuHandle,
    cs: AerogpuHandle,
    gs: AerogpuHandle,
    hs: AerogpuHandle,
    ds: AerogpuHandle,
    mirrorGsToReserved0?: boolean,
  ): void;
  bindShadersEx(
    vs: AerogpuHandle,
    ps: AerogpuHandle,
    cs: AerogpuHandle,
    ex: BindShadersEx,
    mirrorGsToReserved0?: boolean,
  ): void;
  bindShadersEx(
    vs: AerogpuHandle,
    ps: AerogpuHandle,
    cs: AerogpuHandle,
    gsOrEx: AerogpuHandle | BindShadersEx,
    hsOrMirror?: AerogpuHandle | boolean,
    ds?: AerogpuHandle,
    mirrorGsToReserved0 = false,
  ): void {
    let gs: AerogpuHandle;
    let hs: AerogpuHandle;
    let dsFinal: AerogpuHandle;
    let mirror = mirrorGsToReserved0;
    if (typeof gsOrEx === "object" && gsOrEx !== null) {
      gs = gsOrEx.gs;
      hs = gsOrEx.hs;
      dsFinal = gsOrEx.ds;
      // Object-call form: bindShadersEx(vs, ps, cs, {gs,hs,ds}, mirrorGsToReserved0?)
      if (typeof hsOrMirror === "boolean") {
        mirror = hsOrMirror;
      } else if (hsOrMirror !== undefined) {
        throw new Error("bindShadersEx: when passing {gs,hs,ds}, the 5th argument must be mirrorGsToReserved0");
      }
      if (ds !== undefined) {
        throw new Error("bindShadersEx: too many arguments for {gs,hs,ds} form");
      }
    } else {
      gs = gsOrEx;
      // Handle-call form: bindShadersEx(vs, ps, cs, gs, hs, ds, mirrorGsToReserved0?)
      if (hsOrMirror === undefined || typeof hsOrMirror === "boolean") {
        throw new Error("bindShadersEx: missing hs handle");
      }
      hs = hsOrMirror;
      if (ds === undefined) {
        throw new Error("bindShadersEx: missing ds handle");
      }
      dsFinal = ds;
    }

    const base = this.appendRaw(AerogpuCmdOpcode.BindShaders, AEROGPU_CMD_BIND_SHADERS_EX_SIZE);
    this.view.setUint32(base + 8, vs, true);
    this.view.setUint32(base + 12, ps, true);
    this.view.setUint32(base + 16, cs, true);
    this.view.setUint32(base + 20, mirror ? gs : 0, true);
    // ABI extension payload: trailing `{gs, hs, ds}`.
    this.view.setUint32(base + 24, gs, true);
    this.view.setUint32(base + 28, hs, true);
    this.view.setUint32(base + 32, dsFinal, true);
  }

  /**
   * Convenience helper for binding tessellation shaders (HS/DS) via the append-only BIND_SHADERS
   * extension.
   *
   * Leaves VS/PS/CS/GS unbound (0).
   */
  bindShadersHsDs(hs: AerogpuHandle, ds: AerogpuHandle): void {
    this.bindShadersEx(/*vs=*/ 0, /*ps=*/ 0, /*cs=*/ 0, { gs: 0, hs, ds });
  }

  bindShaders(vs: AerogpuHandle, ps: AerogpuHandle, cs: AerogpuHandle): void {
    this.bindShadersWithGs(vs, 0, ps, cs);
  }

  setShaderConstantsF(
    stage: AerogpuShaderStage,
    startRegister: number,
    data: Float32Array | readonly number[],
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    if (data.length % 4 !== 0) {
      throw new Error(`SET_SHADER_CONSTANTS_F data must be float4-aligned (got ${data.length} floats)`);
    }

    const vec4Count = data.length / 4;
    const unpadded = AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + data.length * 4;
    const stageEnc = stageEx === null || stageEx === undefined ? stage : AerogpuShaderStage.Compute;
    const reserved0 = encodeStageExReserved0(stageEnc, stageEx);
    const base = this.appendRaw(AerogpuCmdOpcode.SetShaderConstantsF, unpadded);
    this.view.setUint32(base + 8, stageEnc, true);
    this.view.setUint32(base + 12, startRegister, true);
    this.view.setUint32(base + 16, vec4Count, true);
    this.view.setUint32(base + 20, reserved0, true);
    for (let i = 0; i < data.length; i++) {
      this.view.setFloat32(base + AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + i * 4, data[i]!, true);
    }
  }

  /**
   * Stage-ex aware variant of {@link setShaderConstantsF}.
   *
   * Encodes `stageEx` into `reserved0` and sets the legacy `stage` field to `COMPUTE`.
   */
  setShaderConstantsFEx(
    stageEx: AerogpuShaderStageEx,
    startRegister: number,
    data: Float32Array | readonly number[],
  ): void {
    // Delegate to the stageEx-optional variant so the packet encoding logic stays in one place.
    this.setShaderConstantsF(AerogpuShaderStage.Compute, startRegister, data, stageEx);
  }

  setShaderConstantsI(
    stage: AerogpuShaderStage,
    startRegister: number,
    data: Int32Array | readonly number[],
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    if (data.length % 4 !== 0) {
      throw new Error(`SET_SHADER_CONSTANTS_I data must be int4-aligned (got ${data.length} ints)`);
    }

    const vec4Count = data.length / 4;
    const unpadded = AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE + data.length * 4;
    const stageEnc = stageEx === null || stageEx === undefined ? stage : AerogpuShaderStage.Compute;
    const reserved0 = encodeStageExReserved0(stageEnc, stageEx);
    const base = this.appendRaw(AerogpuCmdOpcode.SetShaderConstantsI, unpadded);
    this.view.setUint32(base + 8, stageEnc, true);
    this.view.setUint32(base + 12, startRegister, true);
    this.view.setUint32(base + 16, vec4Count, true);
    this.view.setUint32(base + 20, reserved0, true);
    for (let i = 0; i < data.length; i++) {
      this.view.setInt32(base + AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE + i * 4, data[i]!, true);
    }
  }

  /**
   * Stage-ex aware variant of {@link setShaderConstantsI}.
   *
   * Encodes `stageEx` into `reserved0` and sets the legacy `stage` field to `COMPUTE`.
   */
  setShaderConstantsIEx(
    stageEx: AerogpuShaderStageEx,
    startRegister: number,
    data: Int32Array | readonly number[],
  ): void {
    // Delegate to the stageEx-optional variant so the packet encoding logic stays in one place.
    this.setShaderConstantsI(AerogpuShaderStage.Compute, startRegister, data, stageEx);
  }

  /**
   * SET_SHADER_CONSTANTS_B.
   *
   * `data` is a contiguous range of scalar bool registers, represented as booleans or numbers.
   * Each element must encode `0` or `1` (booleans are converted to 0/1).
   *
   * Payload encoding: one `u32` per bool register.
   */
  setShaderConstantsB(
    stage: AerogpuShaderStage,
    startRegister: number,
    data: Uint32Array | readonly (boolean | number)[],
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    const boolCount = data.length;
    const unpadded = AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE + boolCount * 4;
    const stageEnc = stageEx === null || stageEx === undefined ? stage : AerogpuShaderStage.Compute;
    const reserved0 = encodeStageExReserved0(stageEnc, stageEx);
    const base = this.appendRaw(AerogpuCmdOpcode.SetShaderConstantsB, unpadded);
    this.view.setUint32(base + 8, stageEnc, true);
    this.view.setUint32(base + 12, startRegister, true);
    this.view.setUint32(base + 16, boolCount, true);
    this.view.setUint32(base + 20, reserved0, true);
    for (let i = 0; i < boolCount; i++) {
      const inValue = data[i]!;
      const v = typeof inValue === "boolean" ? (inValue ? 1 : 0) : inValue;
      if (v !== 0 && v !== 1) {
        throw new Error(`SET_SHADER_CONSTANTS_B data must be 0/1 (got ${v} at index ${i})`);
      }
      this.view.setUint32(base + AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE + i * 4, v, true);
    }
  }

  /**
   * Stage-ex aware variant of {@link setShaderConstantsB}.
   *
   * Encodes `stageEx` into `reserved0` and sets the legacy `stage` field to `COMPUTE`.
   */
  setShaderConstantsBEx(
    stageEx: AerogpuShaderStageEx,
    startRegister: number,
    data: Uint32Array | readonly (boolean | number)[],
  ): void {
    // Delegate to the stageEx-optional variant so the packet encoding logic stays in one place.
    this.setShaderConstantsB(AerogpuShaderStage.Compute, startRegister, data, stageEx);
  }

  createInputLayout(inputLayoutHandle: AerogpuHandle, blob: Uint8Array): void {
    const unpadded = AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE + blob.byteLength;
    const base = this.appendRaw(AerogpuCmdOpcode.CreateInputLayout, unpadded);
    this.view.setUint32(base + 8, inputLayoutHandle, true);
    this.view.setUint32(base + 12, blob.byteLength, true);
    new Uint8Array(this.buf, base + AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE, blob.byteLength).set(blob);
  }

  destroyInputLayout(inputLayoutHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DestroyInputLayout, AEROGPU_CMD_DESTROY_INPUT_LAYOUT_SIZE);
    this.view.setUint32(base + 8, inputLayoutHandle, true);
  }

  setInputLayout(inputLayoutHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetInputLayout, AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE);
    this.view.setUint32(base + 8, inputLayoutHandle, true);
  }

  setBlendState(
    enable: boolean | number,
    srcFactor: AerogpuBlendFactor,
    dstFactor: AerogpuBlendFactor,
    blendOp: AerogpuBlendOp,
    colorWriteMask: number,
    srcFactorAlpha: AerogpuBlendFactor = srcFactor,
    dstFactorAlpha: AerogpuBlendFactor = dstFactor,
    blendOpAlpha: AerogpuBlendOp = blendOp,
    blendConstantRgba: [number, number, number, number] = [1, 1, 1, 1],
    sampleMask = 0xffffffff,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetBlendState, AEROGPU_CMD_SET_BLEND_STATE_SIZE);
    this.view.setUint32(base + 8, enable ? 1 : 0, true);
    this.view.setUint32(base + 12, srcFactor, true);
    this.view.setUint32(base + 16, dstFactor, true);
    this.view.setUint32(base + 20, blendOp, true);
    this.view.setUint8(base + 24, colorWriteMask);
    this.view.setUint32(base + 28, srcFactorAlpha, true);
    this.view.setUint32(base + 32, dstFactorAlpha, true);
    this.view.setUint32(base + 36, blendOpAlpha, true);
    for (let i = 0; i < 4; i++) {
      this.view.setFloat32(base + 40 + i * 4, blendConstantRgba[i]!, true);
    }
    this.view.setUint32(base + 56, sampleMask >>> 0, true);
  }

  setDepthStencilState(
    depthEnable: boolean | number,
    depthWriteEnable: boolean | number,
    depthFunc: AerogpuCompareFunc,
    stencilEnable: boolean | number,
    stencilReadMask: number,
    stencilWriteMask: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetDepthStencilState, AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE);
    this.view.setUint32(base + 8, depthEnable ? 1 : 0, true);
    this.view.setUint32(base + 12, depthWriteEnable ? 1 : 0, true);
    this.view.setUint32(base + 16, depthFunc, true);
    this.view.setUint32(base + 20, stencilEnable ? 1 : 0, true);
    this.view.setUint8(base + 24, stencilReadMask);
    this.view.setUint8(base + 25, stencilWriteMask);
  }

  setRasterizerState(
    fillMode: AerogpuFillMode,
    cullMode: AerogpuCullMode,
    frontCcw: boolean | number,
    scissorEnable: boolean | number,
    depthBias: number,
    flags = 0,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetRasterizerState, AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE);
    this.view.setUint32(base + 8, fillMode, true);
    this.view.setUint32(base + 12, cullMode, true);
    this.view.setUint32(base + 16, frontCcw ? 1 : 0, true);
    this.view.setUint32(base + 20, scissorEnable ? 1 : 0, true);
    this.view.setInt32(base + 24, depthBias, true);
    this.view.setUint32(base + 28, flags, true);
  }

  setRenderTargets(colors: readonly AerogpuHandle[], depthStencil: AerogpuHandle): void {
    if (colors.length > AEROGPU_MAX_RENDER_TARGETS) {
      throw new Error(`too many render targets: ${colors.length}`);
    }
    const base = this.appendRaw(AerogpuCmdOpcode.SetRenderTargets, AEROGPU_CMD_SET_RENDER_TARGETS_SIZE);
    this.view.setUint32(base + 8, colors.length, true);
    this.view.setUint32(base + 12, depthStencil, true);
    for (let i = 0; i < colors.length; i++) {
      this.view.setUint32(base + 16 + i * 4, colors[i], true);
    }
  }

  setViewport(x: number, y: number, width: number, height: number, minDepth: number, maxDepth: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetViewport, AEROGPU_CMD_SET_VIEWPORT_SIZE);
    this.view.setFloat32(base + 8, x, true);
    this.view.setFloat32(base + 12, y, true);
    this.view.setFloat32(base + 16, width, true);
    this.view.setFloat32(base + 20, height, true);
    this.view.setFloat32(base + 24, minDepth, true);
    this.view.setFloat32(base + 28, maxDepth, true);
  }

  setScissor(x: number, y: number, width: number, height: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetScissor, AEROGPU_CMD_SET_SCISSOR_SIZE);
    this.view.setInt32(base + 8, x, true);
    this.view.setInt32(base + 12, y, true);
    this.view.setInt32(base + 16, width, true);
    this.view.setInt32(base + 20, height, true);
  }

  setVertexBuffers(startSlot: number, bindings: readonly AerogpuVertexBufferBinding[]): void {
    const unpadded = AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE + bindings.length * 16;
    const base = this.appendRaw(AerogpuCmdOpcode.SetVertexBuffers, unpadded);
    this.view.setUint32(base + 8, startSlot, true);
    this.view.setUint32(base + 12, bindings.length, true);
    for (let i = 0; i < bindings.length; i++) {
      const b = bindings[i];
      const off = base + AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE + i * 16;
      this.view.setUint32(off + 0, b.buffer, true);
      this.view.setUint32(off + 4, b.strideBytes, true);
      this.view.setUint32(off + 8, b.offsetBytes, true);
    }
  }

  setIndexBuffer(buffer: AerogpuHandle, format: AerogpuIndexFormat, offsetBytes: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetIndexBuffer, AEROGPU_CMD_SET_INDEX_BUFFER_SIZE);
    this.view.setUint32(base + 8, buffer, true);
    this.view.setUint32(base + 12, format, true);
    this.view.setUint32(base + 16, offsetBytes, true);
  }

  setPrimitiveTopology(topology: AerogpuPrimitiveTopology): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetPrimitiveTopology, AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE);
    this.view.setUint32(base + 8, topology, true);
  }

  setTexture(
    shaderStage: AerogpuShaderStage,
    slot: number,
    texture: AerogpuHandle,
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    const shaderStageEnc =
      stageEx === null || stageEx === undefined ? shaderStage : AerogpuShaderStage.Compute;
    const reserved0 = encodeStageExReserved0(shaderStageEnc, stageEx);
    const base = this.appendRaw(AerogpuCmdOpcode.SetTexture, AEROGPU_CMD_SET_TEXTURE_SIZE);
    this.view.setUint32(base + 8, shaderStageEnc, true);
    this.view.setUint32(base + 12, slot, true);
    this.view.setUint32(base + 16, texture, true);
    this.view.setUint32(base + 20, reserved0, true);
  }

  /**
   * Stage-ex aware variant of {@link setTexture}.
   *
   * Encodes `stageEx` into `reserved0` and sets the legacy `shaderStage` field to `COMPUTE`.
   */
  setTextureEx(stageEx: AerogpuShaderStageEx, slot: number, texture: AerogpuHandle): void {
    // Delegate to the stageEx-optional variant so the packet encoding logic stays in one place.
    this.setTexture(AerogpuShaderStage.Compute, slot, texture, stageEx);
  }

  setSamplerState(shaderStage: AerogpuShaderStage, slot: number, state: number, value: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetSamplerState, AEROGPU_CMD_SET_SAMPLER_STATE_SIZE);
    this.view.setUint32(base + 8, shaderStage, true);
    this.view.setUint32(base + 12, slot, true);
    this.view.setUint32(base + 16, state, true);
    this.view.setUint32(base + 20, value, true);
  }

  createSampler(
    samplerHandle: AerogpuHandle,
    filter: AerogpuSamplerFilter,
    addressU: AerogpuSamplerAddressMode,
    addressV: AerogpuSamplerAddressMode,
    addressW: AerogpuSamplerAddressMode,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.CreateSampler, AEROGPU_CMD_CREATE_SAMPLER_SIZE);
    this.view.setUint32(base + 8, samplerHandle, true);
    this.view.setUint32(base + 12, filter, true);
    this.view.setUint32(base + 16, addressU, true);
    this.view.setUint32(base + 20, addressV, true);
    this.view.setUint32(base + 24, addressW, true);
  }

  destroySampler(samplerHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DestroySampler, AEROGPU_CMD_DESTROY_SAMPLER_SIZE);
    this.view.setUint32(base + 8, samplerHandle, true);
  }

  setSamplers(
    shaderStage: AerogpuShaderStage,
    startSlot: number,
    handles: ArrayLike<AerogpuHandle>,
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    const unpadded = AEROGPU_CMD_SET_SAMPLERS_SIZE + handles.length * 4;
    const shaderStageEnc =
      stageEx === null || stageEx === undefined ? shaderStage : AerogpuShaderStage.Compute;
    const reserved0 = encodeStageExReserved0(shaderStageEnc, stageEx);
    const base = this.appendRaw(AerogpuCmdOpcode.SetSamplers, unpadded);
    this.view.setUint32(base + 8, shaderStageEnc, true);
    this.view.setUint32(base + 12, startSlot, true);
    this.view.setUint32(base + 16, handles.length, true);
    this.view.setUint32(base + 20, reserved0, true);
    for (let i = 0; i < handles.length; i++) {
      this.view.setUint32(base + AEROGPU_CMD_SET_SAMPLERS_SIZE + i * 4, handles[i]!, true);
    }
  }

  /**
   * Stage-ex aware variant of {@link setSamplers}.
   *
   * Encodes `stageEx` into `reserved0` and sets the legacy `shaderStage` field to `COMPUTE`.
   */
  setSamplersEx(stageEx: AerogpuShaderStageEx, startSlot: number, handles: ArrayLike<AerogpuHandle>): void {
    // Delegate to the stageEx-optional variant so the packet encoding logic stays in one place.
    this.setSamplers(AerogpuShaderStage.Compute, startSlot, handles, stageEx);
  }

  setConstantBuffers(
    shaderStage: AerogpuShaderStage,
    startSlot: number,
    bindings: readonly AerogpuConstantBufferBinding[],
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    const unpadded = AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + bindings.length * 16;
    const shaderStageEnc =
      stageEx === null || stageEx === undefined ? shaderStage : AerogpuShaderStage.Compute;
    const reserved0 = encodeStageExReserved0(shaderStageEnc, stageEx);
    const base = this.appendRaw(AerogpuCmdOpcode.SetConstantBuffers, unpadded);
    this.view.setUint32(base + 8, shaderStageEnc, true);
    this.view.setUint32(base + 12, startSlot, true);
    this.view.setUint32(base + 16, bindings.length, true);
    this.view.setUint32(base + 20, reserved0, true);
    for (let i = 0; i < bindings.length; i++) {
      const b = bindings[i];
      const off = base + AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + i * 16;
      this.view.setUint32(off + 0, b.buffer, true);
      this.view.setUint32(off + 4, b.offsetBytes, true);
      this.view.setUint32(off + 8, b.sizeBytes, true);
    }
  }

  /**
   * Stage-ex aware variant of {@link setConstantBuffers}.
   *
   * Encodes `stageEx` into `reserved0` and sets the legacy `shaderStage` field to `COMPUTE`.
   */
  setConstantBuffersEx(
    stageEx: AerogpuShaderStageEx,
    startSlot: number,
    bindings: readonly AerogpuConstantBufferBinding[],
  ): void {
    // Delegate to the stageEx-optional variant so the packet encoding logic stays in one place.
    this.setConstantBuffers(AerogpuShaderStage.Compute, startSlot, bindings, stageEx);
  }

  setShaderResourceBuffers(
    shaderStage: AerogpuShaderStage,
    startSlot: number,
    bindings: readonly AerogpuShaderResourceBufferBinding[],
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    const unpadded = AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + bindings.length * 16;
    const shaderStageEnc =
      stageEx === null || stageEx === undefined ? shaderStage : AerogpuShaderStage.Compute;
    const reserved0 = encodeStageExReserved0(shaderStageEnc, stageEx);
    const base = this.appendRaw(AerogpuCmdOpcode.SetShaderResourceBuffers, unpadded);
    this.view.setUint32(base + 8, shaderStageEnc, true);
    this.view.setUint32(base + 12, startSlot, true);
    this.view.setUint32(base + 16, bindings.length, true);
    this.view.setUint32(base + 20, reserved0, true);
    for (let i = 0; i < bindings.length; i++) {
      const b = bindings[i];
      const off = base + AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + i * 16;
      this.view.setUint32(off + 0, b.buffer, true);
      this.view.setUint32(off + 4, b.offsetBytes, true);
      this.view.setUint32(off + 8, b.sizeBytes, true);
    }
  }

  /**
   * Stage-ex aware variant of {@link setShaderResourceBuffers}.
   *
   * Encodes `stageEx` via `(shaderStage = COMPUTE, reserved0 = stageEx)` (non-zero).
   */
  setShaderResourceBuffersEx(
    stageEx: AerogpuShaderStageEx,
    startSlot: number,
    bindings: readonly AerogpuShaderResourceBufferBinding[],
  ): void {
    // Delegate to the stageEx-optional variant so the packet encoding logic stays in one place.
    this.setShaderResourceBuffers(AerogpuShaderStage.Compute, startSlot, bindings, stageEx);
  }

  setUnorderedAccessBuffers(
    shaderStage: AerogpuShaderStage,
    startSlot: number,
    bindings: readonly AerogpuUnorderedAccessBufferBinding[],
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    const unpadded = AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + bindings.length * 16;
    const shaderStageEnc =
      stageEx === null || stageEx === undefined ? shaderStage : AerogpuShaderStage.Compute;
    const reserved0 = encodeStageExReserved0(shaderStageEnc, stageEx);
    const base = this.appendRaw(AerogpuCmdOpcode.SetUnorderedAccessBuffers, unpadded);
    this.view.setUint32(base + 8, shaderStageEnc, true);
    this.view.setUint32(base + 12, startSlot, true);
    this.view.setUint32(base + 16, bindings.length, true);
    this.view.setUint32(base + 20, reserved0, true);
    for (let i = 0; i < bindings.length; i++) {
      const b = bindings[i];
      const off = base + AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + i * 16;
      this.view.setUint32(off + 0, b.buffer, true);
      this.view.setUint32(off + 4, b.offsetBytes, true);
      this.view.setUint32(off + 8, b.sizeBytes, true);
      this.view.setUint32(off + 12, b.initialCount, true);
    }
  }

  /**
   * Stage-ex aware variant of {@link setUnorderedAccessBuffers}.
   *
   * Encodes `stageEx` via `(shaderStage = COMPUTE, reserved0 = stageEx)` (non-zero).
   */
  setUnorderedAccessBuffersEx(
    stageEx: AerogpuShaderStageEx,
    startSlot: number,
    bindings: readonly AerogpuUnorderedAccessBufferBinding[],
  ): void {
    // Delegate to the stageEx-optional variant so the packet encoding logic stays in one place.
    this.setUnorderedAccessBuffers(AerogpuShaderStage.Compute, startSlot, bindings, stageEx);
  }
  setRenderState(state: number, value: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetRenderState, AEROGPU_CMD_SET_RENDER_STATE_SIZE);
    this.view.setUint32(base + 8, state, true);
    this.view.setUint32(base + 12, value, true);
  }

  clear(flags: number, colorRgba: [number, number, number, number], depth: number, stencil: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.Clear, AEROGPU_CMD_CLEAR_SIZE);
    this.view.setUint32(base + 8, flags, true);
    for (let i = 0; i < 4; i++) {
      this.view.setFloat32(base + 12 + i * 4, colorRgba[i], true);
    }
    this.view.setFloat32(base + 28, depth, true);
    this.view.setUint32(base + 32, stencil, true);
  }

  draw(vertexCount: number, instanceCount: number, firstVertex: number, firstInstance: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.Draw, AEROGPU_CMD_DRAW_SIZE);
    this.view.setUint32(base + 8, vertexCount, true);
    this.view.setUint32(base + 12, instanceCount, true);
    this.view.setUint32(base + 16, firstVertex, true);
    this.view.setUint32(base + 20, firstInstance, true);
  }

  drawIndexed(
    indexCount: number,
    instanceCount: number,
    firstIndex: number,
    baseVertex: number,
    firstInstance: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DrawIndexed, AEROGPU_CMD_DRAW_INDEXED_SIZE);
    this.view.setUint32(base + 8, indexCount, true);
    this.view.setUint32(base + 12, instanceCount, true);
    this.view.setUint32(base + 16, firstIndex, true);
    this.view.setInt32(base + 20, baseVertex, true);
    this.view.setUint32(base + 24, firstInstance, true);
  }

  dispatch(groupCountX: number, groupCountY: number, groupCountZ: number): void {
    this.dispatchStageEx(groupCountX, groupCountY, groupCountZ);
  }

  /**
   * DISPATCH with an optional `stage_ex` selector encoded via the packet's `reserved0` field.
   *
   * This is primarily used for extended-stage compute passes (GS/HS/DS) in D3D11 command streams.
   * For legacy compute dispatches, prefer {@link dispatch} (or omit `stageEx`).
   */
  dispatchStageEx(
    groupCountX: number,
    groupCountY: number,
    groupCountZ: number,
    stageEx?: AerogpuShaderStageEx | null,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.Dispatch, AEROGPU_CMD_DISPATCH_SIZE);
    this.view.setUint32(base + 8, groupCountX, true);
    this.view.setUint32(base + 12, groupCountY, true);
    this.view.setUint32(base + 16, groupCountZ, true);
    const reserved0 = encodeStageExReserved0(AerogpuShaderStage.Compute, stageEx);
    this.view.setUint32(base + 20, reserved0, true);
  }

  /**
   * Stage-ex aware variant of {@link dispatch}.
   *
   * Encodes `stageEx` into `DISPATCH.reserved0` using the stage_ex ABI rules. The caller must not
   * pass `stageEx=0` (`AerogpuShaderStageEx.None`), which is reserved for legacy/default Compute
   * dispatches.
   */
  dispatchEx(stageEx: AerogpuShaderStageEx, groupCountX: number, groupCountY: number, groupCountZ: number): void {
    if ((stageEx >>> 0) === AerogpuShaderStageEx.None) {
      throw new Error("DISPATCH stageEx=0 is reserved for legacy/default; omit stageEx instead");
    }
    this.dispatchStageEx(groupCountX, groupCountY, groupCountZ, stageEx);
  }

  present(scanoutId: number, flags: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.Present, AEROGPU_CMD_PRESENT_SIZE);
    this.view.setUint32(base + 8, scanoutId, true);
    this.view.setUint32(base + 12, flags, true);
  }

  presentEx(scanoutId: number, flags: number, d3d9PresentFlags: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.PresentEx, AEROGPU_CMD_PRESENT_EX_SIZE);
    this.view.setUint32(base + 8, scanoutId, true);
    this.view.setUint32(base + 12, flags, true);
    this.view.setUint32(base + 16, d3d9PresentFlags, true);
  }

  exportSharedSurface(resourceHandle: AerogpuHandle, shareToken: bigint): void {
    const base = this.appendRaw(AerogpuCmdOpcode.ExportSharedSurface, AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE);
    this.view.setUint32(base + 8, resourceHandle, true);
    this.view.setBigUint64(base + 16, shareToken, true);
  }

  importSharedSurface(outResourceHandle: AerogpuHandle, shareToken: bigint): void {
    const base = this.appendRaw(AerogpuCmdOpcode.ImportSharedSurface, AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE);
    this.view.setUint32(base + 8, outResourceHandle, true);
    this.view.setBigUint64(base + 16, shareToken, true);
  }

  releaseSharedSurface(shareToken: bigint): void {
    const base = this.appendRaw(
      AerogpuCmdOpcode.ReleaseSharedSurface,
      AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE,
    );
    this.view.setBigUint64(base + 8, shareToken, true);
  }

  flush(): void {
    this.appendRaw(AerogpuCmdOpcode.Flush, AEROGPU_CMD_FLUSH_SIZE);
  }
}
