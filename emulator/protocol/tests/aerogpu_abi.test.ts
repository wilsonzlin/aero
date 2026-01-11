import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  AEROGPU_CMD_BIND_SHADERS_SIZE,
  AEROGPU_CMD_CLEAR_SIZE,
  AEROGPU_CMD_COPY_BUFFER_SIZE,
  AEROGPU_CMD_COPY_TEXTURE2D_SIZE,
  AEROGPU_CMD_CREATE_BUFFER_SIZE,
  AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE,
  AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE,
  AEROGPU_CMD_CREATE_TEXTURE2D_SIZE,
  AEROGPU_CMD_DESTROY_INPUT_LAYOUT_SIZE,
  AEROGPU_CMD_DESTROY_RESOURCE_SIZE,
  AEROGPU_CMD_DESTROY_SHADER_SIZE,
  AEROGPU_CMD_DRAW_INDEXED_SIZE,
  AEROGPU_CMD_DRAW_SIZE,
  AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_FLUSH_SIZE,
  AEROGPU_CMD_HDR_OFF_OPCODE,
  AEROGPU_CMD_HDR_OFF_SIZE_BYTES,
  AEROGPU_CMD_HDR_SIZE,
  AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_PRESENT_EX_SIZE,
  AEROGPU_CMD_PRESENT_SIZE,
  AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE,
  AEROGPU_CMD_SET_BLEND_STATE_SIZE,
  AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE,
  AEROGPU_CMD_SET_INDEX_BUFFER_SIZE,
  AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE,
  AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE,
  AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE,
  AEROGPU_CMD_SET_RENDER_STATE_SIZE,
  AEROGPU_CMD_SET_RENDER_TARGETS_SIZE,
  AEROGPU_CMD_SET_SAMPLER_STATE_SIZE,
  AEROGPU_CMD_SET_SCISSOR_SIZE,
  AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE,
  AEROGPU_CMD_SET_TEXTURE_SIZE,
  AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE,
  AEROGPU_CMD_SET_VIEWPORT_SIZE,
  AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION,
  AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS,
  AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC,
  AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AEROGPU_CMD_STREAM_MAGIC,
  AEROGPU_CMD_STREAM_FLAG_NONE,
  AEROGPU_CMD_UPLOAD_RESOURCE_SIZE,
  AEROGPU_CLEAR_COLOR,
  AEROGPU_CLEAR_DEPTH,
  AEROGPU_CLEAR_STENCIL,
  AEROGPU_COPY_FLAG_NONE,
  AEROGPU_COPY_FLAG_WRITEBACK_DST,
  AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_ELEMENT_COUNT,
  AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_MAGIC,
  AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_RESERVED0,
  AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_VERSION,
  AEROGPU_INPUT_LAYOUT_BLOB_HEADER_SIZE,
  AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
  AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_ALIGNED_BYTE_OFFSET,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_DXGI_FORMAT,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INPUT_SLOT,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INPUT_SLOT_CLASS,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INSTANCE_DATA_STEP_RATE,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_SEMANTIC_INDEX,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_SEMANTIC_NAME_HASH,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_SIZE,
  AEROGPU_MAX_RENDER_TARGETS,
  AEROGPU_PRESENT_FLAG_NONE,
  AEROGPU_PRESENT_FLAG_VSYNC,
  AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
  AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
  AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
  AEROGPU_RESOURCE_USAGE_NONE,
  AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
  AEROGPU_RESOURCE_USAGE_SCANOUT,
  AEROGPU_RESOURCE_USAGE_TEXTURE,
  AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
  AerogpuBlendFactor,
  AerogpuBlendOp,
  AerogpuCompareFunc,
  AerogpuCullMode,
  AerogpuFillMode,
  AerogpuCmdOpcode,
  AerogpuIndexFormat,
  AerogpuPrimitiveTopology,
  AerogpuShaderStage,
  decodeCmdHdr,
  decodeCmdStreamHeader,
} from "../aerogpu/aerogpu_cmd.ts";
import {
  AEROGPU_ABI_MAJOR,
  AEROGPU_ABI_MINOR,
  AEROGPU_ABI_VERSION_U32,
  AEROGPU_FEATURE_FENCE_PAGE,
  AEROGPU_FEATURE_TRANSFER,
  AEROGPU_FEATURE_VBLANK,
  AEROGPU_IRQ_FENCE,
  AEROGPU_MMIO_MAGIC,
  AEROGPU_MMIO_REG_DOORBELL,
  AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS,
  AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
  AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
  AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER,
  AEROGPU_PCI_DEVICE_ID,
  AEROGPU_PCI_BAR0_INDEX,
  AEROGPU_PCI_BAR0_SIZE_BYTES,
  AEROGPU_PCI_PROG_IF,
  AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE,
  AEROGPU_PCI_SUBSYSTEM_ID,
  AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
  AEROGPU_PCI_VENDOR_ID,
  AEROGPU_RING_CONTROL_ENABLE,
  AerogpuAbiError,
  parseAndValidateAbiVersionU32,
} from "../aerogpu/aerogpu_pci.ts";
import {
  AEROGPU_ALLOC_ENTRY_OFF_GPA,
  AEROGPU_ALLOC_ENTRY_OFF_SIZE_BYTES,
  AEROGPU_ALLOC_ENTRY_SIZE,
  AEROGPU_ALLOC_TABLE_HEADER_OFF_ABI_VERSION,
  AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_COUNT,
  AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_STRIDE_BYTES,
  AEROGPU_ALLOC_TABLE_HEADER_OFF_MAGIC,
  AEROGPU_ALLOC_TABLE_HEADER_OFF_SIZE_BYTES,
  AEROGPU_ALLOC_TABLE_HEADER_SIZE,
  AEROGPU_ALLOC_TABLE_MAGIC,
  AEROGPU_FENCE_PAGE_MAGIC,
  AEROGPU_FENCE_PAGE_OFF_COMPLETED_FENCE,
  AEROGPU_FENCE_PAGE_OFF_MAGIC,
  AEROGPU_FENCE_PAGE_SIZE,
  AEROGPU_RING_MAGIC,
  AEROGPU_RING_HEADER_OFF_ABI_VERSION,
  AEROGPU_RING_HEADER_OFF_ENTRY_COUNT,
  AEROGPU_RING_HEADER_OFF_ENTRY_STRIDE_BYTES,
  AEROGPU_RING_HEADER_OFF_HEAD,
  AEROGPU_RING_HEADER_OFF_MAGIC,
  AEROGPU_RING_HEADER_OFF_SIZE_BYTES,
  AEROGPU_RING_HEADER_OFF_TAIL,
  AEROGPU_RING_HEADER_SIZE,
  AEROGPU_SUBMIT_DESC_OFF_ALLOC_TABLE_GPA,
  AEROGPU_SUBMIT_DESC_OFF_CMD_GPA,
  AEROGPU_SUBMIT_DESC_OFF_SIGNAL_FENCE,
  AEROGPU_SUBMIT_DESC_SIZE,
  AEROGPU_SUBMIT_FLAG_NO_IRQ,
  AEROGPU_SUBMIT_FLAG_PRESENT,
  decodeAllocTableHeader,
  decodeRingHeader,
  decodeSubmitDesc,
  writeFencePageCompletedFence,
} from "../aerogpu/aerogpu_ring.ts";
import {
  AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE,
  AEROGPU_UMDPRIV_FEATURE_VBLANK,
  AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE,
  AEROGPU_UMDPRIV_FLAG_HAS_VBLANK,
  AEROGPU_UMDPRIV_FLAG_IS_LEGACY,
  AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP,
  AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU,
  AEROGPU_UMDPRIV_STRUCT_VERSION_V1,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_ABI_VERSION_U32,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_FEATURES,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_MMIO_MAGIC,
  AEROGPU_UMD_PRIVATE_V1_OFF_FLAGS,
  AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES,
  AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION,
  AEROGPU_UMD_PRIVATE_V1_SIZE,
} from "../aerogpu/aerogpu_umd_private.ts";

type AbiDump = {
  sizes: Map<string, number>;
  offsets: Map<string, number>;
  consts: Map<string, bigint>;
};

const testDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(testDir, "../../..");

function parseAbiDump(text: string): AbiDump {
  const sizes = new Map<string, number>();
  const offsets = new Map<string, number>();
  const consts = new Map<string, bigint>();

  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    const parts = trimmed.split(/\s+/);
    if (parts[0] === "SIZE") {
      sizes.set(parts[1], Number(parts[2]));
    } else if (parts[0] === "OFF") {
      offsets.set(`${parts[1]}.${parts[2]}`, Number(parts[3]));
    } else if (parts[0] === "CONST") {
      consts.set(parts[1], BigInt(parts[2]));
    } else {
      throw new Error(`Unknown ABI dump tag: ${parts[0]}`);
    }
  }

  return { sizes, offsets, consts };
}

let cachedAbi: AbiDump | null = null;
function abiDump(): AbiDump {
  if (cachedAbi) return cachedAbi;

  const cSrc = path.join(testDir, "aerogpu_abi_dump.c");

  const outPath = path.join(
    tmpdir(),
    `aerogpu_abi_dump_node_${process.pid}${process.platform === "win32" ? ".exe" : ""}`,
  );

  const compile = spawnSync("cc", ["-I", repoRoot, "-std=c11", "-o", outPath, cSrc], { encoding: "utf8" });
  assert.equal(compile.status, 0, `cc failed: ${compile.stderr}\n${compile.stdout}`);

  const run = spawnSync(outPath, [], { encoding: "utf8" });
  assert.equal(run.status, 0, `ABI dump helper failed: ${run.stderr}\n${run.stdout}`);

  cachedAbi = parseAbiDump(run.stdout);
  return cachedAbi;
}

function parseCcmdOpcodeConstNames(): string[] {
  const headerPath = path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_cmd.h");
  const text = fs.readFileSync(headerPath, "utf8");

  const start = text.indexOf("enum aerogpu_cmd_opcode");
  assert.notEqual(start, -1, "missing enum aerogpu_cmd_opcode in aerogpu_cmd.h");
  const afterStart = text.slice(start);

  const open = afterStart.indexOf("{");
  assert.notEqual(open, -1, "missing '{' for enum aerogpu_cmd_opcode");
  const afterOpen = afterStart.slice(open + 1);

  const close = afterOpen.indexOf("};");
  assert.notEqual(close, -1, "missing '};' for enum aerogpu_cmd_opcode");
  const body = afterOpen.slice(0, close);

  const names = new Set<string>();
  let idx = 0;
  for (;;) {
    const pos = body.indexOf("AEROGPU_CMD_", idx);
    if (pos === -1) break;
    let end = pos;
    while (end < body.length) {
      const ch = body.charCodeAt(end);
      const isAlphaNum = (ch >= 0x30 && ch <= 0x39) || (ch >= 0x41 && ch <= 0x5a);
      if (!isAlphaNum && ch !== 0x5f) break;
      end++;
    }
    names.add(body.slice(pos, end));
    idx = end;
  }

  return [...names].sort();
}

function upperSnakeToPascalCase(s: string): string {
  return s
    .split("_")
    .filter((part) => part.length > 0)
    .map((part) => part[0]!.toUpperCase() + part.slice(1).toLowerCase())
    .join("");
}

test("TypeScript layout matches C headers", () => {
  const abi = abiDump();

  const size = (name: string) => {
    const v = abi.sizes.get(name);
    assert.ok(v !== undefined, `missing SIZE for ${name}`);
    return v;
  };
  const off = (ty: string, field: string) => {
    const v = abi.offsets.get(`${ty}.${field}`);
    assert.ok(v !== undefined, `missing OFF for ${ty}.${field}`);
    return v;
  };
  const konst = (name: string) => {
    const v = abi.consts.get(name);
    assert.ok(v !== undefined, `missing CONST for ${name}`);
    return v;
  };

  // Sizes.
  assert.equal(size("aerogpu_cmd_stream_header"), AEROGPU_CMD_STREAM_HEADER_SIZE);
  assert.equal(size("aerogpu_cmd_hdr"), AEROGPU_CMD_HDR_SIZE);
  assert.equal(size("aerogpu_cmd_create_buffer"), AEROGPU_CMD_CREATE_BUFFER_SIZE);
  assert.equal(size("aerogpu_cmd_create_texture2d"), AEROGPU_CMD_CREATE_TEXTURE2D_SIZE);
  assert.equal(size("aerogpu_cmd_destroy_resource"), AEROGPU_CMD_DESTROY_RESOURCE_SIZE);
  assert.equal(size("aerogpu_cmd_resource_dirty_range"), AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE);
  assert.equal(size("aerogpu_cmd_upload_resource"), AEROGPU_CMD_UPLOAD_RESOURCE_SIZE);
  assert.equal(size("aerogpu_cmd_copy_buffer"), AEROGPU_CMD_COPY_BUFFER_SIZE);
  assert.equal(size("aerogpu_cmd_copy_texture2d"), AEROGPU_CMD_COPY_TEXTURE2D_SIZE);
  assert.equal(size("aerogpu_cmd_create_shader_dxbc"), AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE);
  assert.equal(size("aerogpu_cmd_destroy_shader"), AEROGPU_CMD_DESTROY_SHADER_SIZE);
  assert.equal(size("aerogpu_cmd_bind_shaders"), AEROGPU_CMD_BIND_SHADERS_SIZE);
  assert.equal(size("aerogpu_cmd_set_shader_constants_f"), AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE);
  assert.equal(size("aerogpu_input_layout_blob_header"), AEROGPU_INPUT_LAYOUT_BLOB_HEADER_SIZE);
  assert.equal(size("aerogpu_input_layout_element_dxgi"), AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_SIZE);
  assert.equal(size("aerogpu_cmd_create_input_layout"), AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE);
  assert.equal(size("aerogpu_cmd_destroy_input_layout"), AEROGPU_CMD_DESTROY_INPUT_LAYOUT_SIZE);
  assert.equal(size("aerogpu_cmd_set_input_layout"), AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE);
  assert.equal(size("aerogpu_cmd_set_blend_state"), AEROGPU_CMD_SET_BLEND_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_depth_stencil_state"), AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_rasterizer_state"), AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_render_targets"), AEROGPU_CMD_SET_RENDER_TARGETS_SIZE);
  assert.equal(size("aerogpu_cmd_set_viewport"), AEROGPU_CMD_SET_VIEWPORT_SIZE);
  assert.equal(size("aerogpu_cmd_set_scissor"), AEROGPU_CMD_SET_SCISSOR_SIZE);
  assert.equal(size("aerogpu_cmd_set_vertex_buffers"), AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE);
  assert.equal(size("aerogpu_cmd_set_index_buffer"), AEROGPU_CMD_SET_INDEX_BUFFER_SIZE);
  assert.equal(size("aerogpu_cmd_set_primitive_topology"), AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE);
  assert.equal(size("aerogpu_cmd_set_texture"), AEROGPU_CMD_SET_TEXTURE_SIZE);
  assert.equal(size("aerogpu_cmd_set_sampler_state"), AEROGPU_CMD_SET_SAMPLER_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_render_state"), AEROGPU_CMD_SET_RENDER_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_clear"), AEROGPU_CMD_CLEAR_SIZE);
  assert.equal(size("aerogpu_cmd_draw"), AEROGPU_CMD_DRAW_SIZE);
  assert.equal(size("aerogpu_cmd_draw_indexed"), AEROGPU_CMD_DRAW_INDEXED_SIZE);
  assert.equal(size("aerogpu_cmd_present"), AEROGPU_CMD_PRESENT_SIZE);
  assert.equal(size("aerogpu_cmd_present_ex"), AEROGPU_CMD_PRESENT_EX_SIZE);
  assert.equal(size("aerogpu_cmd_export_shared_surface"), AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE);
  assert.equal(size("aerogpu_cmd_import_shared_surface"), AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE);
  assert.equal(size("aerogpu_cmd_flush"), AEROGPU_CMD_FLUSH_SIZE);

  assert.equal(size("aerogpu_alloc_table_header"), AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  assert.equal(size("aerogpu_alloc_entry"), AEROGPU_ALLOC_ENTRY_SIZE);

  assert.equal(size("aerogpu_submit_desc"), AEROGPU_SUBMIT_DESC_SIZE);
  assert.equal(size("aerogpu_ring_header"), AEROGPU_RING_HEADER_SIZE);
  assert.equal(size("aerogpu_fence_page"), AEROGPU_FENCE_PAGE_SIZE);
  assert.equal(size("aerogpu_umd_private_v1"), AEROGPU_UMD_PRIVATE_V1_SIZE);
  assert.equal(size("aerogpu_wddm_alloc_priv"), 40);

  // Escape ABI (driver-private; stable across x86/x64).
  assert.equal(size("aerogpu_escape_header"), 16);
  assert.equal(size("aerogpu_escape_query_device_out"), 24);
  assert.equal(size("aerogpu_escape_query_device_v2_out"), 48);
  assert.equal(size("aerogpu_escape_query_fence_out"), 32);
  assert.equal(size("aerogpu_escape_dump_ring_inout"), 40 + 32 * 24);
  assert.equal(size("aerogpu_escape_dump_ring_v2_inout"), 52 + 32 * 40);
  assert.equal(size("aerogpu_escape_selftest_inout"), 32);
  assert.equal(size("aerogpu_escape_query_vblank_out"), 56);

  // Key offsets.
  assert.equal(off("aerogpu_cmd_stream_header", "magic"), AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC);
  assert.equal(off("aerogpu_cmd_stream_header", "abi_version"), AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION);
  assert.equal(off("aerogpu_cmd_stream_header", "size_bytes"), AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES);
  assert.equal(off("aerogpu_cmd_stream_header", "flags"), AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS);

  assert.equal(off("aerogpu_cmd_hdr", "opcode"), AEROGPU_CMD_HDR_OFF_OPCODE);
  assert.equal(off("aerogpu_cmd_hdr", "size_bytes"), AEROGPU_CMD_HDR_OFF_SIZE_BYTES);

  assert.equal(off("aerogpu_input_layout_blob_header", "magic"), AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_MAGIC);
  assert.equal(off("aerogpu_input_layout_blob_header", "version"), AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_VERSION);
  assert.equal(
    off("aerogpu_input_layout_blob_header", "element_count"),
    AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_ELEMENT_COUNT,
  );
  assert.equal(
    off("aerogpu_input_layout_blob_header", "reserved0"),
    AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_RESERVED0,
  );

  assert.equal(
    off("aerogpu_input_layout_element_dxgi", "semantic_name_hash"),
    AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_SEMANTIC_NAME_HASH,
  );
  assert.equal(
    off("aerogpu_input_layout_element_dxgi", "semantic_index"),
    AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_SEMANTIC_INDEX,
  );
  assert.equal(off("aerogpu_input_layout_element_dxgi", "dxgi_format"), AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_DXGI_FORMAT);
  assert.equal(off("aerogpu_input_layout_element_dxgi", "input_slot"), AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INPUT_SLOT);
  assert.equal(
    off("aerogpu_input_layout_element_dxgi", "aligned_byte_offset"),
    AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_ALIGNED_BYTE_OFFSET,
  );
  assert.equal(
    off("aerogpu_input_layout_element_dxgi", "input_slot_class"),
    AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INPUT_SLOT_CLASS,
  );
  assert.equal(
    off("aerogpu_input_layout_element_dxgi", "instance_data_step_rate"),
    AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INSTANCE_DATA_STEP_RATE,
  );

  assert.equal(off("aerogpu_alloc_table_header", "magic"), AEROGPU_ALLOC_TABLE_HEADER_OFF_MAGIC);
  assert.equal(
    off("aerogpu_alloc_table_header", "abi_version"),
    AEROGPU_ALLOC_TABLE_HEADER_OFF_ABI_VERSION,
  );
  assert.equal(
    off("aerogpu_alloc_table_header", "size_bytes"),
    AEROGPU_ALLOC_TABLE_HEADER_OFF_SIZE_BYTES,
  );
  assert.equal(
    off("aerogpu_alloc_table_header", "entry_count"),
    AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_COUNT,
  );
  assert.equal(
    off("aerogpu_alloc_table_header", "entry_stride_bytes"),
    AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_STRIDE_BYTES,
  );

  assert.equal(off("aerogpu_alloc_entry", "gpa"), AEROGPU_ALLOC_ENTRY_OFF_GPA);
  assert.equal(off("aerogpu_alloc_entry", "size_bytes"), AEROGPU_ALLOC_ENTRY_OFF_SIZE_BYTES);

  assert.equal(off("aerogpu_submit_desc", "cmd_gpa"), AEROGPU_SUBMIT_DESC_OFF_CMD_GPA);
  assert.equal(off("aerogpu_submit_desc", "alloc_table_gpa"), AEROGPU_SUBMIT_DESC_OFF_ALLOC_TABLE_GPA);
  assert.equal(off("aerogpu_submit_desc", "signal_fence"), AEROGPU_SUBMIT_DESC_OFF_SIGNAL_FENCE);

  assert.equal(off("aerogpu_ring_header", "head"), AEROGPU_RING_HEADER_OFF_HEAD);
  assert.equal(off("aerogpu_ring_header", "tail"), AEROGPU_RING_HEADER_OFF_TAIL);

  assert.equal(off("aerogpu_umd_private_v1", "size_bytes"), AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES);
  assert.equal(
    off("aerogpu_umd_private_v1", "struct_version"),
    AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION,
  );
  assert.equal(
    off("aerogpu_umd_private_v1", "device_mmio_magic"),
    AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_MMIO_MAGIC,
  );
  assert.equal(
    off("aerogpu_umd_private_v1", "device_abi_version_u32"),
    AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_ABI_VERSION_U32,
  );
  assert.equal(
    off("aerogpu_umd_private_v1", "device_features"),
    AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_FEATURES,
  );
  assert.equal(off("aerogpu_umd_private_v1", "flags"), AEROGPU_UMD_PRIVATE_V1_OFF_FLAGS);

  // Variable-length packets (must remain stable for parsing).
  assert.equal(off("aerogpu_cmd_create_shader_dxbc", "dxbc_size_bytes"), 16);
  assert.equal(off("aerogpu_cmd_set_shader_constants_f", "vec4_count"), 16);
  assert.equal(off("aerogpu_cmd_create_input_layout", "blob_size_bytes"), 12);
  assert.equal(off("aerogpu_cmd_set_vertex_buffers", "buffer_count"), 12);
  assert.equal(off("aerogpu_cmd_upload_resource", "offset_bytes"), 16);
  assert.equal(off("aerogpu_cmd_upload_resource", "size_bytes"), 24);

  // Fixed-layout packet fields (helps catch accidental field reordering).
  assert.equal(off("aerogpu_cmd_upload_resource", "resource_handle"), 8);
  assert.equal(off("aerogpu_cmd_set_shader_constants_f", "stage"), 8);
  assert.equal(off("aerogpu_cmd_set_shader_constants_f", "start_register"), 12);
  assert.equal(off("aerogpu_cmd_create_input_layout", "input_layout_handle"), 8);
  assert.equal(off("aerogpu_cmd_destroy_input_layout", "input_layout_handle"), 8);
  assert.equal(off("aerogpu_cmd_set_input_layout", "input_layout_handle"), 8);
  assert.equal(off("aerogpu_cmd_set_primitive_topology", "topology"), 8);
  assert.equal(off("aerogpu_cmd_set_texture", "shader_stage"), 8);
  assert.equal(off("aerogpu_cmd_set_texture", "slot"), 12);
  assert.equal(off("aerogpu_cmd_set_texture", "texture"), 16);
  assert.equal(off("aerogpu_cmd_set_sampler_state", "shader_stage"), 8);
  assert.equal(off("aerogpu_cmd_set_sampler_state", "slot"), 12);
  assert.equal(off("aerogpu_cmd_set_sampler_state", "state"), 16);
  assert.equal(off("aerogpu_cmd_set_sampler_state", "value"), 20);
  assert.equal(off("aerogpu_cmd_set_render_state", "state"), 8);
  assert.equal(off("aerogpu_cmd_set_render_state", "value"), 12);

  assert.equal(off("aerogpu_wddm_alloc_priv", "magic"), 0);
  assert.equal(off("aerogpu_wddm_alloc_priv", "version"), 4);
  assert.equal(off("aerogpu_wddm_alloc_priv", "alloc_id"), 8);
  assert.equal(off("aerogpu_wddm_alloc_priv", "flags"), 12);
  assert.equal(off("aerogpu_wddm_alloc_priv", "share_token"), 16);
  assert.equal(off("aerogpu_wddm_alloc_priv", "size_bytes"), 24);
  assert.equal(off("aerogpu_wddm_alloc_priv", "reserved0"), 32);

  assert.equal(off("aerogpu_escape_header", "version"), 0);
  assert.equal(off("aerogpu_escape_header", "op"), 4);
  assert.equal(off("aerogpu_escape_header", "size"), 8);
  assert.equal(off("aerogpu_escape_header", "reserved0"), 12);

  assert.equal(off("aerogpu_escape_query_device_v2_out", "detected_mmio_magic"), 16);
  assert.equal(off("aerogpu_escape_query_device_v2_out", "abi_version_u32"), 20);
  assert.equal(off("aerogpu_escape_query_device_v2_out", "features_lo"), 24);
  assert.equal(off("aerogpu_escape_query_device_v2_out", "features_hi"), 32);
  assert.equal(off("aerogpu_escape_query_device_v2_out", "reserved0"), 40);

  assert.equal(off("aerogpu_escape_query_vblank_out", "vidpn_source_id"), 16);
  assert.equal(off("aerogpu_escape_query_vblank_out", "irq_enable"), 20);
  assert.equal(off("aerogpu_escape_query_vblank_out", "irq_status"), 24);
  assert.equal(off("aerogpu_escape_query_vblank_out", "flags"), 28);
  assert.equal(off("aerogpu_escape_query_vblank_out", "vblank_seq"), 32);
  assert.equal(off("aerogpu_escape_query_vblank_out", "last_vblank_time_ns"), 40);
  assert.equal(off("aerogpu_escape_query_vblank_out", "vblank_period_ns"), 48);

  // Constants.
  assert.equal(konst("AEROGPU_ABI_MAJOR"), BigInt(AEROGPU_ABI_MAJOR));
  assert.equal(konst("AEROGPU_ABI_MINOR"), BigInt(AEROGPU_ABI_MINOR));
  assert.equal(konst("AEROGPU_ABI_VERSION_U32"), BigInt(AEROGPU_ABI_VERSION_U32));
  assert.equal(konst("AEROGPU_PCI_VENDOR_ID"), BigInt(AEROGPU_PCI_VENDOR_ID));
  assert.equal(konst("AEROGPU_PCI_DEVICE_ID"), BigInt(AEROGPU_PCI_DEVICE_ID));
  assert.equal(konst("AEROGPU_PCI_SUBSYSTEM_VENDOR_ID"), BigInt(AEROGPU_PCI_SUBSYSTEM_VENDOR_ID));
  assert.equal(konst("AEROGPU_PCI_SUBSYSTEM_ID"), BigInt(AEROGPU_PCI_SUBSYSTEM_ID));
  assert.equal(
    konst("AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER"),
    BigInt(AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER),
  );
  assert.equal(konst("AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE"), BigInt(AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE));
  assert.equal(konst("AEROGPU_PCI_PROG_IF"), BigInt(AEROGPU_PCI_PROG_IF));
  assert.equal(konst("AEROGPU_PCI_BAR0_INDEX"), BigInt(AEROGPU_PCI_BAR0_INDEX));
  assert.equal(konst("AEROGPU_PCI_BAR0_SIZE_BYTES"), BigInt(AEROGPU_PCI_BAR0_SIZE_BYTES));

  assert.equal(konst("AEROGPU_MMIO_MAGIC"), BigInt(AEROGPU_MMIO_MAGIC));
  assert.equal(konst("AEROGPU_MMIO_REG_DOORBELL"), BigInt(AEROGPU_MMIO_REG_DOORBELL));
  assert.equal(konst("AEROGPU_FEATURE_FENCE_PAGE"), AEROGPU_FEATURE_FENCE_PAGE);
  assert.equal(konst("AEROGPU_FEATURE_VBLANK"), AEROGPU_FEATURE_VBLANK);
  assert.equal(konst("AEROGPU_FEATURE_TRANSFER"), AEROGPU_FEATURE_TRANSFER);
  assert.equal(konst("AEROGPU_RING_CONTROL_ENABLE"), BigInt(AEROGPU_RING_CONTROL_ENABLE));
  assert.equal(konst("AEROGPU_IRQ_FENCE"), BigInt(AEROGPU_IRQ_FENCE));
  assert.equal(
    konst("AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO"),
    BigInt(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO),
  );
  assert.equal(
    konst("AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO"),
    BigInt(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO),
  );
  assert.equal(
    konst("AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS"),
    BigInt(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS),
  );

  assert.equal(konst("AEROGPU_CMD_STREAM_MAGIC"), BigInt(AEROGPU_CMD_STREAM_MAGIC));
  assert.equal(konst("AEROGPU_CMD_STREAM_FLAG_NONE"), BigInt(AEROGPU_CMD_STREAM_FLAG_NONE));

  assert.equal(konst("AEROGPU_RESOURCE_USAGE_NONE"), BigInt(AEROGPU_RESOURCE_USAGE_NONE));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER"), BigInt(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_INDEX_BUFFER"), BigInt(AEROGPU_RESOURCE_USAGE_INDEX_BUFFER));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER"), BigInt(AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_TEXTURE"), BigInt(AEROGPU_RESOURCE_USAGE_TEXTURE));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_RENDER_TARGET"), BigInt(AEROGPU_RESOURCE_USAGE_RENDER_TARGET));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL"), BigInt(AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_SCANOUT"), BigInt(AEROGPU_RESOURCE_USAGE_SCANOUT));

  assert.equal(konst("AEROGPU_COPY_FLAG_NONE"), BigInt(AEROGPU_COPY_FLAG_NONE));
  assert.equal(konst("AEROGPU_COPY_FLAG_WRITEBACK_DST"), BigInt(AEROGPU_COPY_FLAG_WRITEBACK_DST));

  assert.equal(konst("AEROGPU_MAX_RENDER_TARGETS"), BigInt(AEROGPU_MAX_RENDER_TARGETS));
  const cOpcodeConsts = parseCcmdOpcodeConstNames();
  const expectedTsKeys = cOpcodeConsts.map((cName) => upperSnakeToPascalCase(cName.replace(/^AEROGPU_CMD_/, "")));

  assert.deepEqual(new Set(Object.keys(AerogpuCmdOpcode)), new Set(expectedTsKeys), "opcode key set");
  for (const cName of cOpcodeConsts) {
    const key = upperSnakeToPascalCase(cName.replace(/^AEROGPU_CMD_/, ""));
    const value = konst(cName);
    const actual = (AerogpuCmdOpcode as Record<string, number>)[key];
    assert.ok(actual !== undefined, `missing TS opcode binding for ${cName}`);
    assert.equal(BigInt(actual), value, `opcode value for ${cName}`);
  }

  assert.equal(konst("AEROGPU_CLEAR_COLOR"), BigInt(AEROGPU_CLEAR_COLOR));
  assert.equal(konst("AEROGPU_CLEAR_DEPTH"), BigInt(AEROGPU_CLEAR_DEPTH));
  assert.equal(konst("AEROGPU_CLEAR_STENCIL"), BigInt(AEROGPU_CLEAR_STENCIL));

  assert.equal(konst("AEROGPU_PRESENT_FLAG_NONE"), BigInt(AEROGPU_PRESENT_FLAG_NONE));
  assert.equal(konst("AEROGPU_PRESENT_FLAG_VSYNC"), BigInt(AEROGPU_PRESENT_FLAG_VSYNC));

  assert.equal(konst("AEROGPU_BLEND_ZERO"), BigInt(AerogpuBlendFactor.Zero));
  assert.equal(konst("AEROGPU_BLEND_ONE"), BigInt(AerogpuBlendFactor.One));
  assert.equal(konst("AEROGPU_BLEND_SRC_ALPHA"), BigInt(AerogpuBlendFactor.SrcAlpha));
  assert.equal(konst("AEROGPU_BLEND_INV_SRC_ALPHA"), BigInt(AerogpuBlendFactor.InvSrcAlpha));
  assert.equal(konst("AEROGPU_BLEND_DEST_ALPHA"), BigInt(AerogpuBlendFactor.DestAlpha));
  assert.equal(konst("AEROGPU_BLEND_INV_DEST_ALPHA"), BigInt(AerogpuBlendFactor.InvDestAlpha));

  assert.equal(konst("AEROGPU_BLEND_OP_ADD"), BigInt(AerogpuBlendOp.Add));
  assert.equal(konst("AEROGPU_BLEND_OP_SUBTRACT"), BigInt(AerogpuBlendOp.Subtract));
  assert.equal(konst("AEROGPU_BLEND_OP_REV_SUBTRACT"), BigInt(AerogpuBlendOp.RevSubtract));
  assert.equal(konst("AEROGPU_BLEND_OP_MIN"), BigInt(AerogpuBlendOp.Min));
  assert.equal(konst("AEROGPU_BLEND_OP_MAX"), BigInt(AerogpuBlendOp.Max));

  assert.equal(konst("AEROGPU_COMPARE_NEVER"), BigInt(AerogpuCompareFunc.Never));
  assert.equal(konst("AEROGPU_COMPARE_LESS"), BigInt(AerogpuCompareFunc.Less));
  assert.equal(konst("AEROGPU_COMPARE_EQUAL"), BigInt(AerogpuCompareFunc.Equal));
  assert.equal(konst("AEROGPU_COMPARE_LESS_EQUAL"), BigInt(AerogpuCompareFunc.LessEqual));
  assert.equal(konst("AEROGPU_COMPARE_GREATER"), BigInt(AerogpuCompareFunc.Greater));
  assert.equal(konst("AEROGPU_COMPARE_NOT_EQUAL"), BigInt(AerogpuCompareFunc.NotEqual));
  assert.equal(konst("AEROGPU_COMPARE_GREATER_EQUAL"), BigInt(AerogpuCompareFunc.GreaterEqual));
  assert.equal(konst("AEROGPU_COMPARE_ALWAYS"), BigInt(AerogpuCompareFunc.Always));

  assert.equal(konst("AEROGPU_FILL_SOLID"), BigInt(AerogpuFillMode.Solid));
  assert.equal(konst("AEROGPU_FILL_WIREFRAME"), BigInt(AerogpuFillMode.Wireframe));

  assert.equal(konst("AEROGPU_CULL_NONE"), BigInt(AerogpuCullMode.None));
  assert.equal(konst("AEROGPU_CULL_FRONT"), BigInt(AerogpuCullMode.Front));
  assert.equal(konst("AEROGPU_CULL_BACK"), BigInt(AerogpuCullMode.Back));

  assert.equal(konst("AEROGPU_INPUT_LAYOUT_BLOB_MAGIC"), BigInt(AEROGPU_INPUT_LAYOUT_BLOB_MAGIC));
  assert.equal(konst("AEROGPU_INPUT_LAYOUT_BLOB_VERSION"), BigInt(AEROGPU_INPUT_LAYOUT_BLOB_VERSION));

  assert.equal(konst("AEROGPU_SHADER_STAGE_VERTEX"), BigInt(AerogpuShaderStage.Vertex));
  assert.equal(konst("AEROGPU_SHADER_STAGE_PIXEL"), BigInt(AerogpuShaderStage.Pixel));
  assert.equal(konst("AEROGPU_SHADER_STAGE_COMPUTE"), BigInt(AerogpuShaderStage.Compute));

  assert.equal(konst("AEROGPU_INDEX_FORMAT_UINT16"), BigInt(AerogpuIndexFormat.Uint16));
  assert.equal(konst("AEROGPU_INDEX_FORMAT_UINT32"), BigInt(AerogpuIndexFormat.Uint32));

  assert.equal(konst("AEROGPU_TOPOLOGY_POINTLIST"), BigInt(AerogpuPrimitiveTopology.PointList));
  assert.equal(konst("AEROGPU_TOPOLOGY_LINELIST"), BigInt(AerogpuPrimitiveTopology.LineList));
  assert.equal(konst("AEROGPU_TOPOLOGY_LINESTRIP"), BigInt(AerogpuPrimitiveTopology.LineStrip));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLELIST"), BigInt(AerogpuPrimitiveTopology.TriangleList));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLESTRIP"), BigInt(AerogpuPrimitiveTopology.TriangleStrip));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLEFAN"), BigInt(AerogpuPrimitiveTopology.TriangleFan));

  assert.equal(konst("AEROGPU_SUBMIT_FLAG_PRESENT"), BigInt(AEROGPU_SUBMIT_FLAG_PRESENT));
  assert.equal(konst("AEROGPU_SUBMIT_FLAG_NO_IRQ"), BigInt(AEROGPU_SUBMIT_FLAG_NO_IRQ));

  assert.equal(
    konst("AEROGPU_UMDPRIV_STRUCT_VERSION_V1"),
    BigInt(AEROGPU_UMDPRIV_STRUCT_VERSION_V1),
  );
  assert.equal(
    konst("AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP"),
    BigInt(AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP),
  );
  assert.equal(
    konst("AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU"),
    BigInt(AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU),
  );
  assert.equal(konst("AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE"), AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE);
  assert.equal(konst("AEROGPU_UMDPRIV_FEATURE_VBLANK"), AEROGPU_UMDPRIV_FEATURE_VBLANK);
  assert.equal(konst("AEROGPU_UMDPRIV_FLAG_IS_LEGACY"), BigInt(AEROGPU_UMDPRIV_FLAG_IS_LEGACY));
  assert.equal(konst("AEROGPU_UMDPRIV_FLAG_HAS_VBLANK"), BigInt(AEROGPU_UMDPRIV_FLAG_HAS_VBLANK));
  assert.equal(
    konst("AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE"),
    BigInt(AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE),
  );

  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_MAGIC"), 0x414c4c4fn);
  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_VERSION"), 1n);
  assert.equal(konst("AEROGPU_WDDM_ALLOC_ID_UMD_MAX"), 0x7fffffffn);
  assert.equal(konst("AEROGPU_WDDM_ALLOC_ID_KMD_MIN"), 0x80000000n);
  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED"), 1n);

  assert.equal(konst("AEROGPU_ESCAPE_VERSION"), 1n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_DEVICE"), 1n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2"), 7n);

  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_FENCE"), 2n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_DUMP_RING"), 3n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_SELFTEST"), 4n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_VBLANK"), 5n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_DUMP_RING_V2"), 6n);

  assert.equal(konst("AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN"), 0n);
  assert.equal(konst("AEROGPU_DBGCTL_RING_FORMAT_LEGACY"), 1n);
  assert.equal(konst("AEROGPU_DBGCTL_RING_FORMAT_AGPU"), 2n);

  assert.equal(konst("AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID"), 1n << 31n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED"), 1n);
});

test("decodeAllocTableHeader accepts unknown minor versions and extended strides", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_MAGIC, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ABI_VERSION, (AEROGPU_ABI_MAJOR << 16) | 999, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_SIZE_BYTES, 24 + 2 * 64, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_COUNT, 2, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_STRIDE_BYTES, 64, true);

  const hdr = decodeAllocTableHeader(view, 0, 24 + 2 * 64);
  assert.equal(hdr.entryCount, 2);
  assert.equal(hdr.entryStrideBytes, 64);
});

test("decodeAllocTableHeader rejects too-small strides", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_MAGIC, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ABI_VERSION, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_SIZE_BYTES, 24 + 2 * 16, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_COUNT, 2, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_STRIDE_BYTES, 16, true);

  assert.throws(() => decodeAllocTableHeader(view, 0), /entry_stride_bytes too small/);
});

test("decodeAllocTableHeader rejects size_bytes that cannot fit the declared layout", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_MAGIC, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ABI_VERSION, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_SIZE_BYTES, 24, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_COUNT, 2, true);
  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_STRIDE_BYTES, 32, true);

  assert.throws(() => decodeAllocTableHeader(view, 0), /size_bytes too small for layout/);
});

test("decodeSubmitDesc decodes the expected byte layout", () => {
  const buf = new ArrayBuffer(AEROGPU_SUBMIT_DESC_SIZE);
  const view = new DataView(buf);

  view.setUint32(0, AEROGPU_SUBMIT_DESC_SIZE, true);
  view.setUint32(4, AEROGPU_SUBMIT_FLAG_PRESENT | AEROGPU_SUBMIT_FLAG_NO_IRQ, true);
  view.setUint32(8, 0x11111111, true);
  view.setUint32(12, 0, true);
  view.setBigUint64(AEROGPU_SUBMIT_DESC_OFF_CMD_GPA, 0x1020304050607080n, true);
  view.setUint32(24, 0x11223344, true);
  view.setBigUint64(AEROGPU_SUBMIT_DESC_OFF_ALLOC_TABLE_GPA, 0xAABBCCDDEEFF0011n, true);
  view.setUint32(40, 0x55667788, true);
  view.setBigUint64(AEROGPU_SUBMIT_DESC_OFF_SIGNAL_FENCE, 0x0102030405060708n, true);

  const desc = decodeSubmitDesc(view, 0);
  assert.equal(desc.flags, AEROGPU_SUBMIT_FLAG_PRESENT | AEROGPU_SUBMIT_FLAG_NO_IRQ);
  assert.equal(desc.cmdGpa, 0x1020304050607080n);
  assert.equal(desc.cmdSizeBytes, 0x11223344);
  assert.equal(desc.allocTableGpa, 0xAABBCCDDEEFF0011n);
  assert.equal(desc.allocTableSizeBytes, 0x55667788);
  assert.equal(desc.signalFence, 0x0102030405060708n);
});

test("decodeSubmitDesc accepts extended submit descriptors", () => {
  const buf = new ArrayBuffer(128);
  const view = new DataView(buf);

  view.setUint32(0, 128, true);
  view.setUint32(4, AEROGPU_SUBMIT_FLAG_PRESENT, true);
  view.setBigUint64(AEROGPU_SUBMIT_DESC_OFF_SIGNAL_FENCE, 123n, true);

  const desc = decodeSubmitDesc(view, 0, 128);
  assert.equal(desc.descSizeBytes, 128);
  assert.equal(desc.flags, AEROGPU_SUBMIT_FLAG_PRESENT);
  assert.equal(desc.signalFence, 123n);
});

test("decodeSubmitDesc rejects too-small submit descriptors", () => {
  const buf = new ArrayBuffer(AEROGPU_SUBMIT_DESC_SIZE);
  const view = new DataView(buf);
  view.setUint32(0, 32, true);
  assert.throws(() => decodeSubmitDesc(view, 0), /too small/);
});

test("decodeSubmitDesc rejects submit descriptors that exceed the provided max size", () => {
  const buf = new ArrayBuffer(AEROGPU_SUBMIT_DESC_SIZE);
  const view = new DataView(buf);
  view.setUint32(0, 128, true);
  assert.throws(() => decodeSubmitDesc(view, 0, AEROGPU_SUBMIT_DESC_SIZE), /exceeds max size/);
});

test("decodeRingHeader accepts unknown minor versions and extended strides", () => {
  const buf = new ArrayBuffer(AEROGPU_RING_HEADER_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_RING_HEADER_OFF_MAGIC, AEROGPU_RING_MAGIC, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ABI_VERSION, (AEROGPU_ABI_MAJOR << 16) | 999, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_COUNT, 8, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_STRIDE_BYTES, 128, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_SIZE_BYTES, 64 + 8 * 128, true);

  const hdr = decodeRingHeader(view, 0);
  assert.equal(hdr.entryCount, 8);
  assert.equal(hdr.entryStrideBytes, 128);
  assert.equal(hdr.abiVersion, (AEROGPU_ABI_MAJOR << 16) | 999);
});

test("decodeRingHeader rejects non-power-of-two entry_count", () => {
  const buf = new ArrayBuffer(AEROGPU_RING_HEADER_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_RING_HEADER_OFF_MAGIC, AEROGPU_RING_MAGIC, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ABI_VERSION, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_COUNT, 3, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_STRIDE_BYTES, 64, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_SIZE_BYTES, 64 + 3 * 64, true);

  assert.throws(() => decodeRingHeader(view, 0), /power-of-two/);
});

test("decodeRingHeader rejects too-small entry_stride_bytes", () => {
  const buf = new ArrayBuffer(AEROGPU_RING_HEADER_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_RING_HEADER_OFF_MAGIC, AEROGPU_RING_MAGIC, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ABI_VERSION, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_COUNT, 8, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_STRIDE_BYTES, 32, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_SIZE_BYTES, 64 + 8 * 32, true);

  assert.throws(() => decodeRingHeader(view, 0), /entry_stride_bytes too small/);
});

test("decodeRingHeader rejects rings where size_bytes cannot fit the declared layout", () => {
  const buf = new ArrayBuffer(AEROGPU_RING_HEADER_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_RING_HEADER_OFF_MAGIC, AEROGPU_RING_MAGIC, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ABI_VERSION, AEROGPU_ABI_VERSION_U32, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_COUNT, 8, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_STRIDE_BYTES, 64, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_SIZE_BYTES, 64, true); // Too small for 8 entries.

  assert.throws(() => decodeRingHeader(view, 0), /size_bytes too small for layout/);
});

test("writeFencePageCompletedFence updates the expected bytes", () => {
  const buf = new ArrayBuffer(AEROGPU_FENCE_PAGE_SIZE);
  const view = new DataView(buf);

  // Initialize header (driver-owned).
  view.setUint32(AEROGPU_FENCE_PAGE_OFF_MAGIC, AEROGPU_FENCE_PAGE_MAGIC, true);
  view.setUint32(4, AEROGPU_ABI_VERSION_U32, true);

  writeFencePageCompletedFence(view, 0, 0x0102030405060708n);
  assert.equal(view.getBigUint64(AEROGPU_FENCE_PAGE_OFF_COMPLETED_FENCE, true), 0x0102030405060708n);
});

test("decodeCmdStreamHeader rejects unknown major versions", () => {
  const buf = new ArrayBuffer(AEROGPU_CMD_STREAM_HEADER_SIZE);
  const view = new DataView(buf);
  view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC, AEROGPU_CMD_STREAM_MAGIC, true);
  view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION, ((AEROGPU_ABI_MAJOR + 1) << 16) | AEROGPU_ABI_MINOR, true);
  view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, AEROGPU_CMD_STREAM_HEADER_SIZE, true);
  view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS, 0, true);
  assert.throws(() => decodeCmdStreamHeader(view, 0), /Unsupported major/);
});

test("decodeCmdHdr enforces size_bytes invariants", () => {
  const buf = new ArrayBuffer(AEROGPU_CMD_HDR_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_CMD_HDR_OFF_OPCODE, 0xffffffff, true);

  // Too small.
  view.setUint32(AEROGPU_CMD_HDR_OFF_SIZE_BYTES, 4, true);
  assert.throws(() => decodeCmdHdr(view, 0), /too small/);

  // Not 4-byte aligned.
  view.setUint32(AEROGPU_CMD_HDR_OFF_SIZE_BYTES, 10, true);
  assert.throws(() => decodeCmdHdr(view, 0), /aligned/);

  // Unknown opcode is OK as long as size is valid.
  view.setUint32(AEROGPU_CMD_HDR_OFF_SIZE_BYTES, AEROGPU_CMD_HDR_SIZE, true);
  const hdr = decodeCmdHdr(view, 0);
  assert.equal(hdr.opcode, 0xffffffff);
  assert.equal(hdr.sizeBytes, AEROGPU_CMD_HDR_SIZE);
});
