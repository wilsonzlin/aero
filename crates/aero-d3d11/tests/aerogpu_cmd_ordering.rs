use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::{GuestMemory, GuestMemoryError, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_COPY_FLAG_WRITEBACK_DST, AEROGPU_RESOURCE_USAGE_TEXTURE,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use std::cell::Cell;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn begin_cmd(stream: &mut Vec<u8>, opcode: u32) -> usize {
    let start = stream.len();
    stream.extend_from_slice(&opcode.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size placeholder
    start
}

fn end_cmd(stream: &mut Vec<u8>, start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

struct SwitchingGuestMemory {
    inner: VecGuestMemory,
    src_gpa: u64,
    src_len: usize,
    pattern_a: u8,
    pattern_b: u8,
    upload_starts: Cell<u32>,
    active_pattern: Cell<u8>,
}

impl SwitchingGuestMemory {
    fn new(
        inner: VecGuestMemory,
        src_gpa: u64,
        src_len: usize,
        pattern_a: u8,
        pattern_b: u8,
    ) -> Self {
        Self {
            inner,
            src_gpa,
            src_len,
            pattern_a,
            pattern_b,
            upload_starts: Cell::new(0),
            active_pattern: Cell::new(pattern_a),
        }
    }
}

impl GuestMemory for SwitchingGuestMemory {
    fn read(&self, gpa: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        // Delegate to VecGuestMemory for bounds checking, then overwrite the designated upload range.
        self.inner.read(gpa, dst)?;

        if gpa == self.src_gpa {
            let idx = self.upload_starts.get();
            let next = if idx == 0 {
                self.pattern_a
            } else {
                self.pattern_b
            };
            self.active_pattern.set(next);
            self.upload_starts.set(idx.saturating_add(1));
        }

        let read_end = gpa.checked_add(dst.len() as u64).ok_or(GuestMemoryError {
            gpa,
            len: dst.len(),
        })?;
        let src_end = self
            .src_gpa
            .checked_add(self.src_len as u64)
            .ok_or(GuestMemoryError {
                gpa: self.src_gpa,
                len: self.src_len,
            })?;

        let overlap_start = self.src_gpa.max(gpa);
        let overlap_end = src_end.min(read_end);
        if overlap_start < overlap_end {
            let dst_offset = (overlap_start - gpa) as usize;
            let overlap_len = (overlap_end - overlap_start) as usize;
            dst[dst_offset..dst_offset + overlap_len].fill(self.active_pattern.get());
        }

        Ok(())
    }

    fn write(&self, gpa: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        self.inner.write(gpa, src)
    }
}

#[test]
fn aerogpu_cmd_preserves_upload_copy_ordering() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu_cmd ordering test");
                return;
            }
        };

        const SRC: u32 = 1;
        const DST1: u32 = 2;
        const DST2: u32 = 3;

        let width = 4u32;
        let height = 4u32;
        let total_bytes = (width as usize) * (height as usize) * 4;

        let pattern_a = vec![0x11u8; total_bytes];
        let pattern_b = vec![0x22u8; total_bytes];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for &handle in &[SRC, DST1, DST2] {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // UPLOAD_RESOURCE(src, patternA)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(pattern_a.len() as u64).to_le_bytes());
        stream.extend_from_slice(&pattern_a);
        stream.resize(
            stream.len() + (align4(pattern_a.len()) - pattern_a.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D(dst1 <- src)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&DST1.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE(src, patternB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(pattern_b.len() as u64).to_le_bytes());
        stream.extend_from_slice(&pattern_b);
        stream.resize(
            stream.len() + (align4(pattern_b.len()) - pattern_b.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D(dst2 <- src)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&DST2.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let got_dst1 = exec.read_texture_rgba8(DST1).await.expect("read back dst1");
        let got_dst2 = exec.read_texture_rgba8(DST2).await.expect("read back dst2");

        assert_eq!(got_dst1, pattern_a, "dst1 should match first upload");
        assert_eq!(got_dst2, pattern_b, "dst2 should match second upload");
    });
}

#[test]
fn aerogpu_cmd_preserves_dirty_range_upload_ordering_for_buffers() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!(
                    "wgpu unavailable ({e:#}); skipping aerogpu_cmd dirty-range buffer ordering test"
                );
                return;
            }
        };

        const SRC: u32 = 20;
        const DST1: u32 = 21;
        const DST2: u32 = 22;
        const RB1: u32 = 23;
        const RB2: u32 = 24;

        let buf_size = 16u64;
        let pattern_a = vec![0x11u8; buf_size as usize];
        let pattern_b = vec![0x22u8; buf_size as usize];

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let src_offset = 0u32;
        let rb1_offset = 0x100u32;
        let rb2_offset = 0x200u32;

        let src_gpa = alloc.gpa + src_offset as u64;
        let guest_mem = SwitchingGuestMemory::new(
            VecGuestMemory::new(0x2000),
            src_gpa,
            buf_size as usize,
            0x11,
            0x22,
        );

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (SRC, guest-backed)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&src_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (DST1/DST2, host)
        for &handle in &[DST1, DST2] {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&buf_size.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // CREATE_BUFFER (RB1/RB2, guest-backed for WRITEBACK_DST)
        for (handle, backing_offset) in [(RB1, rb1_offset), (RB2, rb2_offset)] {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&buf_size.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&backing_offset.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // COPY_BUFFER(dst1 <- src) (implicit upload -> pattern A)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST1.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE(src) (forces second implicit upload)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // COPY_BUFFER(dst2 <- src) (implicit upload -> pattern B)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST2.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER(rb1 <- dst1) (WRITEBACK_DST)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&RB1.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&DST1.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER(rb2 <- dst2) (WRITEBACK_DST)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&RB2.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&DST2.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let mem = guest_mem.inner.as_slice();
        let rb1_base = (alloc.gpa + rb1_offset as u64) as usize;
        let rb2_base = (alloc.gpa + rb2_offset as u64) as usize;

        assert_eq!(
            &mem[rb1_base..rb1_base + buf_size as usize],
            pattern_a.as_slice(),
            "rb1 should match first upload",
        );
        assert_eq!(
            &mem[rb2_base..rb2_base + buf_size as usize],
            pattern_b.as_slice(),
            "rb2 should match second upload",
        );
    });
}

#[test]
fn aerogpu_cmd_preserves_dirty_range_upload_ordering_for_textures() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!(
                    "wgpu unavailable ({e:#}); skipping aerogpu_cmd dirty-range texture ordering test"
                );
                return;
            }
        };

        const SRC: u32 = 30;
        const DST1: u32 = 31;
        const DST2: u32 = 32;

        let width = 4u32;
        let height = 4u32;
        let row_pitch = width * 4;
        let total_bytes = (row_pitch * height) as usize;

        let pattern_a = vec![0x11u8; total_bytes];
        let pattern_b = vec![0x22u8; total_bytes];

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let src_gpa = alloc.gpa;
        let guest_mem = SwitchingGuestMemory::new(
            VecGuestMemory::new(0x2000),
            src_gpa,
            total_bytes,
            0x11,
            0x22,
        );

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D(src guest-backed + dst host)
        for (handle, backing_alloc_id, backing_offset_bytes, row_pitch_bytes) in [
            (SRC, alloc.alloc_id, 0u32, row_pitch),
            (DST1, 0u32, 0u32, 0u32),
            (DST2, 0u32, 0u32, 0u32),
        ] {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&row_pitch_bytes.to_le_bytes());
            stream.extend_from_slice(&backing_alloc_id.to_le_bytes());
            stream.extend_from_slice(&backing_offset_bytes.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // COPY_TEXTURE2D(dst1 <- src) (implicit upload -> pattern A)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&DST1.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE(src) (forces second implicit upload)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(total_bytes as u64).to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D(dst2 <- src) (implicit upload -> pattern B)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&DST2.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let got_dst1 = exec.read_texture_rgba8(DST1).await.expect("read back dst1");
        let got_dst2 = exec.read_texture_rgba8(DST2).await.expect("read back dst2");

        assert_eq!(got_dst1, pattern_a, "dst1 should match first upload");
        assert_eq!(got_dst2, pattern_b, "dst2 should match second upload");
    });
}

#[test]
fn aerogpu_cmd_preserves_upload_copy_ordering_for_buffers() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu_cmd buffer ordering test");
                return;
            }
        };

        const SRC: u32 = 10;
        const DST1: u32 = 11;
        const DST2: u32 = 12;
        const RB1: u32 = 13;
        const RB2: u32 = 14;

        let buf_size = 16u64;
        let pattern_a = vec![0x11u8; buf_size as usize];
        let pattern_b = vec![0x22u8; buf_size as usize];

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];
        let rb1_offset = 0u32;
        let rb2_offset = 0x100u32;

        let guest_mem = VecGuestMemory::new(0x2000);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (host allocated)
        for &handle in &[SRC, DST1, DST2] {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&buf_size.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // CREATE_BUFFER readback buffers (guest-backed)
        for (handle, backing_offset) in [(RB1, rb1_offset), (RB2, rb2_offset)] {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&buf_size.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&backing_offset.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // UPLOAD_RESOURCE(src, patternA)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(pattern_a.len() as u64).to_le_bytes());
        stream.extend_from_slice(&pattern_a);
        stream.resize(
            stream.len() + (align4(pattern_a.len()) - pattern_a.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // COPY_BUFFER(dst1 <- src)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST1.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE(src, patternB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(pattern_b.len() as u64).to_le_bytes());
        stream.extend_from_slice(&pattern_b);
        stream.resize(
            stream.len() + (align4(pattern_b.len()) - pattern_b.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // COPY_BUFFER(dst2 <- src)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST2.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER(readback1 <- dst1) (WRITEBACK_DST)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&RB1.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&DST1.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER(readback2 <- dst2) (WRITEBACK_DST)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&RB2.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&DST2.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let mem = guest_mem.as_slice();
        let rb1_base = (alloc.gpa + rb1_offset as u64) as usize;
        let rb2_base = (alloc.gpa + rb2_offset as u64) as usize;

        assert_eq!(
            &mem[rb1_base..rb1_base + buf_size as usize],
            pattern_a.as_slice(),
            "rb1 should match first upload",
        );
        assert_eq!(
            &mem[rb2_base..rb2_base + buf_size as usize],
            pattern_b.as_slice(),
            "rb2 should match second upload",
        );
    });
}
