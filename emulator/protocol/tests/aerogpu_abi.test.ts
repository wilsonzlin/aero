import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  AEROGPU_CMD_BIND_SHADERS_SIZE,
  AEROGPU_CMD_CLEAR_SIZE,
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
  AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE,
  AEROGPU_CMD_SET_BLEND_STATE_SIZE,
  AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE,
  AEROGPU_CMD_SET_INDEX_BUFFER_SIZE,
  AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE,
  AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE,
  AEROGPU_CMD_SET_RENDER_TARGETS_SIZE,
  AEROGPU_CMD_SET_RENDER_STATE_SIZE,
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
  AEROGPU_CMD_UPLOAD_RESOURCE_SIZE,
  AEROGPU_INPUT_LAYOUT_BLOB_HEADER_SIZE,
  AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
  AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
  AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_SIZE,
  AerogpuCmdOpcode,
  AerogpuPrimitiveTopology,
  decodeCmdStreamHeader,
} from "../aerogpu/aerogpu_cmd.ts";
import {
  AEROGPU_ABI_MAJOR,
  AEROGPU_ABI_MINOR,
  AEROGPU_ABI_VERSION_U32,
  AEROGPU_FEATURE_FENCE_PAGE,
  AEROGPU_FEATURE_VBLANK,
  AEROGPU_IRQ_FENCE,
  AEROGPU_MMIO_MAGIC,
  AEROGPU_MMIO_REG_DOORBELL,
  AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS,
  AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
  AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
  AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER,
  AEROGPU_PCI_DEVICE_ID,
  AEROGPU_PCI_PROG_IF,
  AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE,
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

type AbiDump = {
  sizes: Map<string, number>;
  offsets: Map<string, number>;
  consts: Map<string, bigint>;
};

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

  const testDir = path.dirname(fileURLToPath(import.meta.url));
  const repoRoot = path.resolve(testDir, "../../..");
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

  // Key offsets.
  assert.equal(off("aerogpu_cmd_stream_header", "magic"), AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC);
  assert.equal(off("aerogpu_cmd_stream_header", "abi_version"), AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION);
  assert.equal(off("aerogpu_cmd_stream_header", "size_bytes"), AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES);
  assert.equal(off("aerogpu_cmd_stream_header", "flags"), AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS);

  assert.equal(off("aerogpu_cmd_hdr", "opcode"), AEROGPU_CMD_HDR_OFF_OPCODE);
  assert.equal(off("aerogpu_cmd_hdr", "size_bytes"), AEROGPU_CMD_HDR_OFF_SIZE_BYTES);

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

  // Constants.
  assert.equal(konst("AEROGPU_ABI_MAJOR"), BigInt(AEROGPU_ABI_MAJOR));
  assert.equal(konst("AEROGPU_ABI_MINOR"), BigInt(AEROGPU_ABI_MINOR));
  assert.equal(konst("AEROGPU_ABI_VERSION_U32"), BigInt(AEROGPU_ABI_VERSION_U32));
  assert.equal(konst("AEROGPU_PCI_VENDOR_ID"), BigInt(AEROGPU_PCI_VENDOR_ID));
  assert.equal(konst("AEROGPU_PCI_DEVICE_ID"), BigInt(AEROGPU_PCI_DEVICE_ID));
  assert.equal(
    konst("AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER"),
    BigInt(AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER),
  );
  assert.equal(konst("AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE"), BigInt(AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE));
  assert.equal(konst("AEROGPU_PCI_PROG_IF"), BigInt(AEROGPU_PCI_PROG_IF));

  assert.equal(konst("AEROGPU_MMIO_MAGIC"), BigInt(AEROGPU_MMIO_MAGIC));
  assert.equal(konst("AEROGPU_MMIO_REG_DOORBELL"), BigInt(AEROGPU_MMIO_REG_DOORBELL));
  assert.equal(konst("AEROGPU_FEATURE_FENCE_PAGE"), AEROGPU_FEATURE_FENCE_PAGE);
  assert.equal(konst("AEROGPU_FEATURE_VBLANK"), AEROGPU_FEATURE_VBLANK);
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
  assert.equal(konst("AEROGPU_CMD_NOP"), BigInt(AerogpuCmdOpcode.Nop));
  assert.equal(konst("AEROGPU_CMD_DEBUG_MARKER"), BigInt(AerogpuCmdOpcode.DebugMarker));
  assert.equal(konst("AEROGPU_CMD_CREATE_BUFFER"), BigInt(AerogpuCmdOpcode.CreateBuffer));
  assert.equal(konst("AEROGPU_CMD_CREATE_TEXTURE2D"), BigInt(AerogpuCmdOpcode.CreateTexture2d));
  assert.equal(konst("AEROGPU_CMD_DESTROY_RESOURCE"), BigInt(AerogpuCmdOpcode.DestroyResource));
  assert.equal(konst("AEROGPU_CMD_RESOURCE_DIRTY_RANGE"), BigInt(AerogpuCmdOpcode.ResourceDirtyRange));
  assert.equal(konst("AEROGPU_CMD_UPLOAD_RESOURCE"), BigInt(AerogpuCmdOpcode.UploadResource));
  assert.equal(konst("AEROGPU_CMD_CREATE_SHADER_DXBC"), BigInt(AerogpuCmdOpcode.CreateShaderDxbc));
  assert.equal(konst("AEROGPU_CMD_DESTROY_SHADER"), BigInt(AerogpuCmdOpcode.DestroyShader));
  assert.equal(konst("AEROGPU_CMD_BIND_SHADERS"), BigInt(AerogpuCmdOpcode.BindShaders));
  assert.equal(konst("AEROGPU_CMD_SET_SHADER_CONSTANTS_F"), BigInt(AerogpuCmdOpcode.SetShaderConstantsF));
  assert.equal(konst("AEROGPU_CMD_CREATE_INPUT_LAYOUT"), BigInt(AerogpuCmdOpcode.CreateInputLayout));
  assert.equal(konst("AEROGPU_CMD_DESTROY_INPUT_LAYOUT"), BigInt(AerogpuCmdOpcode.DestroyInputLayout));
  assert.equal(konst("AEROGPU_CMD_SET_INPUT_LAYOUT"), BigInt(AerogpuCmdOpcode.SetInputLayout));
  assert.equal(konst("AEROGPU_CMD_SET_BLEND_STATE"), BigInt(AerogpuCmdOpcode.SetBlendState));
  assert.equal(konst("AEROGPU_CMD_SET_DEPTH_STENCIL_STATE"), BigInt(AerogpuCmdOpcode.SetDepthStencilState));
  assert.equal(konst("AEROGPU_CMD_SET_RASTERIZER_STATE"), BigInt(AerogpuCmdOpcode.SetRasterizerState));
  assert.equal(konst("AEROGPU_CMD_SET_RENDER_TARGETS"), BigInt(AerogpuCmdOpcode.SetRenderTargets));
  assert.equal(konst("AEROGPU_CMD_SET_VIEWPORT"), BigInt(AerogpuCmdOpcode.SetViewport));
  assert.equal(konst("AEROGPU_CMD_SET_SCISSOR"), BigInt(AerogpuCmdOpcode.SetScissor));
  assert.equal(konst("AEROGPU_CMD_SET_VERTEX_BUFFERS"), BigInt(AerogpuCmdOpcode.SetVertexBuffers));
  assert.equal(konst("AEROGPU_CMD_SET_INDEX_BUFFER"), BigInt(AerogpuCmdOpcode.SetIndexBuffer));
  assert.equal(konst("AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY"), BigInt(AerogpuCmdOpcode.SetPrimitiveTopology));
  assert.equal(konst("AEROGPU_CMD_SET_TEXTURE"), BigInt(AerogpuCmdOpcode.SetTexture));
  assert.equal(konst("AEROGPU_CMD_SET_SAMPLER_STATE"), BigInt(AerogpuCmdOpcode.SetSamplerState));
  assert.equal(konst("AEROGPU_CMD_SET_RENDER_STATE"), BigInt(AerogpuCmdOpcode.SetRenderState));
  assert.equal(konst("AEROGPU_CMD_CLEAR"), BigInt(AerogpuCmdOpcode.Clear));
  assert.equal(konst("AEROGPU_CMD_DRAW"), BigInt(AerogpuCmdOpcode.Draw));
  assert.equal(konst("AEROGPU_CMD_DRAW_INDEXED"), BigInt(AerogpuCmdOpcode.DrawIndexed));
  assert.equal(konst("AEROGPU_CMD_PRESENT"), BigInt(AerogpuCmdOpcode.Present));
  assert.equal(konst("AEROGPU_CMD_PRESENT_EX"), BigInt(AerogpuCmdOpcode.PresentEx));
  assert.equal(konst("AEROGPU_CMD_EXPORT_SHARED_SURFACE"), BigInt(AerogpuCmdOpcode.ExportSharedSurface));
  assert.equal(konst("AEROGPU_CMD_IMPORT_SHARED_SURFACE"), BigInt(AerogpuCmdOpcode.ImportSharedSurface));
  assert.equal(konst("AEROGPU_CMD_FLUSH"), BigInt(AerogpuCmdOpcode.Flush));

  assert.equal(konst("AEROGPU_INPUT_LAYOUT_BLOB_MAGIC"), BigInt(AEROGPU_INPUT_LAYOUT_BLOB_MAGIC));
  assert.equal(konst("AEROGPU_INPUT_LAYOUT_BLOB_VERSION"), BigInt(AEROGPU_INPUT_LAYOUT_BLOB_VERSION));

  assert.equal(konst("AEROGPU_TOPOLOGY_POINTLIST"), BigInt(AerogpuPrimitiveTopology.PointList));
  assert.equal(konst("AEROGPU_TOPOLOGY_LINELIST"), BigInt(AerogpuPrimitiveTopology.LineList));
  assert.equal(konst("AEROGPU_TOPOLOGY_LINESTRIP"), BigInt(AerogpuPrimitiveTopology.LineStrip));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLELIST"), BigInt(AerogpuPrimitiveTopology.TriangleList));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLESTRIP"), BigInt(AerogpuPrimitiveTopology.TriangleStrip));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLEFAN"), BigInt(AerogpuPrimitiveTopology.TriangleFan));

  assert.equal(konst("AEROGPU_SUBMIT_FLAG_PRESENT"), BigInt(AEROGPU_SUBMIT_FLAG_PRESENT));
  assert.equal(konst("AEROGPU_SUBMIT_FLAG_NO_IRQ"), BigInt(AEROGPU_SUBMIT_FLAG_NO_IRQ));
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
