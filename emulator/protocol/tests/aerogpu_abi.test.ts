import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  AEROGPU_BLEND_STATE_SIZE,
  AEROGPU_CMD_BIND_SHADERS_SIZE,
  AEROGPU_CMD_CLEAR_SIZE,
  AEROGPU_CMD_COPY_BUFFER_SIZE,
  AEROGPU_CMD_COPY_TEXTURE2D_SIZE,
  AEROGPU_CMD_CREATE_BUFFER_SIZE,
  AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE,
  AEROGPU_CMD_CREATE_SAMPLER_SIZE,
  AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE,
  AEROGPU_CMD_CREATE_TEXTURE2D_SIZE,
  AEROGPU_CMD_DESTROY_INPUT_LAYOUT_SIZE,
  AEROGPU_CMD_DESTROY_RESOURCE_SIZE,
  AEROGPU_CMD_DESTROY_SAMPLER_SIZE,
  AEROGPU_CMD_DESTROY_SHADER_SIZE,
  AEROGPU_CMD_DISPATCH_SIZE,
  AEROGPU_CMD_DRAW_INDEXED_SIZE,
  AEROGPU_CMD_DRAW_SIZE,
  AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_FLUSH_SIZE,
  AEROGPU_CMD_HDR_OFF_OPCODE,
  AEROGPU_CMD_HDR_OFF_SIZE_BYTES,
  AEROGPU_CMD_HDR_SIZE,
  AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_PRESENT_SIZE,
  AEROGPU_CMD_PRESENT_EX_SIZE,
  AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE,
  AEROGPU_CMD_SET_BLEND_STATE_SIZE,
  AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE,
  AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE,
  AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE,
  AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE,
  AEROGPU_CMD_SET_INDEX_BUFFER_SIZE,
  AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE,
  AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE,
  AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE,
  AEROGPU_CMD_SET_RENDER_STATE_SIZE,
  AEROGPU_CMD_SET_RENDER_TARGETS_SIZE,
  AEROGPU_CMD_SET_SAMPLER_STATE_SIZE,
  AEROGPU_CMD_SET_SAMPLERS_SIZE,
  AEROGPU_CMD_SET_SCISSOR_SIZE,
  AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE,
  AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE,
  AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE,
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
  AEROGPU_DEPTH_STENCIL_STATE_SIZE,
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
  AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE,
  AEROGPU_RASTERIZER_STATE_SIZE,
  AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
  AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
  AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
  AEROGPU_RESOURCE_USAGE_NONE,
  AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
  AEROGPU_RESOURCE_USAGE_SCANOUT,
  AEROGPU_RESOURCE_USAGE_STORAGE,
  AEROGPU_RESOURCE_USAGE_TEXTURE,
  AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
  AEROGPU_VERTEX_BUFFER_BINDING_SIZE,
  AEROGPU_CONSTANT_BUFFER_BINDING_SIZE,
  AEROGPU_SHADER_RESOURCE_BUFFER_BINDING_SIZE,
  AEROGPU_UNORDERED_ACCESS_BUFFER_BINDING_SIZE,
  AerogpuBlendFactor,
  AerogpuBlendOp,
  AerogpuCmdStreamFlags,
  AerogpuCompareFunc,
  AerogpuCullMode,
  AerogpuCmdOpcode,
  AerogpuFillMode,
  AerogpuIndexFormat,
  AerogpuPrimitiveTopology,
  AerogpuShaderStage,
  AerogpuShaderStageEx,
  decodeCmdHdr,
  decodeCmdStreamHeader,
} from "../aerogpu/aerogpu_cmd.ts";
import * as aerogpuCmd from "../aerogpu/aerogpu_cmd.ts";
import * as aerogpuPci from "../aerogpu/aerogpu_pci.ts";
import {
  AEROGPU_ABI_MAJOR,
  AEROGPU_ABI_MINOR,
  AEROGPU_ABI_VERSION_U32,
  AEROGPU_FEATURE_CURSOR,
  AEROGPU_FEATURE_ERROR_INFO,
  AEROGPU_FEATURE_FENCE_PAGE,
  AEROGPU_FEATURE_SCANOUT,
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
  AEROGPU_PCI_BAR1_INDEX,
  AEROGPU_PCI_BAR1_SIZE_BYTES,
  AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES,
  AEROGPU_PCI_PROG_IF,
  AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE,
  AEROGPU_PCI_SUBSYSTEM_ID,
  AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
  AEROGPU_PCI_VENDOR_ID,
  AEROGPU_RING_CONTROL_ENABLE,
  AerogpuAbiError,
  parseAndValidateAbiVersionU32,
} from "../aerogpu/aerogpu_pci.ts";
import * as aerogpuRing from "../aerogpu/aerogpu_ring.ts";
import {
  AEROGPU_ALLOC_ENTRY_OFF_GPA,
  AEROGPU_ALLOC_ENTRY_OFF_SIZE_BYTES,
  AEROGPU_ALLOC_ENTRY_SIZE,
  AEROGPU_ALLOC_FLAG_NONE,
  AEROGPU_ALLOC_FLAG_READONLY,
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
  AEROGPU_SUBMIT_FLAG_NONE,
  AEROGPU_SUBMIT_FLAG_NO_IRQ,
  AEROGPU_SUBMIT_FLAG_PRESENT,
  decodeAllocTableHeader,
  decodeRingHeader,
  decodeSubmitDesc,
  writeFencePageCompletedFence,
} from "../aerogpu/aerogpu_ring.ts";
import {
  AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE,
  AEROGPU_UMDPRIV_FEATURE_CURSOR,
  AEROGPU_UMDPRIV_FEATURE_ERROR_INFO,
  AEROGPU_UMDPRIV_FEATURE_SCANOUT,
  AEROGPU_UMDPRIV_FEATURE_TRANSFER,
  AEROGPU_UMDPRIV_FEATURE_VBLANK,
  AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE,
  AEROGPU_UMDPRIV_FLAG_HAS_VBLANK,
  AEROGPU_UMDPRIV_FLAG_IS_LEGACY,
  AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP,
  AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU,
  AEROGPU_UMDPRIV_MMIO_REG_MAGIC,
  AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION,
  AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO,
  AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI,
  AEROGPU_UMDPRIV_STRUCT_VERSION_V1,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_ABI_VERSION_U32,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_FEATURES,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_MMIO_MAGIC,
  AEROGPU_UMD_PRIVATE_V1_OFF_FLAGS,
  AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES,
  AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION,
  AEROGPU_UMD_PRIVATE_V1_SIZE,
} from "../aerogpu/aerogpu_umd_private.ts";
import * as aerogpuUmdPrivate from "../aerogpu/aerogpu_umd_private.ts";
import {
  AerogpuWddmAllocKind,
  AEROGPU_WDDM_ALLOC_ID_KMD_MIN,
  AEROGPU_WDDM_ALLOC_ID_UMD_MAX,
  AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER,
  AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT,
  AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH,
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE,
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED,
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE,
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING,
  AEROGPU_WDDM_ALLOC_PRIV_MAGIC,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_ALLOC_ID,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_FLAGS,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_RESERVED0,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_SHARE_TOKEN,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_SIZE_BYTES,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION,
  AEROGPU_WDDM_ALLOC_PRIV_SIZE,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ALLOC_ID,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FLAGS,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FORMAT,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_HEIGHT,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_KIND,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_MAGIC,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED0,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED1,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ROW_PITCH_BYTES,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_SHARE_TOKEN,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_SIZE_BYTES,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_VERSION,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_WIDTH,
  AEROGPU_WDDM_ALLOC_PRIV_V2_SIZE,
  AEROGPU_WDDM_ALLOC_PRIV_VERSION,
  AEROGPU_WDDM_ALLOC_PRIV_VERSION_2,
} from "../aerogpu/aerogpu_wddm_alloc.ts";
import * as aerogpuWddmAlloc from "../aerogpu/aerogpu_wddm_alloc.ts";

// These constants are part of the stable Windows device contract; we intentionally lock their
// numeric values so they cannot drift due to coordinated edits across the C/Rust/TS mirrors.
test("AeroGPU PCI identity + BAR layout constants are stable (device contract)", () => {
  assert.equal(AEROGPU_PCI_VENDOR_ID, 0xa3a0);
  assert.equal(AEROGPU_PCI_DEVICE_ID, 0x0001);
  assert.equal(AEROGPU_PCI_SUBSYSTEM_VENDOR_ID, 0xa3a0);
  assert.equal(AEROGPU_PCI_SUBSYSTEM_ID, 0x0001);
  assert.equal(AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, 0x03);
  assert.equal(AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, 0x00);
  assert.equal(AEROGPU_PCI_PROG_IF, 0x00);
  assert.equal(AEROGPU_PCI_BAR0_INDEX, 0);
  assert.equal(AEROGPU_PCI_BAR0_SIZE_BYTES, 64 * 1024);
  assert.equal(AEROGPU_PCI_BAR1_INDEX, 1);
  assert.equal(AEROGPU_PCI_BAR1_SIZE_BYTES, 64 * 1024 * 1024);
  assert.equal(AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES, 0x40_000);
});

type AbiDump = {
  sizes: Map<string, number>;
  offsets: Map<string, number>;
  consts: Map<string, bigint>;
};

const testDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(testDir, "../../..");

function parseAbiDump(text: string): AbiDump {
  const sizes = new Map<string, number>();
  const sizeLines = new Map<string, number>();
  const offsets = new Map<string, number>();
  const offsetLines = new Map<string, number>();
  const consts = new Map<string, bigint>();
  const constLines = new Map<string, number>();

  let lineNo = 0;
  for (const line of text.split("\n")) {
    lineNo++;
    const trimmed = line.trim();
    if (!trimmed) continue;

    const parts = trimmed.split(/\s+/);
    if (parts[0] === "SIZE") {
      const key = parts[1]!;
      const value = Number(parts[2]!);
      const prev = sizes.get(key);
      if (prev !== undefined) {
        // Duplicates can happen when multiple PRs touch the C ABI dump helper and get merged with
        // minimal conflict resolution. Accept identical duplicates but still fail if values differ.
        if (prev !== value) {
          const prevLine = sizeLines.get(key) ?? 0;
          throw new Error(
            `duplicate SIZE for ${key}: first @${prevLine} = ${prev}, again @${lineNo} = ${value}: ${trimmed}`,
          );
        }
        continue;
      }
      sizes.set(key, value);
      sizeLines.set(key, lineNo);
    } else if (parts[0] === "OFF") {
      const key = `${parts[1]}.${parts[2]}`;
      const value = Number(parts[3]!);
      const prev = offsets.get(key);
      if (prev !== undefined) {
        if (prev !== value) {
          const prevLine = offsetLines.get(key) ?? 0;
          throw new Error(
            `duplicate OFF for ${key}: first @${prevLine} = ${prev}, again @${lineNo} = ${value}: ${trimmed}`,
          );
        }
        continue;
      }
      offsets.set(key, value);
      offsetLines.set(key, lineNo);
    } else if (parts[0] === "CONST") {
      const key = parts[1]!;
      const value = BigInt(parts[2]!);
      const prev = consts.get(key);
      if (prev !== undefined) {
        if (prev !== value) {
          const prevLine = constLines.get(key) ?? 0;
          throw new Error(
            `duplicate CONST for ${key}: first @${prevLine} = ${prev}, again @${lineNo} = ${value}: ${trimmed}`,
          );
        }
        continue;
      }
      consts.set(key, value);
      constLines.set(key, lineNo);
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

  // `ETXTBSY` ("text file busy") can happen on some filesystems if the compiler/linker still has
  // the output file open when we immediately attempt to execute it. Retry a few times with small
  // backoff to make this test robust under parallel runs.
  const sleepMs = (ms: number) => {
    // Node doesn't provide a built-in synchronous sleep; Atomics.wait is the standard workaround.
    const sab = new SharedArrayBuffer(4);
    const ia = new Int32Array(sab);
    Atomics.wait(ia, 0, 0, ms);
  };
  let run = spawnSync(outPath, [], { encoding: "utf8" });
  for (let attempt = 0; attempt < 10; attempt++) {
    // When spawn fails, status is null and error is set.
    if (!run.error) break;
    if ((run.error as NodeJS.ErrnoException).code !== "ETXTBSY") break;
    sleepMs(5 * (attempt + 1));
    run = spawnSync(outPath, [], { encoding: "utf8" });
  }
  assert.equal(run.status, 0, `ABI dump helper failed: ${run.error ?? ""}\n${run.stderr}\n${run.stdout}`);

  cachedAbi = parseAbiDump(run.stdout);

  // Best-effort cleanup: avoid leaving compiled helpers behind in /tmp on CI.
  // (If assertions above fail, we intentionally leave the binary for debugging.)
  try {
    fs.unlinkSync(outPath);
  } catch {
    // ignore
  }
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

function parseCStructDefNames(headerPath: string): string[] {
  const text = fs.readFileSync(headerPath, "utf8");

  const names = new Set<string>();
  let idx = 0;
  for (;;) {
    const pos = text.indexOf("struct aerogpu_", idx);
    if (pos === -1) break;

    const start = pos + "struct ".length;
    let end = start;
    while (end < text.length) {
      const ch = text.charCodeAt(end);
      const isAlphaNum =
        (ch >= 0x30 && ch <= 0x39) || // 0-9
        (ch >= 0x41 && ch <= 0x5a) || // A-Z
        (ch >= 0x61 && ch <= 0x7a); // a-z
      if (!isAlphaNum && ch !== 0x5f) break; // _
      end++;
    }

    let after = end;
    while (after < text.length) {
      const ch = text.charCodeAt(after);
      const isWs = ch === 0x20 || ch === 0x09 || ch === 0x0a || ch === 0x0d;
      if (!isWs) break;
      after++;
    }

    // Only treat `struct name { ... }` as a definition. This excludes usages like:
    // `struct aerogpu_cmd_hdr hdr;`
    if (after < text.length && text[after] === "{") {
      names.add(text.slice(start, end));
    }

    idx = end;
  }

  return [...names].sort();
}

function parseCcmdStructDefNames(): string[] {
  return parseCStructDefNames(path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_cmd.h"));
}

function upperSnakeToPascalCase(s: string): string {
  return s
    .split("_")
    .filter((part) => part.length > 0)
    .map((part) => part[0]!.toUpperCase() + part.slice(1).toLowerCase())
    .join("");
}

function parseCDefineConstNames(headerPath: string): string[] {
  const text = fs.readFileSync(headerPath, "utf8");
  const names = new Set<string>();

  for (const rawLine of text.split("\n")) {
    const line = rawLine.trimStart();
    if (!line.startsWith("#define")) continue;
    const rest = line.slice("#define".length).trimStart();
    const name = rest.split(/\s+/, 1)[0];
    if (!name) continue;

    if (!name.startsWith("AEROGPU_")) continue;
    if (name.startsWith("AEROGPU_PROTOCOL_")) continue;
    // Function-like macros are not ABI surface area.
    if (name.includes("(")) continue;
    // Internal preprocessor helpers used only by the C headers.
    if (name.startsWith("AEROGPU_CONCAT") || name === "AEROGPU_STATIC_ASSERT") continue;

    names.add(name);
  }

  return [...names].sort();
}

function parseCEnumConstNames(headerPath: string, enumName: string, prefix: string): string[] {
  const text = fs.readFileSync(headerPath, "utf8");

  const start = text.indexOf(enumName);
  assert.notEqual(start, -1, `missing ${enumName} in ${headerPath}`);
  const afterStart = text.slice(start);

  const open = afterStart.indexOf("{");
  assert.notEqual(open, -1, `missing '{' for ${enumName}`);
  const afterOpen = afterStart.slice(open + 1);

  const close = afterOpen.indexOf("};");
  assert.notEqual(close, -1, `missing '};' for ${enumName}`);
  const body = afterOpen.slice(0, close);

  const names = new Set<string>();
  let idx = 0;
  for (;;) {
    const pos = body.indexOf(prefix, idx);
    if (pos === -1) break;
    let end = pos;
    while (end < body.length) {
      const ch = body.charCodeAt(end);
      const isAlphaNum =
        (ch >= 0x30 && ch <= 0x39) || // 0-9
        (ch >= 0x41 && ch <= 0x5a) || // A-Z
        (ch >= 0x61 && ch <= 0x7a); // a-z (just in case)
      if (!isAlphaNum && ch !== 0x5f) break;
      end++;
    }
    names.add(body.slice(pos, end));
    idx = end;
  }

  return [...names].sort();
}

function assertNameSetEq(seen: string[], expected: string[], what: string): void {
  const seenSet = new Set(seen);
  const expectedSet = new Set(expected);

  const missing = [...expectedSet].filter((v) => !seenSet.has(v)).sort();
  const extra = [...seenSet].filter((v) => !expectedSet.has(v)).sort();

  assert.deepEqual(missing, [], `${what}: missing`);
  assert.deepEqual(extra, [], `${what}: extra`);
}

function valueToBigInt(v: unknown, name: string): bigint {
  if (typeof v === "number") return BigInt(v);
  if (typeof v === "bigint") return v;
  throw new Error(`expected ${name} to be a number|bigint export, got ${typeof v}`);
}

function topologyCNameToTsKey(cName: string): string {
  const suffix = cName.replace(/^AEROGPU_TOPOLOGY_/, "");
  if (suffix.startsWith("PATCHLIST_")) {
    const n = suffix.replace(/^PATCHLIST_/, "");
    if (!/^\d+$/.test(n)) {
      throw new Error(`unknown topology enum name: ${cName}`);
    }
    return `PatchList${n}`;
  }
  switch (suffix) {
    case "POINTLIST":
      return "PointList";
    case "LINELIST":
      return "LineList";
    case "LINESTRIP":
      return "LineStrip";
    case "TRIANGLELIST":
      return "TriangleList";
    case "TRIANGLESTRIP":
      return "TriangleStrip";
    case "TRIANGLEFAN":
      return "TriangleFan";
    case "LINELIST_ADJ":
      return "LineListAdj";
    case "LINESTRIP_ADJ":
      return "LineStripAdj";
    case "TRIANGLELIST_ADJ":
      return "TriangleListAdj";
    case "TRIANGLESTRIP_ADJ":
      return "TriangleStripAdj";
    default:
      throw new Error(`unknown topology enum name: ${cName}`);
  }
}

function formatCNameToTsKey(cName: string): string {
  const suffix = cName.replace(/^AEROGPU_FORMAT_/, "");
  return suffix
    .split("_")
    .filter((part) => part.length > 0)
    .map((part) => (/\d/.test(part) ? part : part[0]!.toUpperCase() + part.slice(1).toLowerCase()))
    .join("");
}

test("TypeScript layout matches C headers", () => {
  const abi = abiDump();

  const pciHeader = path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_pci.h");
  const ringHeader = path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_ring.h");
  const cmdHeader = path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_cmd.h");
  const umdPrivateHeader = path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_umd_private.h");
  const wddmAllocHeader = path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_wddm_alloc.h");

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
  assert.equal(size("aerogpu_cmd_set_shader_constants_i"), AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE);
  assert.equal(size("aerogpu_cmd_set_shader_constants_b"), AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE);
  assert.equal(size("aerogpu_input_layout_blob_header"), AEROGPU_INPUT_LAYOUT_BLOB_HEADER_SIZE);
  assert.equal(size("aerogpu_input_layout_element_dxgi"), AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_SIZE);
  assert.equal(size("aerogpu_cmd_create_input_layout"), AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE);
  assert.equal(size("aerogpu_cmd_destroy_input_layout"), AEROGPU_CMD_DESTROY_INPUT_LAYOUT_SIZE);
  assert.equal(size("aerogpu_cmd_set_input_layout"), AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE);
  assert.equal(size("aerogpu_blend_state"), AEROGPU_BLEND_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_blend_state"), AEROGPU_CMD_SET_BLEND_STATE_SIZE);
  assert.equal(size("aerogpu_depth_stencil_state"), AEROGPU_DEPTH_STENCIL_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_depth_stencil_state"), AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE);
  assert.equal(size("aerogpu_rasterizer_state"), AEROGPU_RASTERIZER_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_rasterizer_state"), AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_render_targets"), AEROGPU_CMD_SET_RENDER_TARGETS_SIZE);
  assert.equal(size("aerogpu_cmd_set_viewport"), AEROGPU_CMD_SET_VIEWPORT_SIZE);
  assert.equal(size("aerogpu_cmd_set_scissor"), AEROGPU_CMD_SET_SCISSOR_SIZE);
  assert.equal(size("aerogpu_vertex_buffer_binding"), AEROGPU_VERTEX_BUFFER_BINDING_SIZE);
  assert.equal(size("aerogpu_cmd_set_vertex_buffers"), AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE);
  assert.equal(size("aerogpu_cmd_set_index_buffer"), AEROGPU_CMD_SET_INDEX_BUFFER_SIZE);
  assert.equal(size("aerogpu_cmd_set_primitive_topology"), AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE);
  assert.equal(size("aerogpu_cmd_set_texture"), AEROGPU_CMD_SET_TEXTURE_SIZE);
  assert.equal(size("aerogpu_cmd_set_sampler_state"), AEROGPU_CMD_SET_SAMPLER_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_set_render_state"), AEROGPU_CMD_SET_RENDER_STATE_SIZE);
  assert.equal(size("aerogpu_cmd_create_sampler"), AEROGPU_CMD_CREATE_SAMPLER_SIZE);
  assert.equal(size("aerogpu_cmd_destroy_sampler"), AEROGPU_CMD_DESTROY_SAMPLER_SIZE);
  assert.equal(size("aerogpu_cmd_set_samplers"), AEROGPU_CMD_SET_SAMPLERS_SIZE);
  assert.equal(size("aerogpu_constant_buffer_binding"), AEROGPU_CONSTANT_BUFFER_BINDING_SIZE);
  assert.equal(size("aerogpu_cmd_set_constant_buffers"), AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE);
  assert.equal(size("aerogpu_shader_resource_buffer_binding"), AEROGPU_SHADER_RESOURCE_BUFFER_BINDING_SIZE);
  assert.equal(size("aerogpu_cmd_set_shader_resource_buffers"), AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE);
  assert.equal(size("aerogpu_unordered_access_buffer_binding"), AEROGPU_UNORDERED_ACCESS_BUFFER_BINDING_SIZE);
  assert.equal(size("aerogpu_cmd_set_unordered_access_buffers"), AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE);
  assert.equal(size("aerogpu_cmd_clear"), AEROGPU_CMD_CLEAR_SIZE);
  assert.equal(size("aerogpu_cmd_draw"), AEROGPU_CMD_DRAW_SIZE);
  assert.equal(size("aerogpu_cmd_draw_indexed"), AEROGPU_CMD_DRAW_INDEXED_SIZE);
  assert.equal(size("aerogpu_cmd_dispatch"), AEROGPU_CMD_DISPATCH_SIZE);
  assert.equal(size("aerogpu_cmd_present"), AEROGPU_CMD_PRESENT_SIZE);
  assert.equal(size("aerogpu_cmd_present_ex"), AEROGPU_CMD_PRESENT_EX_SIZE);
  assert.equal(size("aerogpu_cmd_export_shared_surface"), AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE);
  assert.equal(size("aerogpu_cmd_import_shared_surface"), AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE);
  assert.equal(size("aerogpu_cmd_release_shared_surface"), AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE);
  assert.equal(size("aerogpu_cmd_flush"), AEROGPU_CMD_FLUSH_SIZE);

  assert.equal(size("aerogpu_alloc_table_header"), AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  assert.equal(size("aerogpu_alloc_entry"), AEROGPU_ALLOC_ENTRY_SIZE);

  assert.equal(size("aerogpu_submit_desc"), AEROGPU_SUBMIT_DESC_SIZE);
  assert.equal(size("aerogpu_ring_header"), AEROGPU_RING_HEADER_SIZE);
  assert.equal(size("aerogpu_fence_page"), AEROGPU_FENCE_PAGE_SIZE);
  assert.equal(size("aerogpu_umd_private_v1"), AEROGPU_UMD_PRIVATE_V1_SIZE);
  assert.equal(size("aerogpu_wddm_alloc_priv"), AEROGPU_WDDM_ALLOC_PRIV_SIZE);
  assert.equal(size("aerogpu_wddm_alloc_priv_v2"), AEROGPU_WDDM_ALLOC_PRIV_V2_SIZE);

  // Escape ABI (driver-private; stable across x86/x64).
  assert.equal(size("aerogpu_escape_header"), 16);
  assert.equal(size("aerogpu_escape_query_device_out"), 24);
  assert.equal(size("aerogpu_escape_query_device_v2_out"), 48);
  assert.equal(size("aerogpu_escape_query_fence_out"), 48);
  assert.equal(size("aerogpu_escape_query_perf_out"), 264);
  assert.equal(size("aerogpu_dbgctl_ring_desc"), 24);
  assert.equal(size("aerogpu_dbgctl_ring_desc_v2"), 40);
  assert.equal(size("aerogpu_escape_dump_ring_inout"), 40 + 32 * 24);
  assert.equal(size("aerogpu_escape_dump_ring_v2_inout"), 52 + 32 * 40);
  assert.equal(size("aerogpu_escape_selftest_inout"), 32);
  assert.equal(size("aerogpu_escape_query_vblank_out"), 56);
  assert.equal(size("aerogpu_escape_dump_vblank_inout"), 56);
  assert.equal(size("aerogpu_escape_query_scanout_out"), 72);
  assert.equal(size("aerogpu_escape_query_scanout_out_v2"), 80);
  assert.equal(size("aerogpu_escape_query_cursor_out"), 72);
  assert.equal(size("aerogpu_escape_query_error_out"), 40);
  assert.equal(size("aerogpu_escape_set_cursor_position_in"), 24);
  assert.equal(size("aerogpu_escape_set_cursor_visibility_in"), 24);
  assert.equal(size("aerogpu_escape_set_cursor_shape_in"), 49);
  assert.equal(size("aerogpu_escape_map_shared_handle_inout"), 32);
  assert.equal(size("aerogpu_escape_read_gpa_inout"), 40 + 4096);
  assert.equal(size("aerogpu_dbgctl_createallocation_desc"), 56);
  assert.equal(size("aerogpu_escape_dump_createallocation_inout"), 32 + 32 * 56);

  // Coverage guard: Escape structs are driver-private, but should remain stable.
  // Ensure the C ABI dump helper is kept in sync with `aerogpu_escape.h` + `aerogpu_dbgctl_escape.h`.
  const escapeHeader = path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_escape.h");
  const dbgctlEscapeHeader = path.join(repoRoot, "drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h");
  assertNameSetEq(
    [...abi.sizes.keys()].filter((name) => name.startsWith("aerogpu_escape_") || name.startsWith("aerogpu_dbgctl_")),
    [
      ...parseCStructDefNames(escapeHeader),
      ...parseCStructDefNames(dbgctlEscapeHeader),
      // Alias typedef in `aerogpu_dbgctl_escape.h`.
      "aerogpu_escape_dump_vblank_inout",
    ],
    "Escape ABI struct coverage",
  );

  // Coverage guard: `aerogpu_pci.h` currently defines constants/enums only (no ABI structs).
  // If this changes, the TS mirror + ABI dump helper must be updated accordingly.
  assertNameSetEq([], parseCStructDefNames(pciHeader), "aerogpu_pci.h struct coverage");

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

  assert.equal(off("aerogpu_wddm_alloc_priv", "magic"), AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC);
  assert.equal(off("aerogpu_wddm_alloc_priv", "version"), AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION);
  assert.equal(off("aerogpu_wddm_alloc_priv", "alloc_id"), AEROGPU_WDDM_ALLOC_PRIV_OFF_ALLOC_ID);
  assert.equal(off("aerogpu_wddm_alloc_priv", "flags"), AEROGPU_WDDM_ALLOC_PRIV_OFF_FLAGS);
  assert.equal(off("aerogpu_wddm_alloc_priv", "share_token"), AEROGPU_WDDM_ALLOC_PRIV_OFF_SHARE_TOKEN);
  assert.equal(off("aerogpu_wddm_alloc_priv", "size_bytes"), AEROGPU_WDDM_ALLOC_PRIV_OFF_SIZE_BYTES);
  assert.equal(off("aerogpu_wddm_alloc_priv", "reserved0"), AEROGPU_WDDM_ALLOC_PRIV_OFF_RESERVED0);

  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "magic"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_MAGIC);
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "version"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_VERSION);
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "alloc_id"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ALLOC_ID);
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "flags"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FLAGS);
  assert.equal(
    off("aerogpu_wddm_alloc_priv_v2", "share_token"),
    AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_SHARE_TOKEN,
  );
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "size_bytes"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_SIZE_BYTES);
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "reserved0"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED0);
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "kind"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_KIND);
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "width"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_WIDTH);
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "height"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_HEIGHT);
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "format"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FORMAT);
  assert.equal(
    off("aerogpu_wddm_alloc_priv_v2", "row_pitch_bytes"),
    AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ROW_PITCH_BYTES,
  );
  assert.equal(off("aerogpu_wddm_alloc_priv_v2", "reserved1"), AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED1);

  // Variable-length packets (must remain stable for parsing).
  assert.equal(off("aerogpu_cmd_create_shader_dxbc", "dxbc_size_bytes"), 16);
  assert.equal(off("aerogpu_cmd_set_shader_constants_f", "vec4_count"), 16);
  assert.equal(off("aerogpu_cmd_set_shader_constants_i", "vec4_count"), 16);
  assert.equal(off("aerogpu_cmd_set_shader_constants_b", "bool_count"), 16);
  assert.equal(off("aerogpu_cmd_create_input_layout", "blob_size_bytes"), 12);
  assert.equal(off("aerogpu_cmd_set_vertex_buffers", "buffer_count"), 12);
  assert.equal(off("aerogpu_cmd_upload_resource", "offset_bytes"), 16);
  assert.equal(off("aerogpu_cmd_upload_resource", "size_bytes"), 24);

  // Fixed-layout packet fields (helps catch accidental field reordering).
  assert.equal(off("aerogpu_cmd_upload_resource", "resource_handle"), 8);
  assert.equal(off("aerogpu_cmd_create_buffer", "buffer_handle"), 8);
  assert.equal(off("aerogpu_cmd_create_buffer", "usage_flags"), 12);
  assert.equal(off("aerogpu_cmd_create_buffer", "size_bytes"), 16);
  assert.equal(off("aerogpu_cmd_create_buffer", "backing_alloc_id"), 24);
  assert.equal(off("aerogpu_cmd_create_buffer", "backing_offset_bytes"), 28);
  assert.equal(off("aerogpu_cmd_create_texture2d", "texture_handle"), 8);
  assert.equal(off("aerogpu_cmd_create_texture2d", "usage_flags"), 12);
  assert.equal(off("aerogpu_cmd_create_texture2d", "format"), 16);
  assert.equal(off("aerogpu_cmd_create_texture2d", "width"), 20);
  assert.equal(off("aerogpu_cmd_create_texture2d", "height"), 24);
  assert.equal(off("aerogpu_cmd_create_texture2d", "mip_levels"), 28);
  assert.equal(off("aerogpu_cmd_create_texture2d", "array_layers"), 32);
  assert.equal(off("aerogpu_cmd_create_texture2d", "row_pitch_bytes"), 36);
  assert.equal(off("aerogpu_cmd_create_texture2d", "backing_alloc_id"), 40);
  assert.equal(off("aerogpu_cmd_create_texture2d", "backing_offset_bytes"), 44);
  assert.equal(off("aerogpu_cmd_destroy_resource", "resource_handle"), 8);
  assert.equal(off("aerogpu_cmd_resource_dirty_range", "resource_handle"), 8);
  assert.equal(off("aerogpu_cmd_resource_dirty_range", "offset_bytes"), 16);
  assert.equal(off("aerogpu_cmd_resource_dirty_range", "size_bytes"), 24);
  assert.equal(off("aerogpu_cmd_create_shader_dxbc", "shader_handle"), 8);
  assert.equal(off("aerogpu_cmd_create_shader_dxbc", "stage"), 12);
  assert.equal(off("aerogpu_cmd_destroy_shader", "shader_handle"), 8);
  assert.equal(off("aerogpu_cmd_bind_shaders", "vs"), 8);
  assert.equal(off("aerogpu_cmd_bind_shaders", "ps"), 12);
  assert.equal(off("aerogpu_cmd_bind_shaders", "cs"), 16);
  assert.equal(off("aerogpu_cmd_bind_shaders", "reserved0"), 20);
  assert.equal(off("aerogpu_cmd_set_shader_constants_f", "stage"), 8);
  assert.equal(off("aerogpu_cmd_set_shader_constants_f", "start_register"), 12);
  assert.equal(off("aerogpu_cmd_set_shader_constants_i", "stage"), 8);
  assert.equal(off("aerogpu_cmd_set_shader_constants_i", "start_register"), 12);
  assert.equal(off("aerogpu_cmd_set_shader_constants_b", "stage"), 8);
  assert.equal(off("aerogpu_cmd_set_shader_constants_b", "start_register"), 12);
  assert.equal(off("aerogpu_cmd_create_input_layout", "input_layout_handle"), 8);
  assert.equal(off("aerogpu_cmd_destroy_input_layout", "input_layout_handle"), 8);
  assert.equal(off("aerogpu_cmd_set_input_layout", "input_layout_handle"), 8);
  assert.equal(off("aerogpu_cmd_set_blend_state", "state"), 8);
  assert.equal(off("aerogpu_blend_state", "enable"), 0);
  assert.equal(off("aerogpu_blend_state", "src_factor"), 4);
  assert.equal(off("aerogpu_blend_state", "dst_factor"), 8);
  assert.equal(off("aerogpu_blend_state", "blend_op"), 12);
  assert.equal(off("aerogpu_blend_state", "color_write_mask"), 16);
  assert.equal(off("aerogpu_cmd_set_depth_stencil_state", "state"), 8);
  assert.equal(off("aerogpu_depth_stencil_state", "depth_enable"), 0);
  assert.equal(off("aerogpu_depth_stencil_state", "depth_write_enable"), 4);
  assert.equal(off("aerogpu_depth_stencil_state", "depth_func"), 8);
  assert.equal(off("aerogpu_depth_stencil_state", "stencil_enable"), 12);
  assert.equal(off("aerogpu_depth_stencil_state", "stencil_read_mask"), 16);
  assert.equal(off("aerogpu_depth_stencil_state", "stencil_write_mask"), 17);
  assert.equal(off("aerogpu_cmd_set_rasterizer_state", "state"), 8);
  assert.equal(off("aerogpu_rasterizer_state", "fill_mode"), 0);
  assert.equal(off("aerogpu_rasterizer_state", "cull_mode"), 4);
  assert.equal(off("aerogpu_rasterizer_state", "front_ccw"), 8);
  assert.equal(off("aerogpu_rasterizer_state", "scissor_enable"), 12);
  assert.equal(off("aerogpu_rasterizer_state", "depth_bias"), 16);
  assert.equal(off("aerogpu_cmd_set_render_targets", "color_count"), 8);
  assert.equal(off("aerogpu_cmd_set_render_targets", "depth_stencil"), 12);
  assert.equal(off("aerogpu_cmd_set_render_targets", "colors"), 16);
  assert.equal(off("aerogpu_cmd_set_viewport", "x_f32"), 8);
  assert.equal(off("aerogpu_cmd_set_viewport", "y_f32"), 12);
  assert.equal(off("aerogpu_cmd_set_viewport", "width_f32"), 16);
  assert.equal(off("aerogpu_cmd_set_viewport", "height_f32"), 20);
  assert.equal(off("aerogpu_cmd_set_viewport", "min_depth_f32"), 24);
  assert.equal(off("aerogpu_cmd_set_viewport", "max_depth_f32"), 28);
  assert.equal(off("aerogpu_cmd_set_scissor", "x"), 8);
  assert.equal(off("aerogpu_cmd_set_scissor", "y"), 12);
  assert.equal(off("aerogpu_cmd_set_scissor", "width"), 16);
  assert.equal(off("aerogpu_cmd_set_scissor", "height"), 20);
  assert.equal(off("aerogpu_vertex_buffer_binding", "buffer"), 0);
  assert.equal(off("aerogpu_vertex_buffer_binding", "stride_bytes"), 4);
  assert.equal(off("aerogpu_vertex_buffer_binding", "offset_bytes"), 8);
  assert.equal(off("aerogpu_cmd_set_vertex_buffers", "start_slot"), 8);
  assert.equal(off("aerogpu_cmd_set_index_buffer", "buffer"), 8);
  assert.equal(off("aerogpu_cmd_set_index_buffer", "format"), 12);
  assert.equal(off("aerogpu_cmd_set_index_buffer", "offset_bytes"), 16);
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

  assert.equal(off("aerogpu_shader_resource_buffer_binding", "buffer"), 0);
  assert.equal(off("aerogpu_shader_resource_buffer_binding", "offset_bytes"), 4);
  assert.equal(off("aerogpu_shader_resource_buffer_binding", "size_bytes"), 8);
  assert.equal(off("aerogpu_shader_resource_buffer_binding", "reserved0"), 12);

  assert.equal(off("aerogpu_cmd_set_shader_resource_buffers", "shader_stage"), 8);
  assert.equal(off("aerogpu_cmd_set_shader_resource_buffers", "start_slot"), 12);
  assert.equal(off("aerogpu_cmd_set_shader_resource_buffers", "buffer_count"), 16);
  assert.equal(off("aerogpu_cmd_set_shader_resource_buffers", "reserved0"), 20);

  assert.equal(off("aerogpu_unordered_access_buffer_binding", "buffer"), 0);
  assert.equal(off("aerogpu_unordered_access_buffer_binding", "offset_bytes"), 4);
  assert.equal(off("aerogpu_unordered_access_buffer_binding", "size_bytes"), 8);
  assert.equal(off("aerogpu_unordered_access_buffer_binding", "initial_count"), 12);

  assert.equal(off("aerogpu_cmd_set_unordered_access_buffers", "shader_stage"), 8);
  assert.equal(off("aerogpu_cmd_set_unordered_access_buffers", "start_slot"), 12);
  assert.equal(off("aerogpu_cmd_set_unordered_access_buffers", "uav_count"), 16);
  assert.equal(off("aerogpu_cmd_set_unordered_access_buffers", "reserved0"), 20);
  assert.equal(off("aerogpu_cmd_clear", "flags"), 8);
  assert.equal(off("aerogpu_cmd_clear", "color_rgba_f32"), 12);
  assert.equal(off("aerogpu_cmd_clear", "depth_f32"), 28);
  assert.equal(off("aerogpu_cmd_clear", "stencil"), 32);
  assert.equal(off("aerogpu_cmd_draw", "vertex_count"), 8);
  assert.equal(off("aerogpu_cmd_draw", "instance_count"), 12);
  assert.equal(off("aerogpu_cmd_draw", "first_vertex"), 16);
  assert.equal(off("aerogpu_cmd_draw", "first_instance"), 20);
  assert.equal(off("aerogpu_cmd_draw_indexed", "index_count"), 8);
  assert.equal(off("aerogpu_cmd_draw_indexed", "instance_count"), 12);
  assert.equal(off("aerogpu_cmd_draw_indexed", "first_index"), 16);
  assert.equal(off("aerogpu_cmd_draw_indexed", "base_vertex"), 20);
  assert.equal(off("aerogpu_cmd_draw_indexed", "first_instance"), 24);
  assert.equal(off("aerogpu_cmd_dispatch", "group_count_x"), 8);
  assert.equal(off("aerogpu_cmd_dispatch", "group_count_y"), 12);
  assert.equal(off("aerogpu_cmd_dispatch", "group_count_z"), 16);
  assert.equal(off("aerogpu_cmd_dispatch", "reserved0"), 20);
  assert.equal(off("aerogpu_cmd_present", "scanout_id"), 8);
  assert.equal(off("aerogpu_cmd_present", "flags"), 12);
  assert.equal(off("aerogpu_cmd_present_ex", "scanout_id"), 8);
  assert.equal(off("aerogpu_cmd_present_ex", "flags"), 12);
  assert.equal(off("aerogpu_cmd_present_ex", "d3d9_present_flags"), 16);
  assert.equal(off("aerogpu_cmd_export_shared_surface", "resource_handle"), 8);
  assert.equal(off("aerogpu_cmd_export_shared_surface", "share_token"), 16);
  assert.equal(off("aerogpu_cmd_import_shared_surface", "out_resource_handle"), 8);
  assert.equal(off("aerogpu_cmd_import_shared_surface", "share_token"), 16);
  assert.equal(off("aerogpu_cmd_flush", "reserved0"), 8);
  assert.equal(off("aerogpu_cmd_flush", "reserved1"), 12);
  assert.equal(off("aerogpu_cmd_copy_buffer", "dst_buffer"), 8);
  assert.equal(off("aerogpu_cmd_copy_buffer", "src_buffer"), 12);
  assert.equal(off("aerogpu_cmd_copy_buffer", "dst_offset_bytes"), 16);
  assert.equal(off("aerogpu_cmd_copy_buffer", "src_offset_bytes"), 24);
  assert.equal(off("aerogpu_cmd_copy_buffer", "size_bytes"), 32);
  assert.equal(off("aerogpu_cmd_copy_buffer", "flags"), 40);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "dst_texture"), 8);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "src_texture"), 12);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "dst_mip_level"), 16);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "dst_array_layer"), 20);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "src_mip_level"), 24);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "src_array_layer"), 28);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "dst_x"), 32);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "dst_y"), 36);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "src_x"), 40);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "src_y"), 44);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "width"), 48);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "height"), 52);
  assert.equal(off("aerogpu_cmd_copy_texture2d", "flags"), 56);

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

  assert.equal(off("aerogpu_escape_query_device_out", "mmio_version"), 16);
  assert.equal(off("aerogpu_escape_query_device_out", "reserved0"), 20);

  assert.equal(off("aerogpu_escape_query_device_v2_out", "detected_mmio_magic"), 16);
  assert.equal(off("aerogpu_escape_query_device_v2_out", "abi_version_u32"), 20);
  assert.equal(off("aerogpu_escape_query_device_v2_out", "features_lo"), 24);
  assert.equal(off("aerogpu_escape_query_device_v2_out", "features_hi"), 32);
  assert.equal(off("aerogpu_escape_query_device_v2_out", "reserved0"), 40);

  assert.equal(off("aerogpu_escape_query_fence_out", "last_submitted_fence"), 16);
  assert.equal(off("aerogpu_escape_query_fence_out", "last_completed_fence"), 24);
  assert.equal(off("aerogpu_escape_query_fence_out", "error_irq_count"), 32);
  assert.equal(off("aerogpu_escape_query_fence_out", "last_error_fence"), 40);

  assert.equal(off("aerogpu_escape_query_perf_out", "last_submitted_fence"), 16);
  assert.equal(off("aerogpu_escape_query_perf_out", "last_completed_fence"), 24);
  assert.equal(off("aerogpu_escape_query_perf_out", "ring0_head"), 32);
  assert.equal(off("aerogpu_escape_query_perf_out", "ring0_tail"), 36);
  assert.equal(off("aerogpu_escape_query_perf_out", "ring0_size_bytes"), 40);
  assert.equal(off("aerogpu_escape_query_perf_out", "ring0_entry_count"), 44);
  assert.equal(off("aerogpu_escape_query_perf_out", "total_submissions"), 48);
  assert.equal(off("aerogpu_escape_query_perf_out", "total_presents"), 56);
  assert.equal(off("aerogpu_escape_query_perf_out", "total_render_submits"), 64);
  assert.equal(off("aerogpu_escape_query_perf_out", "total_internal_submits"), 72);
  assert.equal(off("aerogpu_escape_query_perf_out", "irq_fence_delivered"), 80);
  assert.equal(off("aerogpu_escape_query_perf_out", "irq_vblank_delivered"), 88);
  assert.equal(off("aerogpu_escape_query_perf_out", "irq_spurious"), 96);
  assert.equal(off("aerogpu_escape_query_perf_out", "reset_from_timeout_count"), 104);
  assert.equal(off("aerogpu_escape_query_perf_out", "last_reset_time_100ns"), 112);
  assert.equal(off("aerogpu_escape_query_perf_out", "vblank_seq"), 120);
  assert.equal(off("aerogpu_escape_query_perf_out", "last_vblank_time_ns"), 128);
  assert.equal(off("aerogpu_escape_query_perf_out", "vblank_period_ns"), 136);
  assert.equal(off("aerogpu_escape_query_perf_out", "reserved0"), 140);
  assert.equal(off("aerogpu_escape_query_perf_out", "error_irq_count"), 144);
  assert.equal(off("aerogpu_escape_query_perf_out", "last_error_fence"), 152);
  assert.equal(off("aerogpu_escape_query_perf_out", "ring_push_failures"), 160);
  assert.equal(off("aerogpu_escape_query_perf_out", "selftest_count"), 168);
  assert.equal(off("aerogpu_escape_query_perf_out", "selftest_last_error_code"), 176);
  assert.equal(off("aerogpu_escape_query_perf_out", "flags"), 180);
  assert.equal(off("aerogpu_escape_query_perf_out", "pending_meta_handle_count"), 184);
  assert.equal(off("aerogpu_escape_query_perf_out", "pending_meta_handle_reserved0"), 188);
  assert.equal(off("aerogpu_escape_query_perf_out", "pending_meta_handle_bytes"), 192);
  assert.equal(off("aerogpu_escape_query_perf_out", "get_scanline_cache_hits"), 200);
  assert.equal(off("aerogpu_escape_query_perf_out", "get_scanline_mmio_polls"), 208);
  assert.equal(off("aerogpu_escape_query_perf_out", "contig_pool_hit"), 216);
  assert.equal(off("aerogpu_escape_query_perf_out", "contig_pool_miss"), 224);
  assert.equal(off("aerogpu_escape_query_perf_out", "contig_pool_bytes_saved"), 232);
  assert.equal(off("aerogpu_escape_query_perf_out", "alloc_table_count"), 240);
  assert.equal(off("aerogpu_escape_query_perf_out", "alloc_table_entries"), 248);
  assert.equal(off("aerogpu_escape_query_perf_out", "alloc_table_readonly_entries"), 256);

  assert.equal(off("aerogpu_dbgctl_ring_desc", "signal_fence"), 0);
  assert.equal(off("aerogpu_dbgctl_ring_desc", "cmd_gpa"), 8);
  assert.equal(off("aerogpu_dbgctl_ring_desc", "cmd_size_bytes"), 16);
  assert.equal(off("aerogpu_dbgctl_ring_desc", "flags"), 20);

  assert.equal(off("aerogpu_escape_dump_ring_inout", "ring_id"), 16);
  assert.equal(off("aerogpu_escape_dump_ring_inout", "ring_size_bytes"), 20);
  assert.equal(off("aerogpu_escape_dump_ring_inout", "head"), 24);
  assert.equal(off("aerogpu_escape_dump_ring_inout", "tail"), 28);
  assert.equal(off("aerogpu_escape_dump_ring_inout", "desc_count"), 32);
  assert.equal(off("aerogpu_escape_dump_ring_inout", "desc_capacity"), 36);
  assert.equal(off("aerogpu_escape_dump_ring_inout", "desc"), 40);

  assert.equal(off("aerogpu_dbgctl_ring_desc_v2", "fence"), 0);
  assert.equal(off("aerogpu_dbgctl_ring_desc_v2", "cmd_gpa"), 8);
  assert.equal(off("aerogpu_dbgctl_ring_desc_v2", "cmd_size_bytes"), 16);
  assert.equal(off("aerogpu_dbgctl_ring_desc_v2", "flags"), 20);
  assert.equal(off("aerogpu_dbgctl_ring_desc_v2", "alloc_table_gpa"), 24);
  assert.equal(off("aerogpu_dbgctl_ring_desc_v2", "alloc_table_size_bytes"), 32);
  assert.equal(off("aerogpu_dbgctl_ring_desc_v2", "reserved0"), 36);

  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "ring_id"), 16);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "ring_format"), 20);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "ring_size_bytes"), 24);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "head"), 28);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "tail"), 32);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "desc_count"), 36);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "desc_capacity"), 40);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "reserved0"), 44);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "reserved1"), 48);
  assert.equal(off("aerogpu_escape_dump_ring_v2_inout", "desc"), 52);

  assert.equal(off("aerogpu_escape_selftest_inout", "timeout_ms"), 16);
  assert.equal(off("aerogpu_escape_selftest_inout", "passed"), 20);
  assert.equal(off("aerogpu_escape_selftest_inout", "error_code"), 24);
  assert.equal(off("aerogpu_escape_selftest_inout", "reserved0"), 28);

  assert.equal(off("aerogpu_escape_query_vblank_out", "vidpn_source_id"), 16);
  assert.equal(off("aerogpu_escape_query_vblank_out", "irq_enable"), 20);
  assert.equal(off("aerogpu_escape_query_vblank_out", "irq_status"), 24);
  assert.equal(off("aerogpu_escape_query_vblank_out", "flags"), 28);
  assert.equal(off("aerogpu_escape_query_vblank_out", "vblank_seq"), 32);
  assert.equal(off("aerogpu_escape_query_vblank_out", "last_vblank_time_ns"), 40);
  assert.equal(off("aerogpu_escape_query_vblank_out", "vblank_period_ns"), 48);
  assert.equal(off("aerogpu_escape_query_vblank_out", "vblank_interrupt_type"), 52);

  assert.equal(off("aerogpu_escape_query_scanout_out", "vidpn_source_id"), 16);
  assert.equal(off("aerogpu_escape_query_scanout_out", "reserved0"), 20);
  assert.equal(off("aerogpu_escape_query_scanout_out", "cached_enable"), 24);
  assert.equal(off("aerogpu_escape_query_scanout_out", "cached_width"), 28);
  assert.equal(off("aerogpu_escape_query_scanout_out", "cached_height"), 32);
  assert.equal(off("aerogpu_escape_query_scanout_out", "cached_format"), 36);
  assert.equal(off("aerogpu_escape_query_scanout_out", "cached_pitch_bytes"), 40);
  assert.equal(off("aerogpu_escape_query_scanout_out", "mmio_enable"), 44);
  assert.equal(off("aerogpu_escape_query_scanout_out", "mmio_width"), 48);
  assert.equal(off("aerogpu_escape_query_scanout_out", "mmio_height"), 52);
  assert.equal(off("aerogpu_escape_query_scanout_out", "mmio_format"), 56);
  assert.equal(off("aerogpu_escape_query_scanout_out", "mmio_pitch_bytes"), 60);
  assert.equal(off("aerogpu_escape_query_scanout_out", "mmio_fb_gpa"), 64);
  assert.equal(off("aerogpu_escape_query_scanout_out_v2", "cached_fb_gpa"), 72);

  assert.equal(off("aerogpu_escape_query_cursor_out", "flags"), 16);
  assert.equal(off("aerogpu_escape_query_cursor_out", "reserved0"), 20);
  assert.equal(off("aerogpu_escape_query_cursor_out", "enable"), 24);
  assert.equal(off("aerogpu_escape_query_cursor_out", "x"), 28);
  assert.equal(off("aerogpu_escape_query_cursor_out", "y"), 32);
  assert.equal(off("aerogpu_escape_query_cursor_out", "hot_x"), 36);
  assert.equal(off("aerogpu_escape_query_cursor_out", "hot_y"), 40);
  assert.equal(off("aerogpu_escape_query_cursor_out", "width"), 44);
  assert.equal(off("aerogpu_escape_query_cursor_out", "height"), 48);
  assert.equal(off("aerogpu_escape_query_cursor_out", "format"), 52);
  assert.equal(off("aerogpu_escape_query_cursor_out", "fb_gpa"), 56);
  assert.equal(off("aerogpu_escape_query_cursor_out", "pitch_bytes"), 64);
  assert.equal(off("aerogpu_escape_query_cursor_out", "reserved1"), 68);

  assert.equal(off("aerogpu_escape_set_cursor_position_in", "x"), 16);
  assert.equal(off("aerogpu_escape_set_cursor_position_in", "y"), 20);
  assert.equal(off("aerogpu_escape_set_cursor_visibility_in", "visible"), 16);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "width"), 16);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "height"), 20);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "hot_x"), 24);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "hot_y"), 28);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "pitch_bytes"), 32);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "format"), 36);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "reserved0"), 40);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "reserved1"), 44);
  assert.equal(off("aerogpu_escape_set_cursor_shape_in", "pixels"), 48);
  assert.equal(off("aerogpu_escape_map_shared_handle_inout", "shared_handle"), 16);
  assert.equal(off("aerogpu_escape_map_shared_handle_inout", "debug_token"), 24);
  assert.equal(off("aerogpu_escape_map_shared_handle_inout", "share_token"), 24);
  assert.equal(off("aerogpu_escape_map_shared_handle_inout", "reserved0"), 28);

  assert.equal(off("aerogpu_escape_query_error_out", "flags"), 16);
  assert.equal(off("aerogpu_escape_query_error_out", "error_code"), 20);
  assert.equal(off("aerogpu_escape_query_error_out", "error_fence"), 24);
  assert.equal(off("aerogpu_escape_query_error_out", "error_count"), 32);
  assert.equal(off("aerogpu_escape_query_error_out", "reserved0"), 36);

  assert.equal(off("aerogpu_escape_read_gpa_inout", "gpa"), 16);
  assert.equal(off("aerogpu_escape_read_gpa_inout", "size_bytes"), 24);
  assert.equal(off("aerogpu_escape_read_gpa_inout", "reserved0"), 28);
  assert.equal(off("aerogpu_escape_read_gpa_inout", "status"), 32);
  assert.equal(off("aerogpu_escape_read_gpa_inout", "bytes_copied"), 36);
  assert.equal(off("aerogpu_escape_read_gpa_inout", "data"), 40);

  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "seq"), 0);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "call_seq"), 4);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "alloc_index"), 8);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "num_allocations"), 12);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "create_flags"), 16);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "alloc_id"), 20);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "priv_flags"), 24);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "pitch_bytes"), 28);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "share_token"), 32);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "size_bytes"), 40);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "flags_in"), 48);
  assert.equal(off("aerogpu_dbgctl_createallocation_desc", "flags_out"), 52);

  assert.equal(off("aerogpu_escape_dump_createallocation_inout", "write_index"), 16);
  assert.equal(off("aerogpu_escape_dump_createallocation_inout", "entry_count"), 20);
  assert.equal(off("aerogpu_escape_dump_createallocation_inout", "entry_capacity"), 24);
  assert.equal(off("aerogpu_escape_dump_createallocation_inout", "reserved0"), 28);
  assert.equal(off("aerogpu_escape_dump_createallocation_inout", "entries"), 32);

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
  assert.equal(konst("AEROGPU_PCI_BAR1_INDEX"), BigInt(AEROGPU_PCI_BAR1_INDEX));
  assert.equal(konst("AEROGPU_PCI_BAR1_SIZE_BYTES"), BigInt(AEROGPU_PCI_BAR1_SIZE_BYTES));
  assert.equal(
    konst("AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES"),
    BigInt(AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES),
  );

  assert.equal(konst("AEROGPU_MMIO_MAGIC"), BigInt(AEROGPU_MMIO_MAGIC));
  assert.equal(konst("AEROGPU_MMIO_REG_DOORBELL"), BigInt(AEROGPU_MMIO_REG_DOORBELL));
  assert.equal(konst("AEROGPU_FEATURE_FENCE_PAGE"), AEROGPU_FEATURE_FENCE_PAGE);
  assert.equal(konst("AEROGPU_FEATURE_CURSOR"), AEROGPU_FEATURE_CURSOR);
  assert.equal(konst("AEROGPU_FEATURE_SCANOUT"), AEROGPU_FEATURE_SCANOUT);
  assert.equal(konst("AEROGPU_FEATURE_VBLANK"), AEROGPU_FEATURE_VBLANK);
  assert.equal(konst("AEROGPU_FEATURE_TRANSFER"), AEROGPU_FEATURE_TRANSFER);
  assert.equal(konst("AEROGPU_FEATURE_ERROR_INFO"), AEROGPU_FEATURE_ERROR_INFO);
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
  assert.equal(AEROGPU_CMD_STREAM_FLAG_NONE, AerogpuCmdStreamFlags.None);

  assert.equal(konst("AEROGPU_RESOURCE_USAGE_NONE"), BigInt(AEROGPU_RESOURCE_USAGE_NONE));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER"), BigInt(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_INDEX_BUFFER"), BigInt(AEROGPU_RESOURCE_USAGE_INDEX_BUFFER));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER"), BigInt(AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_TEXTURE"), BigInt(AEROGPU_RESOURCE_USAGE_TEXTURE));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_RENDER_TARGET"), BigInt(AEROGPU_RESOURCE_USAGE_RENDER_TARGET));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL"), BigInt(AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_SCANOUT"), BigInt(AEROGPU_RESOURCE_USAGE_SCANOUT));
  assert.equal(konst("AEROGPU_RESOURCE_USAGE_STORAGE"), BigInt(AEROGPU_RESOURCE_USAGE_STORAGE));

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

  // Coverage guard: every opcode (except NOP/DEBUG_MARKER) must have a corresponding
  // `AEROGPU_CMD_*_SIZE` constant matching the C packet struct size.
  const cmdAny = aerogpuCmd as unknown as Record<string, unknown>;
  for (const cName of cOpcodeConsts) {
    if (cName === "AEROGPU_CMD_NOP" || cName === "AEROGPU_CMD_DEBUG_MARKER") {
      continue;
    }
    const suffix = cName.replace(/^AEROGPU_CMD_/, "").toLowerCase();
    const structName = `aerogpu_cmd_${suffix}`;
    const expectedSize = size(structName);
    const sizeConstName = `${cName}_SIZE`;
    const actualSize = cmdAny[sizeConstName];
    assert.equal(typeof actualSize, "number", `missing TS packet size constant ${sizeConstName}`);
    assert.equal(actualSize, expectedSize, `${sizeConstName} must match sizeof(${structName})`);
  }

  // Coverage guard: every `struct aerogpu_* { ... }` definition in `aerogpu_cmd.h` must have a
  // corresponding `AEROGPU_*_SIZE` constant matching the C struct size.
  for (const structName of parseCcmdStructDefNames()) {
    const suffix = structName.replace(/^aerogpu_/, "").toUpperCase();
    const sizeConstName = `AEROGPU_${suffix}_SIZE`;
    const expectedSize = size(structName);
    const actualSize = cmdAny[sizeConstName];
    assert.equal(typeof actualSize, "number", `missing TS struct size constant ${sizeConstName}`);
    assert.equal(actualSize, expectedSize, `${sizeConstName} must match sizeof(${structName})`);
  }

  // Coverage guard: same for `aerogpu_ring.h`.
  const ringAny = aerogpuRing as unknown as Record<string, unknown>;
  for (const structName of parseCStructDefNames(ringHeader)) {
    const suffix = structName.replace(/^aerogpu_/, "").toUpperCase();
    const sizeConstName = `AEROGPU_${suffix}_SIZE`;
    const expectedSize = size(structName);
    const actualSize = ringAny[sizeConstName];
    assert.equal(typeof actualSize, "number", `missing TS struct size constant ${sizeConstName}`);
    assert.equal(actualSize, expectedSize, `${sizeConstName} must match sizeof(${structName})`);
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
  assert.equal(konst("AEROGPU_BLEND_CONSTANT"), BigInt(AerogpuBlendFactor.Constant));
  assert.equal(konst("AEROGPU_BLEND_INV_CONSTANT"), BigInt(AerogpuBlendFactor.InvConstant));

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
  assert.equal(
    konst("AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE"),
    BigInt(AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE),
  );

  assert.equal(konst("AEROGPU_INPUT_LAYOUT_BLOB_MAGIC"), BigInt(AEROGPU_INPUT_LAYOUT_BLOB_MAGIC));
  assert.equal(konst("AEROGPU_INPUT_LAYOUT_BLOB_VERSION"), BigInt(AEROGPU_INPUT_LAYOUT_BLOB_VERSION));

  assert.equal(konst("AEROGPU_SHADER_STAGE_VERTEX"), BigInt(AerogpuShaderStage.Vertex));
  assert.equal(konst("AEROGPU_SHADER_STAGE_PIXEL"), BigInt(AerogpuShaderStage.Pixel));
  assert.equal(konst("AEROGPU_SHADER_STAGE_COMPUTE"), BigInt(AerogpuShaderStage.Compute));
  assert.equal(konst("AEROGPU_SHADER_STAGE_GEOMETRY"), BigInt(AerogpuShaderStage.Geometry));
  assert.equal(konst("AEROGPU_SHADER_STAGE_EX_NONE"), BigInt(AerogpuShaderStageEx.None));
  assert.equal(konst("AEROGPU_SHADER_STAGE_EX_GEOMETRY"), BigInt(AerogpuShaderStageEx.Geometry));
  assert.equal(konst("AEROGPU_SHADER_STAGE_EX_HULL"), BigInt(AerogpuShaderStageEx.Hull));
  assert.equal(konst("AEROGPU_SHADER_STAGE_EX_DOMAIN"), BigInt(AerogpuShaderStageEx.Domain));
  assert.equal(konst("AEROGPU_SHADER_STAGE_EX_COMPUTE"), BigInt(AerogpuShaderStageEx.Compute));

  assert.equal(konst("AEROGPU_INDEX_FORMAT_UINT16"), BigInt(AerogpuIndexFormat.Uint16));
  assert.equal(konst("AEROGPU_INDEX_FORMAT_UINT32"), BigInt(AerogpuIndexFormat.Uint32));

  assert.equal(konst("AEROGPU_TOPOLOGY_POINTLIST"), BigInt(AerogpuPrimitiveTopology.PointList));
  assert.equal(konst("AEROGPU_TOPOLOGY_LINELIST"), BigInt(AerogpuPrimitiveTopology.LineList));
  assert.equal(konst("AEROGPU_TOPOLOGY_LINESTRIP"), BigInt(AerogpuPrimitiveTopology.LineStrip));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLELIST"), BigInt(AerogpuPrimitiveTopology.TriangleList));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLESTRIP"), BigInt(AerogpuPrimitiveTopology.TriangleStrip));
  assert.equal(konst("AEROGPU_TOPOLOGY_TRIANGLEFAN"), BigInt(AerogpuPrimitiveTopology.TriangleFan));
  assert.equal(konst("AEROGPU_TOPOLOGY_LINELIST_ADJ"), BigInt(AerogpuPrimitiveTopology.LineListAdj));
  assert.equal(konst("AEROGPU_TOPOLOGY_LINESTRIP_ADJ"), BigInt(AerogpuPrimitiveTopology.LineStripAdj));
  assert.equal(
    konst("AEROGPU_TOPOLOGY_TRIANGLELIST_ADJ"),
    BigInt(AerogpuPrimitiveTopology.TriangleListAdj),
  );
  assert.equal(
    konst("AEROGPU_TOPOLOGY_TRIANGLESTRIP_ADJ"),
    BigInt(AerogpuPrimitiveTopology.TriangleStripAdj),
  );
  for (let cp = 1; cp <= 32; cp++) {
    const name = `AEROGPU_TOPOLOGY_PATCHLIST_${cp}`;
    const key = `PatchList${cp}` as keyof typeof AerogpuPrimitiveTopology;
    assert.equal(konst(name), BigInt(AerogpuPrimitiveTopology[key]));
  }

  assert.equal(konst("AEROGPU_SUBMIT_FLAG_PRESENT"), BigInt(AEROGPU_SUBMIT_FLAG_PRESENT));
  assert.equal(konst("AEROGPU_SUBMIT_FLAG_NO_IRQ"), BigInt(AEROGPU_SUBMIT_FLAG_NO_IRQ));
  assert.equal(konst("AEROGPU_SUBMIT_FLAG_NONE"), BigInt(AEROGPU_SUBMIT_FLAG_NONE));

  assert.equal(konst("AEROGPU_ALLOC_FLAG_NONE"), BigInt(AEROGPU_ALLOC_FLAG_NONE));
  assert.equal(konst("AEROGPU_ALLOC_FLAG_READONLY"), BigInt(AEROGPU_ALLOC_FLAG_READONLY));

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
  assert.equal(konst("AEROGPU_UMDPRIV_MMIO_REG_MAGIC"), BigInt(AEROGPU_UMDPRIV_MMIO_REG_MAGIC));
  assert.equal(
    konst("AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION"),
    BigInt(AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION),
  );
  assert.equal(
    konst("AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO"),
    BigInt(AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO),
  );
  assert.equal(
    konst("AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI"),
    BigInt(AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI),
  );
  assert.equal(konst("AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE"), AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE);
  assert.equal(konst("AEROGPU_UMDPRIV_FEATURE_CURSOR"), AEROGPU_UMDPRIV_FEATURE_CURSOR);
  assert.equal(konst("AEROGPU_UMDPRIV_FEATURE_SCANOUT"), AEROGPU_UMDPRIV_FEATURE_SCANOUT);
  assert.equal(konst("AEROGPU_UMDPRIV_FEATURE_VBLANK"), AEROGPU_UMDPRIV_FEATURE_VBLANK);
  assert.equal(konst("AEROGPU_UMDPRIV_FEATURE_TRANSFER"), AEROGPU_UMDPRIV_FEATURE_TRANSFER);
  assert.equal(konst("AEROGPU_UMDPRIV_FEATURE_ERROR_INFO"), AEROGPU_UMDPRIV_FEATURE_ERROR_INFO);
  assert.equal(konst("AEROGPU_UMDPRIV_FLAG_IS_LEGACY"), BigInt(AEROGPU_UMDPRIV_FLAG_IS_LEGACY));
  assert.equal(konst("AEROGPU_UMDPRIV_FLAG_HAS_VBLANK"), BigInt(AEROGPU_UMDPRIV_FLAG_HAS_VBLANK));
  assert.equal(
    konst("AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE"),
    BigInt(AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE),
  );

  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_MAGIC"), BigInt(AEROGPU_WDDM_ALLOC_PRIV_MAGIC));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_VERSION"), BigInt(AEROGPU_WDDM_ALLOC_PRIV_VERSION));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_VERSION_2"), BigInt(AEROGPU_WDDM_ALLOC_PRIV_VERSION_2));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_ID_UMD_MAX"), BigInt(AEROGPU_WDDM_ALLOC_ID_UMD_MAX));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_ID_KMD_MIN"), BigInt(AEROGPU_WDDM_ALLOC_ID_KMD_MIN));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE"), BigInt(AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED"), BigInt(AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED));
  assert.equal(
    konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE"),
    BigInt(AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE),
  );
  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING"), BigInt(AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER"), AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER);
  assert.equal(
    konst("AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH"),
    BigInt(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH),
  );
  assert.equal(
    konst("AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT"),
    BigInt(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT),
  );
  assert.equal(konst("AEROGPU_WDDM_ALLOC_KIND_UNKNOWN"), BigInt(AerogpuWddmAllocKind.Unknown));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_KIND_BUFFER"), BigInt(AerogpuWddmAllocKind.Buffer));
  assert.equal(konst("AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D"), BigInt(AerogpuWddmAllocKind.Texture2d));

  assert.equal(konst("AEROGPU_ESCAPE_VERSION"), 1n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_DEVICE"), 1n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2"), 7n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE"), 8n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION"), 9n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_SCANOUT"), 10n);

  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_FENCE"), 2n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_PERF"), 12n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_DUMP_RING"), 3n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_SELFTEST"), 4n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_VBLANK"), 5n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_DUMP_VBLANK"), 5n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_DUMP_RING_V2"), 6n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_CURSOR"), 11n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_SET_CURSOR_SHAPE"), 15n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_SET_CURSOR_POSITION"), 16n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_SET_CURSOR_VISIBILITY"), 17n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_READ_GPA"), 13n);
  assert.equal(konst("AEROGPU_ESCAPE_OP_QUERY_ERROR"), 14n);
  assert.equal(konst("AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS"), 32n);
  assert.equal(konst("AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS"), 32n);
  assert.equal(konst("AEROGPU_DBGCTL_READ_GPA_MAX_BYTES"), 4096n);

  assert.equal(konst("AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN"), 0n);
  assert.equal(konst("AEROGPU_DBGCTL_RING_FORMAT_LEGACY"), 1n);
  assert.equal(konst("AEROGPU_DBGCTL_RING_FORMAT_AGPU"), 2n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_PERF_FLAGS_VALID"), 1n << 31n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_PERF_FLAG_RING_VALID"), 1n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_PERF_FLAG_VBLANK_VALID"), 2n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_PERF_FLAG_GETSCANLINE_COUNTERS_VALID"), 1n << 2n);

  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_OK"), 0n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE"), 1n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY"), 2n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY"), 3n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES"), 4n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_TIMEOUT"), 5n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_REGS_OUT_OF_RANGE"), 6n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK"), 7n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_REGS_OUT_OF_RANGE"), 8n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_LATCHED"), 9n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_CLEARED"), 10n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_REGS_OUT_OF_RANGE"), 11n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_RW_MISMATCH"), 12n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED"), 13n);
  assert.equal(konst("AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED"), 14n);

  assert.equal(konst("AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID"), 1n << 31n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED"), 1n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID"), 2n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_SCANOUT_FLAGS_VALID"), 1n << 31n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_CACHED_FB_GPA_VALID"), 1n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED"), 2n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID"), 1n << 31n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED"), 1n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED"), 2n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID"), 1n << 31n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED"), 1n);
  assert.equal(konst("AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_LATCHED"), 2n);

  // Coverage guard: keep the C ABI dump helper in sync with the Escape headers.
  const expectedEscapeConsts = [
    ...parseCDefineConstNames(escapeHeader),
    ...parseCDefineConstNames(dbgctlEscapeHeader),
    ...parseCEnumConstNames(dbgctlEscapeHeader, "enum aerogpu_dbgctl_ring_format", "AEROGPU_DBGCTL_RING_FORMAT_"),
    ...parseCEnumConstNames(dbgctlEscapeHeader, "enum aerogpu_dbgctl_selftest_error", "AEROGPU_DBGCTL_SELFTEST_"),
  ];
  const seenEscapeConsts = [...abi.consts.keys()].filter(
    (name) => name.startsWith("AEROGPU_ESCAPE_") || name.startsWith("AEROGPU_DBGCTL_"),
  );
  assertNameSetEq(seenEscapeConsts, expectedEscapeConsts, "Escape ABI constant coverage");

  // -------------------------- Exhaustive ABI constant coverage --------------------------
  //
  // These checks are header-driven: any addition/removal/change to the canonical headers in
  // `drivers/aerogpu/protocol/` must be reflected in the TS mirrors in `emulator/protocol/aerogpu/`.
  const expectedPciConsts = [
    ...parseCDefineConstNames(pciHeader),
    ...parseCEnumConstNames(pciHeader, "enum aerogpu_error_code", "AEROGPU_ERROR_"),
    ...parseCEnumConstNames(pciHeader, "enum aerogpu_format", "AEROGPU_FORMAT_"),
  ].sort();
  const expectedRingConsts = [
    ...parseCDefineConstNames(ringHeader),
    ...parseCEnumConstNames(ringHeader, "enum aerogpu_submit_flags", "AEROGPU_SUBMIT_FLAG_"),
    ...parseCEnumConstNames(ringHeader, "enum aerogpu_engine_id", "AEROGPU_ENGINE_"),
    ...parseCEnumConstNames(ringHeader, "enum aerogpu_alloc_flags", "AEROGPU_ALLOC_FLAG_"),
  ].sort();
  const expectedCmdConsts = [
    ...parseCDefineConstNames(cmdHeader),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_cmd_stream_flags", "AEROGPU_CMD_STREAM_FLAG_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_cmd_opcode", "AEROGPU_CMD_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_shader_stage", "AEROGPU_SHADER_STAGE_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_shader_stage_ex", "AEROGPU_SHADER_STAGE_EX_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_index_format", "AEROGPU_INDEX_FORMAT_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_primitive_topology", "AEROGPU_TOPOLOGY_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_resource_usage_flags", "AEROGPU_RESOURCE_USAGE_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_copy_flags", "AEROGPU_COPY_FLAG_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_blend_factor", "AEROGPU_BLEND_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_blend_op", "AEROGPU_BLEND_OP_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_compare_func", "AEROGPU_COMPARE_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_fill_mode", "AEROGPU_FILL_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_cull_mode", "AEROGPU_CULL_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_clear_flags", "AEROGPU_CLEAR_"),
    ...parseCEnumConstNames(cmdHeader, "enum aerogpu_present_flags", "AEROGPU_PRESENT_FLAG_"),
  ].sort();
  const expectedUmdPrivateConsts = [...parseCDefineConstNames(umdPrivateHeader)].sort();
  const expectedWddmAllocConsts = [
    ...parseCDefineConstNames(wddmAllocHeader),
    ...parseCEnumConstNames(
      wddmAllocHeader,
      "enum aerogpu_wddm_alloc_private_flags",
      "AEROGPU_WDDM_ALLOC_PRIV_FLAG_",
    ),
    ...parseCEnumConstNames(wddmAllocHeader, "enum aerogpu_wddm_alloc_kind", "AEROGPU_WDDM_ALLOC_KIND_"),
  ].sort();

  // aerogpu_pci.h constants
  const pciSeen: string[] = [];
  for (const name of expectedPciConsts) {
    const expected = konst(name);
    const direct = (aerogpuPci as unknown as Record<string, unknown>)[name];
    if (direct !== undefined) {
      pciSeen.push(name);
      assert.equal(valueToBigInt(direct, name), expected, `constant value for ${name}`);
      continue;
    }
    if (name.startsWith("AEROGPU_ERROR_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_ERROR_/, ""));
      const actual = (aerogpuPci.AerogpuErrorCode as unknown as Record<string, number>)[key];
      assert.ok(actual !== undefined, `missing TS AerogpuErrorCode binding for ${name} (${key})`);
      pciSeen.push(name);
      assert.equal(BigInt(actual), expected, `error code value for ${name}`);
      continue;
    }
    if (name.startsWith("AEROGPU_FORMAT_")) {
      const key = formatCNameToTsKey(name);
      const actual = (aerogpuPci.AerogpuFormat as unknown as Record<string, number>)[key];
      assert.ok(actual !== undefined, `missing TS AerogpuFormat binding for ${name} (${key})`);
      pciSeen.push(name);
      assert.equal(BigInt(actual), expected, `format value for ${name}`);
      continue;
    }
    throw new Error(`unhandled aerogpu_pci.h constant: ${name}`);
  }
  assertNameSetEq(pciSeen, expectedPciConsts, "aerogpu_pci.h constants");

  // aerogpu_ring.h constants
  const ringSeen: string[] = [];
  for (const name of expectedRingConsts) {
    const expected = konst(name);
    const actual = ringAny[name];
    assert.notEqual(actual, undefined, `missing TS export for ${name}`);
    ringSeen.push(name);
    assert.equal(valueToBigInt(actual, name), expected, `constant value for ${name}`);
  }
  assertNameSetEq(ringSeen, expectedRingConsts, "aerogpu_ring.h constants");

  // aerogpu_cmd.h constants
  const cmdExports = aerogpuCmd as unknown as Record<string, unknown>;
  const cmdSeen: string[] = [];
  for (const name of expectedCmdConsts) {
    const expected = konst(name);

    const direct = cmdExports[name];
    if (direct !== undefined) {
      cmdSeen.push(name);
      assert.equal(valueToBigInt(direct, name), expected, `constant value for ${name}`);
      continue;
    }

    if (name.startsWith("AEROGPU_CMD_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_CMD_/, ""));
      const actual = (AerogpuCmdOpcode as unknown as Record<string, number>)[key];
      assert.ok(actual !== undefined, `missing TS opcode binding for ${name} (${key})`);
      cmdSeen.push(name);
      assert.equal(BigInt(actual), expected, `opcode value for ${name}`);
      continue;
    }

    if (name.startsWith("AEROGPU_SHADER_STAGE_EX_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_SHADER_STAGE_EX_/, ""));
      cmdSeen.push(name);
      assert.equal(BigInt((AerogpuShaderStageEx as unknown as Record<string, number>)[key]!), expected);
      continue;
    }
    if (name.startsWith("AEROGPU_SHADER_STAGE_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_SHADER_STAGE_/, ""));
      cmdSeen.push(name);
      assert.equal(BigInt((AerogpuShaderStage as unknown as Record<string, number>)[key]!), expected);
      continue;
    }
    if (name.startsWith("AEROGPU_INDEX_FORMAT_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_INDEX_FORMAT_/, ""));
      cmdSeen.push(name);
      assert.equal(BigInt((AerogpuIndexFormat as unknown as Record<string, number>)[key]!), expected);
      continue;
    }
    if (name.startsWith("AEROGPU_TOPOLOGY_")) {
      const key = topologyCNameToTsKey(name);
      cmdSeen.push(name);
      assert.equal(
        BigInt((AerogpuPrimitiveTopology as unknown as Record<string, number>)[key]!),
        expected,
        `topology value for ${name}`,
      );
      continue;
    }
    if (name.startsWith("AEROGPU_BLEND_OP_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_BLEND_OP_/, ""));
      cmdSeen.push(name);
      assert.equal(BigInt((AerogpuBlendOp as unknown as Record<string, number>)[key]!), expected);
      continue;
    }
    if (name.startsWith("AEROGPU_BLEND_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_BLEND_/, ""));
      cmdSeen.push(name);
      assert.equal(BigInt((AerogpuBlendFactor as unknown as Record<string, number>)[key]!), expected);
      continue;
    }
    if (name.startsWith("AEROGPU_COMPARE_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_COMPARE_/, ""));
      cmdSeen.push(name);
      assert.equal(BigInt((AerogpuCompareFunc as unknown as Record<string, number>)[key]!), expected);
      continue;
    }
    if (name.startsWith("AEROGPU_FILL_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_FILL_/, ""));
      cmdSeen.push(name);
      assert.equal(BigInt((AerogpuFillMode as unknown as Record<string, number>)[key]!), expected);
      continue;
    }
    if (name.startsWith("AEROGPU_CULL_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_CULL_/, ""));
      cmdSeen.push(name);
      assert.equal(BigInt((AerogpuCullMode as unknown as Record<string, number>)[key]!), expected);
      continue;
    }

    throw new Error(`unhandled aerogpu_cmd.h constant: ${name}`);
  }
  assertNameSetEq(cmdSeen, expectedCmdConsts, "aerogpu_cmd.h constants");

  // aerogpu_umd_private.h constants
  const umdPrivateExports = aerogpuUmdPrivate as unknown as Record<string, unknown>;
  const umdPrivateSeen: string[] = [];
  for (const name of expectedUmdPrivateConsts) {
    const expected = konst(name);
    const actual = umdPrivateExports[name];
    assert.notEqual(actual, undefined, `missing TS export for ${name}`);
    umdPrivateSeen.push(name);
    assert.equal(valueToBigInt(actual, name), expected, `constant value for ${name}`);
  }
  assertNameSetEq(umdPrivateSeen, expectedUmdPrivateConsts, "aerogpu_umd_private.h constants");

  // aerogpu_wddm_alloc.h constants
  const wddmAllocExports = aerogpuWddmAlloc as unknown as Record<string, unknown>;
  const wddmAllocSeen: string[] = [];
  for (const name of expectedWddmAllocConsts) {
    const expected = konst(name);

    const direct = wddmAllocExports[name];
    if (direct !== undefined) {
      wddmAllocSeen.push(name);
      assert.equal(valueToBigInt(direct, name), expected, `constant value for ${name}`);
      continue;
    }

    if (name.startsWith("AEROGPU_WDDM_ALLOC_KIND_")) {
      const key = upperSnakeToPascalCase(name.replace(/^AEROGPU_WDDM_ALLOC_KIND_/, ""));
      const actual = (AerogpuWddmAllocKind as unknown as Record<string, number>)[key];
      assert.ok(actual !== undefined, `missing TS AerogpuWddmAllocKind binding for ${name} (${key})`);
      wddmAllocSeen.push(name);
      assert.equal(BigInt(actual), expected, `wddm alloc kind value for ${name}`);
      continue;
    }

    throw new Error(`unhandled aerogpu_wddm_alloc.h constant: ${name}`);
  }
  assertNameSetEq(wddmAllocSeen, expectedWddmAllocConsts, "aerogpu_wddm_alloc.h constants");
});

test("decodeAllocTableHeader accepts unknown minor versions and extended strides", () => {
  const buf = new ArrayBuffer(AEROGPU_ALLOC_TABLE_HEADER_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_ALLOC_TABLE_HEADER_OFF_MAGIC, AEROGPU_ALLOC_TABLE_MAGIC, true);
  view.setUint32(
    AEROGPU_ALLOC_TABLE_HEADER_OFF_ABI_VERSION,
    (AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 1),
    true,
  );
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
  view.setUint32(AEROGPU_RING_HEADER_OFF_ABI_VERSION, (AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 1), true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_COUNT, 8, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_ENTRY_STRIDE_BYTES, 128, true);
  view.setUint32(AEROGPU_RING_HEADER_OFF_SIZE_BYTES, 64 + 8 * 128, true);

  const hdr = decodeRingHeader(view, 0);
  assert.equal(hdr.entryCount, 8);
  assert.equal(hdr.entryStrideBytes, 128);
  assert.equal(hdr.abiVersion, (AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 1));
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

test("parseAndValidateAbiVersionU32 rejects unknown major versions", () => {
  const versionU32 = ((AEROGPU_ABI_MAJOR + 1) << 16) | AEROGPU_ABI_MINOR;
  assert.throws(() => parseAndValidateAbiVersionU32(versionU32), AerogpuAbiError);
});

test("parseAndValidateAbiVersionU32 accepts unknown minor versions", () => {
  const versionU32 = (AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 1);
  const parsed = parseAndValidateAbiVersionU32(versionU32);
  assert.equal(parsed.major, AEROGPU_ABI_MAJOR);
  assert.equal(parsed.minor, AEROGPU_ABI_MINOR + 1);
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
