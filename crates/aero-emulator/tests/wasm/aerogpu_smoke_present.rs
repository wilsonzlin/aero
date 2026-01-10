use aero_emulator::devices::aerogpu::{
    AerogpuDevice, CmdClearRgba, CmdCreateSurface, CmdDrawTriangleTest, CmdHeader, CmdPresent,
    CmdUpdateSurface, DeviceConfig, EventCmdStatus, GuestMemory, GuestMemoryError, IrqBits, Opcode,
    StatusCode, SurfaceFormat, REG_CMD_RING_DOORBELL, REG_IRQ_ACK, REG_IRQ_STATUS,
};
use std::sync::{Arc, Mutex};

struct TestGuestMemory {
    mem: Mutex<Vec<u8>>,
}

impl TestGuestMemory {
    fn new(size: usize) -> Self {
        Self {
            mem: Mutex::new(vec![0u8; size]),
        }
    }

    fn write(&self, guest_phys_addr: u64, data: &[u8]) {
        let mut mem = self.mem.lock().unwrap();
        let start = guest_phys_addr as usize;
        let end = start + data.len();
        mem[start..end].copy_from_slice(data);
    }
}

impl GuestMemory for TestGuestMemory {
    fn read(&self, guest_phys_addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        let mem = self.mem.lock().unwrap();
        let start = guest_phys_addr as usize;
        let end = start + dst.len();
        let src = mem.get(start..end).ok_or(GuestMemoryError::OutOfBounds)?;
        dst.copy_from_slice(src);
        Ok(())
    }
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001B3;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

fn wait_for_cmd_status(
    evt_cons: &aero_emulator::devices::aerogpu::RingConsumer,
    opcode: u32,
) -> EventCmdStatus {
    loop {
        let bytes = evt_cons.pop_blocking().expect("event ring");
        if let Some(evt) = EventCmdStatus::decode(&bytes) {
            if evt.opcode == opcode {
                return evt;
            }
        }
    }
}

#[test]
fn smoke_present_and_error_paths() {
    let guest_mem = Arc::new(TestGuestMemory::new(1024 * 1024));
    let mut dev = AerogpuDevice::new(
        guest_mem.clone(),
        DeviceConfig {
            cmd_ring_size_bytes: 16 * 1024,
            evt_ring_size_bytes: 16 * 1024,
        },
    );

    let cmd_prod = dev.cmd_ring_producer();
    let evt_cons = dev.evt_ring_consumer();

    // CREATE_SURFACE(16x16, RGBA8888)
    cmd_prod
        .try_push(
            &CmdCreateSurface {
                width: 16,
                height: 16,
                format: SurfaceFormat::Rgba8888 as u32,
            }
            .encode(),
        )
        .unwrap();
    dev.mmio_write_u32(REG_CMD_RING_DOORBELL, 1);

    let evt = wait_for_cmd_status(&evt_cons, Opcode::CREATE_SURFACE);
    assert_eq!(evt.status, StatusCode::Ok);
    let surface_id = evt.data[0];
    assert_ne!(surface_id, 0);

    // CLEAR_RGBA(surface_id, blue) + PRESENT(surface_id)
    cmd_prod
        .try_push(
            &CmdClearRgba {
                surface_id,
                rgba: rgba(0, 0, 255, 255),
            }
            .encode(),
        )
        .unwrap();
    cmd_prod
        .try_push(&CmdPresent { surface_id }.encode())
        .unwrap();
    dev.mmio_write_u32(REG_CMD_RING_DOORBELL, 1);

    let evt = wait_for_cmd_status(&evt_cons, Opcode::CLEAR_RGBA);
    assert_eq!(evt.status, StatusCode::Ok);
    let evt = wait_for_cmd_status(&evt_cons, Opcode::PRESENT);
    assert_eq!(evt.status, StatusCode::Ok);
    assert_ne!(dev.mmio_read_u32(REG_IRQ_STATUS) & IrqBits::PRESENT_DONE, 0);
    dev.mmio_write_u32(REG_IRQ_ACK, IrqBits::PRESENT_DONE);

    let fb = dev.framebuffer();
    assert_eq!(fb.width, 16);
    assert_eq!(fb.height, 16);
    assert_eq!(fb.format, SurfaceFormat::Rgba8888);
    let expected_clear = vec![0u8, 0u8, 255u8, 255u8]
        .into_iter()
        .cycle()
        .take((16 * 16 * 4) as usize)
        .collect::<Vec<_>>();
    assert_eq!(fb.pixels, expected_clear);

    // UPDATE_SURFACE(surface_id, addr=0, stride=64) + PRESENT(surface_id)
    let mut pattern = vec![0u8; (16 * 16 * 4) as usize];
    for y in 0..16 {
        for x in 0..16 {
            let idx = (y * 16 + x) * 4;
            pattern[idx + 0] = x as u8; // R
            pattern[idx + 1] = y as u8; // G
            pattern[idx + 2] = 0x80; // B
            pattern[idx + 3] = 0xFF; // A
        }
    }
    guest_mem.write(0, &pattern);

    cmd_prod
        .try_push(
            &CmdUpdateSurface {
                surface_id,
                guest_phys_addr: 0,
                stride: 16 * 4,
            }
            .encode(),
        )
        .unwrap();
    cmd_prod
        .try_push(&CmdPresent { surface_id }.encode())
        .unwrap();
    dev.mmio_write_u32(REG_CMD_RING_DOORBELL, 1);

    let evt = wait_for_cmd_status(&evt_cons, Opcode::UPDATE_SURFACE);
    assert_eq!(evt.status, StatusCode::Ok);
    let evt = wait_for_cmd_status(&evt_cons, Opcode::PRESENT);
    assert_eq!(evt.status, StatusCode::Ok);

    let fb = dev.framebuffer();
    assert_eq!(fb.pixels, pattern);

    // DRAW_TRIANGLE_TEST(surface_id) + PRESENT(surface_id)
    cmd_prod
        .try_push(&CmdDrawTriangleTest { surface_id }.encode())
        .unwrap();
    cmd_prod
        .try_push(&CmdPresent { surface_id }.encode())
        .unwrap();
    dev.mmio_write_u32(REG_CMD_RING_DOORBELL, 1);

    let evt = wait_for_cmd_status(&evt_cons, Opcode::DRAW_TRIANGLE_TEST);
    assert_eq!(evt.status, StatusCode::Ok);
    let evt = wait_for_cmd_status(&evt_cons, Opcode::PRESENT);
    assert_eq!(evt.status, StatusCode::Ok);

    let fb = dev.framebuffer();
    // Hash over the triangle output to catch regressions in the diagnostic rasterizer.
    const EXPECTED_TRIANGLE_HASH: u64 = 12_765_624_695_350_605_273;
    let got = fnv1a64(&fb.pixels);
    assert_eq!(got, EXPECTED_TRIANGLE_HASH);

    // Unknown opcode must not crash and must return INVALID_OPCODE.
    let unknown = {
        let size_bytes = 8u32;
        let mut v = Vec::new();
        v.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        v.extend_from_slice(&size_bytes.to_le_bytes());
        // `CmdHeader::decode` expects size_bytes == bytes.len().
        assert_eq!(CmdHeader::decode(&v).unwrap().size_bytes, size_bytes);
        v
    };
    cmd_prod.try_push(&unknown).unwrap();
    dev.mmio_write_u32(REG_CMD_RING_DOORBELL, 1);

    let evt = wait_for_cmd_status(&evt_cons, 0xDEAD_BEEF);
    assert_eq!(evt.status, StatusCode::InvalidOpcode);
}
