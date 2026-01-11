#![cfg(target_arch = "wasm32")]

use aero_gpu_wasm::{
    clear_guest_memory, destroy_gpu, init_aerogpu_d3d9, read_guest_memory, set_guest_memory,
    submit_aerogpu_d3d9,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AEROGPU_ALLOC_TABLE_MAGIC};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use js_sys::Uint8Array;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

const GUEST_LEN: u32 = 0x4000;

const ALLOC_ID: u32 = 1;
const ALLOC_GPA: u64 = 0x1000;
const ALLOC_SIZE: u64 = 0x3000;

fn build_single_alloc_table(alloc_id: u32, gpa: u64, size_bytes: u64) -> Uint8Array {
    let mut bytes = Vec::new();

    // aerogpu_alloc_table_header (24 bytes)
    bytes.extend_from_slice(&AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patch later)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // entry_count
    bytes.extend_from_slice(&(AerogpuAllocEntry::SIZE_BYTES as u32).to_le_bytes()); // stride
    bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    // aerogpu_alloc_entry (32 bytes)
    bytes.extend_from_slice(&alloc_id.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
    bytes.extend_from_slice(&gpa.to_le_bytes());
    bytes.extend_from_slice(&size_bytes.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes()); // reserved0

    let total_size_bytes: u32 = bytes
        .len()
        .try_into()
        .expect("alloc table should fit in u32 size_bytes");
    bytes[8..12].copy_from_slice(&total_size_bytes.to_le_bytes());
    Uint8Array::from(bytes.as_slice())
}

async fn init_webgpu_or_panic() {
    if let Err(err) = init_aerogpu_d3d9(None, None).await {
        // These regression tests specifically validate WRITEBACK_DST on the headless WebGPU path.
        panic!(
            "init_aerogpu_d3d9(None, None) failed; these tests require a browser with WebGPU enabled (e.g. `wasm-pack test --headless --chrome`). Error: {}",
            js_value_to_string(err)
        );
    }
}

fn js_value_to_string(v: JsValue) -> String {
    v.as_string().unwrap_or_else(|| format!("{v:?}"))
}

fn read_guest_all() -> Vec<u8> {
    let u8 = read_guest_memory(0, GUEST_LEN).expect("read_guest_memory");
    let mut out = vec![0u8; GUEST_LEN as usize];
    u8.copy_to(&mut out);
    out
}

#[wasm_bindgen_test(async)]
async fn copy_buffer_writeback_updates_guest_memory() {
    destroy_gpu().expect("destroy_gpu (pre)");
    clear_guest_memory();

    init_webgpu_or_panic().await;

    let alloc_table = build_single_alloc_table(ALLOC_ID, ALLOC_GPA, ALLOC_SIZE);

    const SRC: u32 = 1;
    const DST: u32 = 2;
    const BUFFER_SIZE: u64 = 0x200;
    const SRC_OFFSET: u32 = 0x000;
    const DST_OFFSET: u32 = 0x800;
    const SRC_COPY_OFF: u64 = 0x40;
    const DST_COPY_OFF: u64 = 0x80;
    const COPY_SIZE: u64 = 0x40;

    let mut guest = vec![0xAAu8; GUEST_LEN as usize];

    let src_base = (ALLOC_GPA + SRC_OFFSET as u64) as usize;
    let dst_base = (ALLOC_GPA + DST_OFFSET as u64) as usize;
    guest[src_base..src_base + BUFFER_SIZE as usize].fill(0x11);
    guest[dst_base..dst_base + BUFFER_SIZE as usize].fill(0xCC);

    let src_copy_base = src_base + SRC_COPY_OFF as usize;
    for (i, b) in guest[src_copy_base..src_copy_base + COPY_SIZE as usize]
        .iter_mut()
        .enumerate()
    {
        *b = i as u8;
    }

    let mut expected = guest.clone();
    let dst_copy_base = dst_base + DST_COPY_OFF as usize;
    expected[dst_copy_base..dst_copy_base + COPY_SIZE as usize].copy_from_slice(
        &guest[src_copy_base..src_copy_base + COPY_SIZE as usize],
    );

    let guest_u8 = Uint8Array::from(guest.as_slice());
    set_guest_memory(guest_u8);

    let mut writer = AerogpuCmdWriter::new();
    writer.create_buffer(SRC, 0, BUFFER_SIZE, ALLOC_ID, SRC_OFFSET);
    writer.create_buffer(DST, 0, BUFFER_SIZE, ALLOC_ID, DST_OFFSET);
    writer.resource_dirty_range(SRC, 0, BUFFER_SIZE);
    writer.copy_buffer_writeback_dst(DST, SRC, DST_COPY_OFF, SRC_COPY_OFF, COPY_SIZE);
    let cmd_bytes = writer.finish();

    submit_aerogpu_d3d9(Uint8Array::from(cmd_bytes.as_slice()), 1, 0, Some(alloc_table))
        .await
        .expect("submit_aerogpu_d3d9");

    let after = read_guest_all();
    assert_eq!(after, expected, "guest memory should reflect COPY_BUFFER writeback");

    destroy_gpu().expect("destroy_gpu (post)");
    clear_guest_memory();
}

#[wasm_bindgen_test(async)]
async fn copy_texture2d_writeback_updates_guest_memory() {
    destroy_gpu().expect("destroy_gpu (pre)");
    clear_guest_memory();

    init_webgpu_or_panic().await;

    let alloc_table = build_single_alloc_table(ALLOC_ID, ALLOC_GPA, ALLOC_SIZE);

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const TEX_W: u32 = 4;
    const TEX_H: u32 = 4;
    const BPP: u32 = 4;
    const ROW_PITCH: u32 = 32;
    const BACKING_SIZE: u64 = ROW_PITCH as u64 * TEX_H as u64;

    const SRC_TEX_OFFSET: u32 = 0x000;
    const DST_TEX_OFFSET: u32 = 0x400;

    const SRC_X: u32 = 1;
    const SRC_Y: u32 = 1;
    const DST_X: u32 = 1;
    const DST_Y: u32 = 1;
    const COPY_W: u32 = 2;
    const COPY_H: u32 = 2;

    let mut guest = vec![0xAAu8; GUEST_LEN as usize];

    let src_base = (ALLOC_GPA + SRC_TEX_OFFSET as u64) as usize;
    let dst_base = (ALLOC_GPA + DST_TEX_OFFSET as u64) as usize;
    guest[src_base..src_base + BACKING_SIZE as usize].fill(0x55);
    guest[dst_base..dst_base + BACKING_SIZE as usize].fill(0xEE);

    // Fill src texels with a deterministic per-coordinate pattern. Padding bytes per row are left
    // as 0x55 so row_pitch handling is exercised.
    for y in 0..TEX_H {
        for x in 0..TEX_W {
            let px = [
                x as u8,
                y as u8,
                (x ^ y) as u8,
                0xFFu8, // alpha
            ];
            let off = src_base
                + (y as usize * ROW_PITCH as usize)
                + (x as usize * BPP as usize);
            guest[off..off + 4].copy_from_slice(&px);
        }
    }

    let mut expected = guest.clone();
    let bytes_per_row = COPY_W * BPP;
    for row in 0..COPY_H {
        let src_row = (SRC_Y + row) as usize;
        let dst_row = (DST_Y + row) as usize;
        let src_off = src_base
            + src_row * ROW_PITCH as usize
            + (SRC_X * BPP) as usize;
        let dst_off = dst_base
            + dst_row * ROW_PITCH as usize
            + (DST_X * BPP) as usize;
        expected[dst_off..dst_off + bytes_per_row as usize]
            .copy_from_slice(&guest[src_off..src_off + bytes_per_row as usize]);
    }

    let guest_u8 = Uint8Array::from(guest.as_slice());
    set_guest_memory(guest_u8);

    let mut writer = AerogpuCmdWriter::new();
    let format = AerogpuFormat::R8G8B8A8Unorm as u32;
    writer.create_texture2d(
        SRC_TEX,
        0,
        format,
        TEX_W,
        TEX_H,
        1,
        1,
        ROW_PITCH,
        ALLOC_ID,
        SRC_TEX_OFFSET,
    );
    writer.create_texture2d(
        DST_TEX,
        0,
        format,
        TEX_W,
        TEX_H,
        1,
        1,
        ROW_PITCH,
        ALLOC_ID,
        DST_TEX_OFFSET,
    );
    writer.resource_dirty_range(SRC_TEX, 0, BACKING_SIZE);
    writer.copy_texture2d_writeback_dst(
        DST_TEX, SRC_TEX, 0, 0, 0, 0, DST_X, DST_Y, SRC_X, SRC_Y, COPY_W, COPY_H,
    );
    let cmd_bytes = writer.finish();

    submit_aerogpu_d3d9(Uint8Array::from(cmd_bytes.as_slice()), 1, 0, Some(alloc_table))
        .await
        .expect("submit_aerogpu_d3d9");

    let after = read_guest_all();
    assert_eq!(
        after, expected,
        "guest memory should reflect COPY_TEXTURE2D writeback (row_pitch aware)"
    );

    destroy_gpu().expect("destroy_gpu (post)");
    clear_guest_memory();
}

