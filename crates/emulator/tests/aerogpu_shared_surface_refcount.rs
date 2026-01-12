use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdOpcode, AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR,
    AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuAllocEntry as ProtocolAllocEntry, AerogpuAllocTableHeader as ProtocolAllocTableHeader,
    AEROGPU_ALLOC_TABLE_MAGIC,
};
use emulator::devices::aerogpu_regs::{irq_bits, AeroGpuRegs};
use emulator::devices::aerogpu_ring::AeroGpuSubmitDesc;
use emulator::gpu_worker::aerogpu_software::AeroGpuSoftwareExecutor;
use memory::MemoryBus;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = 4;

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let range = self.range(paddr, buf.len());
        buf.copy_from_slice(&self.data[range]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let range = self.range(paddr, buf.len());
        self.data[range].copy_from_slice(buf);
    }
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert_eq!(size_bytes % 4, 0);
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>), abi_version: u32) -> Vec<u8> {
    let mut out = Vec::new();
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, abi_version);
    push_u32(&mut out, 0); // size_bytes placeholder
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

fn write_alloc_table(
    mem: &mut VecMemory,
    gpa: u64,
    abi_version: u32,
    backing_gpa: u64,
    backing_size: u64,
) -> u32 {
    let size_bytes = (ProtocolAllocTableHeader::SIZE_BYTES + ProtocolAllocEntry::SIZE_BYTES) as u32;
    let mut buf = Vec::with_capacity(size_bytes as usize);

    push_u32(&mut buf, AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut buf, abi_version);
    push_u32(&mut buf, size_bytes);
    push_u32(&mut buf, 1); // entry_count
    push_u32(&mut buf, ProtocolAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut buf, 0); // reserved0

    push_u32(&mut buf, 1); // alloc_id
    push_u32(&mut buf, 0); // flags
    push_u64(&mut buf, backing_gpa);
    push_u64(&mut buf, backing_size);
    push_u64(&mut buf, 0); // reserved0

    assert_eq!(buf.len(), size_bytes as usize);
    mem.write_physical(gpa, &buf);
    size_bytes
}

#[test]
fn shared_surface_alias_survives_destroy_of_original_handle() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0x1234_5678u64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // Set RT to the original handle and clear red.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 1); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            // Release the original handle; alias must keep the underlying texture alive.
            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });

            // Switch RT to the imported alias and clear green.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 2); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 0);
    assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}

#[test]
fn shared_surface_import_is_idempotent_for_existing_alias_handle() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let cmd_gpa = 0x2000u64;
    let share_token = 0x2222_3333u64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 2); // width
                push_u32(out, 2); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            // Import once...
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            // ...and again with the same output handle (should be a no-op, not a refcount bump).
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });
            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
            });

            // If the import above incorrectly incremented the refcount twice, the underlying
            // texture would still be alive (and mapped) here. Correct behavior is that the
            // final import fails because the token mapping is dropped when the last handle is
            // destroyed.
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 3); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa: 0,
        alloc_table_size_bytes: 0,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn shared_surface_reusing_original_handle_while_alias_alive_is_an_error() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0x1111_2222u64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });

            // Reusing the now-destroyed original handle would overwrite the underlying texture
            // still referenced by the alias. The software executor must reject this.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // The original texture should still be writable via the alias.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 2); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}

#[test]
fn shared_surface_import_into_destroyed_original_handle_is_an_error() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0xAAAA_BBBBu64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            // Destroy the original handle; alias keeps the underlying alive.
            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });

            // Buggy guest behavior: attempt to re-import into the destroyed original handle ID.
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 1); // out_resource_handle (destroyed original)
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            // The underlying texture should still be writable via the existing alias.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 2); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}

#[test]
fn shared_surface_double_destroy_of_original_handle_is_idempotent() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0x0BAD_F00Du64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            // Destroy original handle twice; the second destroy must be an idempotent no-op.
            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });
            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 2); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 0);
    assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}

#[test]
fn shared_surface_set_render_targets_using_destroyed_original_handle_is_an_error() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0xD00D_F00Du64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // Clear original handle red.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 1); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });

            // Create an alias to keep the underlying alive.
            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            // Destroy the original handle; the underlying remains alive via handle 2.
            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });

            // Buggy guest behavior: bind the destroyed original handle as a render target.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 1); // colors[0] (destroyed original)
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            // Attempt to clear blue; this must be ignored (no-op) because RT is invalid.
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    // The last clear should have been ignored; backing memory stays red.
    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [255, 0, 0, 255]);
    }
}

#[test]
fn shared_surface_upload_using_destroyed_original_handle_is_an_error() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0xBEEF_1234u64;
    let upload_size = (width * height * 4) as u64;
    let upload_size_u32: u32 = upload_size.try_into().unwrap();

    let blue: Vec<u8> = (0..(width * height))
        .flat_map(|_| [0u8, 0u8, 255u8, 255u8])
        .collect();

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // Clear original handle red.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 1); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });

            // Keep the underlying alive via an alias.
            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            // Destroy the original handle.
            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });

            // Buggy guest behavior: upload into the destroyed original handle ID.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle (destroyed original)
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, upload_size);
                out.extend_from_slice(&blue);
                // Payload must be 4-byte aligned.
                assert_eq!(upload_size_u32 % 4, 0);
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    // Upload must be rejected; backing memory stays red from the clear.
    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [255, 0, 0, 255]);
    }
}

#[test]
fn shared_surface_release_invalidates_token_but_existing_alias_still_works() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0x9999_AAAAu64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            // Drop the original handle so the alias is the only live reference.
            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });

            // Remove the token mapping: future imports should fail but existing aliases remain valid.
            emit_packet(out, AerogpuCmdOpcode::ReleaseSharedSurface as u32, |out| {
                push_u64(out, share_token);
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 3); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 2); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}

#[test]
fn shared_surface_release_unknown_token_is_noop() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0xDEAD_BEEFu64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // Token has not been exported yet; release must be a no-op (must not retire the token).
            emit_packet(out, AerogpuCmdOpcode::ReleaseSharedSurface as u32, |out| {
                push_u64(out, share_token);
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 2); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 0);
    assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}

#[test]
fn shared_surface_release_retires_token_for_future_exports() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let cmd_gpa = 0x2000u64;
    let share_token = 0xABCD_EF01u64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::ReleaseSharedSurface as u32, |out| {
                push_u64(out, share_token);
                push_u64(out, 0); // reserved0
            });

            // Once released, the token is retired and must not be re-exported, even for the same
            // underlying resource.
            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa: 0,
        alloc_table_size_bytes: 0,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn shared_surface_token_is_retired_when_last_handle_destroyed() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let cmd_gpa = 0x2000u64;
    let share_token = 0xCAFE_BABEu64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // Tokens are retired when the last handle to the underlying resource is destroyed, so
            // the same numeric token value cannot be re-exported for new resources.
            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa: 0,
        alloc_table_size_bytes: 0,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn shared_surface_export_with_zero_token_sets_error_irq() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let cmd_gpa = 0x2000u64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // share_token (reserved)
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa: 0,
        alloc_table_size_bytes: 0,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn alloc_table_allows_zero_gpa_entries() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 1); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 0);
    assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

    let mut out = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, [255, 0, 0, 255]);
    }
}

#[test]
fn shared_surface_import_with_zero_token_sets_error_irq() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let cmd_gpa = 0x2000u64;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 2); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // share_token (reserved)
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa: 0,
        alloc_table_size_bytes: 0,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn shared_surface_export_token_collision_is_an_error_but_keeps_existing_mapping() {
    let mut mem = VecMemory::new(0x20_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuSoftwareExecutor::new();

    let alloc_table_gpa = 0x1000u64;
    let backing_gpa = 0x8000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        backing_gpa,
        0x1000,
    );

    let cmd_gpa = 0x2000u64;
    let (width, height) = (2u32, 2u32);
    let share_token = 0xAAAA_BBBB_CCCC_DDDDu64;
    let tex2_offset = 0x100u32;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (auto)
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, tex2_offset); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // Exporting the same token for a different texture should raise an error, but the
            // original mapping must remain intact so imports still resolve to texture 1.
            emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
                push_u32(out, 3); // out_resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, share_token);
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 3); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 0.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits());
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });
        },
        regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc = AeroGpuSubmitDesc {
        desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
        flags: 0,
        context_id: 0,
        engine_id: 0,
        cmd_gpa,
        cmd_size_bytes: stream.len() as u32,
        alloc_table_gpa,
        alloc_table_size_bytes,
        signal_fence: 0,
    };

    exec.execute_submission(&mut regs, &mut mem, &desc);

    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let mut tex1 = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa, &mut tex1);
    for px in tex1.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }

    let mut tex2 = vec![0u8; (width * height * 4) as usize];
    mem.read_physical(backing_gpa + u64::from(tex2_offset), &mut tex2);
    for px in tex2.chunks_exact(4) {
        assert_eq!(px, [0, 0, 0, 0]);
    }
}
