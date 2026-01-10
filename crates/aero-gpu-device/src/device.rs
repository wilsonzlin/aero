//! Emulator-side GPU device model.
//!
//! The real Aero emulator will expose this as a PCI device with a single MMIO
//! BAR. The guest driver allocates shared memory in guest RAM for rings and
//! programs their physical addresses via MMIO registers.

use core::fmt;

use crate::abi;
use crate::backend::{BackendError, GpuBackend, PresentedFrame, Viewport};
use crate::guest_memory::{GuestMemory, GuestMemoryError};
use crate::ring::{ByteRing, RingError, RingLocation};

#[derive(Debug)]
pub enum DeviceError {
    GuestMemory(GuestMemoryError),
    Ring(RingError),
    InvalidCommand(&'static str),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestMemory(e) => write!(f, "{e}"),
            Self::Ring(e) => write!(f, "{e}"),
            Self::InvalidCommand(msg) => write!(f, "invalid command: {msg}"),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<GuestMemoryError> for DeviceError {
    fn from(value: GuestMemoryError) -> Self {
        Self::GuestMemory(value)
    }
}

impl From<RingError> for DeviceError {
    fn from(value: RingError) -> Self {
        Self::Ring(value)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MmioState {
    cmd_ring_base: u64,
    cmd_ring_size: u32,
    cpl_ring_base: u64,
    cpl_ring_size: u32,
    desc_base: u64,
    desc_size: u32,

    int_status: u32,
    int_mask: u32,

    last_completed_seq: u64,
    last_fault_seq: u64,
}

/// Interrupt callback invoked when the device transitions from "no interrupt
/// pending" to "interrupt pending".
pub trait InterruptSink {
    fn raise_irq(&mut self);
    fn lower_irq(&mut self);
}

/// GPU command processor: parses the guest command stream and dispatches into a
/// backend (DirectX→WebGPU translator in production, software backend in tests).
#[derive(Debug)]
pub struct GpuCommandProcessor<B: GpuBackend> {
    backend: B,
    max_commands_per_doorbell: usize,
}

impl<B: GpuBackend> GpuCommandProcessor<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            max_commands_per_doorbell: 1024,
        }
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    fn process(
        &mut self,
        mem: &mut dyn GuestMemory,
        mmio: &mut MmioState,
        irq: Option<&mut dyn InterruptSink>,
    ) -> Result<usize, DeviceError> {
        if mmio.cmd_ring_base == 0 || mmio.cpl_ring_base == 0 {
            return Ok(0);
        }

        let cmd_loc = RingLocation {
            base_paddr: mmio.cmd_ring_base,
        };
        let cpl_loc = RingLocation {
            base_paddr: mmio.cpl_ring_base,
        };

        let cmd_ring = ByteRing::open(mem, cmd_loc)?;
        if mmio.cmd_ring_size != 0 && mmio.cmd_ring_size != cmd_ring.ring_size_bytes() {
            return Err(DeviceError::InvalidCommand("cmd ring size mismatch"));
        }
        let cpl_ring = ByteRing::open(mem, cpl_loc)?;
        if mmio.cpl_ring_size != 0 && mmio.cpl_ring_size != cpl_ring.ring_size_bytes() {
            return Err(DeviceError::InvalidCommand("completion ring size mismatch"));
        }

        let mut cmd_ring = cmd_ring;
        let mut cpl_ring = cpl_ring;

        let mut processed = 0usize;
        let mut wrote_completion = false;

        while processed < self.max_commands_per_doorbell {
            let Some(record) = cmd_ring.pop(mem)? else {
                break;
            };
            processed += 1;

            let (opcode, seq, status) = match parse_and_dispatch(&mut self.backend, mem, &record) {
                Ok((opcode, seq)) => (opcode, seq, abi::status::OK),
                Err(CommandExecError {
                    opcode,
                    seq,
                    status,
                }) => {
                    mmio.last_fault_seq = seq;
                    mmio.int_status |= abi::mmio::INT_STATUS_FAULT;
                    (opcode, seq, status)
                }
            };

            mmio.last_completed_seq = seq;
            let cpl = encode_completion(opcode, seq, status);
            cpl_ring.push(mem, &cpl)?;
            wrote_completion = true;
        }

        if wrote_completion {
            let had_irq = (mmio.int_status & abi::mmio::INT_STATUS_CPL_AVAIL) != 0;
            mmio.int_status |= abi::mmio::INT_STATUS_CPL_AVAIL;
            if !had_irq && (mmio.int_mask & abi::mmio::INT_STATUS_CPL_AVAIL) != 0 {
                if let Some(irq) = irq {
                    irq.raise_irq();
                }
            }
        }

        Ok(processed)
    }
}

/// Emulator-side PCI/MMIO GPU device.
#[derive(Debug)]
pub struct GpuDevice<B: GpuBackend> {
    mmio: MmioState,
    processor: GpuCommandProcessor<B>,
}

impl<B: GpuBackend> GpuDevice<B> {
    pub fn new(backend: B) -> Self {
        Self {
            mmio: MmioState::default(),
            processor: GpuCommandProcessor::new(backend),
        }
    }

    pub fn take_presented_frame(&mut self) -> Option<PresentedFrame> {
        self.processor.backend_mut().take_presented_frame()
    }

    pub fn mmio_read32(&self, offset: u64) -> u32 {
        match offset {
            abi::mmio::REG_ABI_VERSION => ((abi::ABI_MAJOR as u32) << 16) | (abi::ABI_MINOR as u32),

            abi::mmio::REG_CMD_RING_BASE_LO => self.mmio.cmd_ring_base as u32,
            abi::mmio::REG_CMD_RING_BASE_HI => (self.mmio.cmd_ring_base >> 32) as u32,
            abi::mmio::REG_CMD_RING_SIZE => self.mmio.cmd_ring_size,

            abi::mmio::REG_CPL_RING_BASE_LO => self.mmio.cpl_ring_base as u32,
            abi::mmio::REG_CPL_RING_BASE_HI => (self.mmio.cpl_ring_base >> 32) as u32,
            abi::mmio::REG_CPL_RING_SIZE => self.mmio.cpl_ring_size,

            abi::mmio::REG_DESC_BASE_LO => self.mmio.desc_base as u32,
            abi::mmio::REG_DESC_BASE_HI => (self.mmio.desc_base >> 32) as u32,
            abi::mmio::REG_DESC_SIZE => self.mmio.desc_size,

            abi::mmio::REG_INT_STATUS => self.mmio.int_status,
            abi::mmio::REG_INT_MASK => self.mmio.int_mask,

            abi::mmio::REG_LAST_COMPLETED_SEQ_LO => self.mmio.last_completed_seq as u32,
            abi::mmio::REG_LAST_COMPLETED_SEQ_HI => (self.mmio.last_completed_seq >> 32) as u32,

            abi::mmio::REG_LAST_FAULT_SEQ_LO => self.mmio.last_fault_seq as u32,
            abi::mmio::REG_LAST_FAULT_SEQ_HI => (self.mmio.last_fault_seq >> 32) as u32,

            _ => 0,
        }
    }

    pub fn mmio_write32(
        &mut self,
        offset: u64,
        value: u32,
        mem: &mut dyn GuestMemory,
        mut irq: Option<&mut dyn InterruptSink>,
    ) -> Result<(), DeviceError> {
        if offset == abi::mmio::REG_DOORBELL {
            // Throttle/batch: one doorbell can process multiple commands.
            self.processor.process(mem, &mut self.mmio, irq)?;
            return Ok(());
        }

        match offset {
            abi::mmio::REG_CMD_RING_BASE_LO => {
                self.mmio.cmd_ring_base = (self.mmio.cmd_ring_base & !0xFFFF_FFFF) | value as u64;
            }
            abi::mmio::REG_CMD_RING_BASE_HI => {
                self.mmio.cmd_ring_base =
                    (self.mmio.cmd_ring_base & 0xFFFF_FFFF) | ((value as u64) << 32);
            }
            abi::mmio::REG_CMD_RING_SIZE => self.mmio.cmd_ring_size = value,

            abi::mmio::REG_CPL_RING_BASE_LO => {
                self.mmio.cpl_ring_base = (self.mmio.cpl_ring_base & !0xFFFF_FFFF) | value as u64;
            }
            abi::mmio::REG_CPL_RING_BASE_HI => {
                self.mmio.cpl_ring_base =
                    (self.mmio.cpl_ring_base & 0xFFFF_FFFF) | ((value as u64) << 32);
            }
            abi::mmio::REG_CPL_RING_SIZE => self.mmio.cpl_ring_size = value,

            abi::mmio::REG_DESC_BASE_LO => {
                self.mmio.desc_base = (self.mmio.desc_base & !0xFFFF_FFFF) | value as u64;
            }
            abi::mmio::REG_DESC_BASE_HI => {
                self.mmio.desc_base = (self.mmio.desc_base & 0xFFFF_FFFF) | ((value as u64) << 32);
            }
            abi::mmio::REG_DESC_SIZE => self.mmio.desc_size = value,

            abi::mmio::REG_INT_MASK => {
                self.mmio.int_mask = value;
                // If interrupts were unmasked and something is pending, raise now.
                if (self.mmio.int_status & self.mmio.int_mask) != 0 {
                    if let Some(irq) = irq.as_deref_mut() {
                        irq.raise_irq();
                    }
                }
            }
            abi::mmio::REG_INT_ACK => {
                self.mmio.int_status &= !value;
                if (self.mmio.int_status & self.mmio.int_mask) == 0 {
                    if let Some(irq) = irq.as_deref_mut() {
                        irq.lower_irq();
                    }
                }
            }

            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct CommandExecError {
    opcode: u16,
    seq: u64,
    status: u32,
}

fn parse_and_dispatch(
    backend: &mut dyn GpuBackend,
    mem: &mut dyn GuestMemory,
    record: &[u8],
) -> Result<(u16, u64), CommandExecError> {
    if record.len() < abi::GpuCmdHeader::SIZE {
        return Err(CommandExecError {
            opcode: 0,
            seq: 0,
            status: abi::status::INVALID_COMMAND,
        });
    }

    let magic = u32::from_le_bytes(record[0..4].try_into().unwrap());
    if magic != abi::GPU_CMD_MAGIC {
        return Err(CommandExecError {
            opcode: 0,
            seq: 0,
            status: abi::status::INVALID_COMMAND,
        });
    }
    let size_bytes = u32::from_le_bytes(record[4..8].try_into().unwrap()) as usize;
    if size_bytes != record.len() {
        return Err(CommandExecError {
            opcode: 0,
            seq: 0,
            status: abi::status::INVALID_COMMAND,
        });
    }

    let opcode = u16::from_le_bytes(record[8..10].try_into().unwrap());
    let _flags = u16::from_le_bytes(record[10..12].try_into().unwrap());
    let abi_major = u16::from_le_bytes(record[12..14].try_into().unwrap());
    let _abi_minor = u16::from_le_bytes(record[14..16].try_into().unwrap());
    let seq = u64::from_le_bytes(record[16..24].try_into().unwrap());

    if abi_major != abi::ABI_MAJOR {
        return Err(CommandExecError {
            opcode,
            seq,
            status: abi::status::UNSUPPORTED,
        });
    }

    let payload = &record[abi::GpuCmdHeader::SIZE..];

    let result = match opcode {
        abi::opcode::NOP => Ok(()),

        abi::opcode::CREATE_BUFFER => {
            if payload.len() < 24 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let buffer_id = read_u32(payload, 0);
            let size_bytes = read_u64(payload, 8);
            let usage = read_u32(payload, 16);
            backend.create_buffer(buffer_id, size_bytes, usage)
        }
        abi::opcode::DESTROY_BUFFER => {
            if payload.len() < 8 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let buffer_id = read_u32(payload, 0);
            backend.destroy_buffer(buffer_id)
        }
        abi::opcode::WRITE_BUFFER => {
            if payload.len() < 32 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let buffer_id = read_u32(payload, 0);
            let dst_offset = read_u64(payload, 8);
            let src_paddr = read_u64(payload, 16);
            let size_bytes = read_u32(payload, 24) as usize;
            let mut tmp = vec![0u8; size_bytes];
            mem.read(src_paddr, &mut tmp)
                .map_err(|_| CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::OUT_OF_BOUNDS,
                })?;
            backend.write_buffer(buffer_id, dst_offset, &tmp)
        }
        abi::opcode::READ_BUFFER => {
            if payload.len() < 32 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let buffer_id = read_u32(payload, 0);
            let src_offset = read_u64(payload, 8);
            let dst_paddr = read_u64(payload, 16);
            let size_bytes = read_u32(payload, 24) as usize;
            let data = match backend.read_buffer(buffer_id, src_offset, size_bytes) {
                Ok(data) => data,
                Err(e) => {
                    return Err(CommandExecError {
                        opcode,
                        seq,
                        status: backend_error_to_status(&e),
                    })
                }
            };
            mem.write(dst_paddr, &data).map_err(|_| CommandExecError {
                opcode,
                seq,
                status: abi::status::OUT_OF_BOUNDS,
            })?;
            Ok(())
        }

        abi::opcode::CREATE_TEXTURE2D => {
            if payload.len() < 24 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let texture_id = read_u32(payload, 0);
            let width = read_u32(payload, 4);
            let height = read_u32(payload, 8);
            let format = read_u32(payload, 12);
            let usage = read_u32(payload, 16);
            backend.create_texture2d(texture_id, width, height, format, usage)
        }
        abi::opcode::DESTROY_TEXTURE => {
            if payload.len() < 8 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let texture_id = read_u32(payload, 0);
            backend.destroy_texture(texture_id)
        }
        abi::opcode::WRITE_TEXTURE2D => {
            if payload.len() < 32 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let texture_id = read_u32(payload, 0);
            let mip_level = read_u32(payload, 4);
            let src_paddr = read_u64(payload, 8);
            let bytes_per_row = read_u32(payload, 16);
            let width = read_u32(payload, 20);
            let height = read_u32(payload, 24);
            let total = (bytes_per_row as usize)
                .checked_mul(height as usize)
                .ok_or(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::OUT_OF_BOUNDS,
                })?;
            let mut tmp = vec![0u8; total];
            mem.read(src_paddr, &mut tmp)
                .map_err(|_| CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::OUT_OF_BOUNDS,
                })?;
            backend.write_texture2d(texture_id, mip_level, width, height, bytes_per_row, &tmp)
        }
        abi::opcode::READ_TEXTURE2D => {
            if payload.len() < 32 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let texture_id = read_u32(payload, 0);
            let mip_level = read_u32(payload, 4);
            let dst_paddr = read_u64(payload, 8);
            let bytes_per_row = read_u32(payload, 16);
            let width = read_u32(payload, 20);
            let height = read_u32(payload, 24);
            let data =
                match backend.read_texture2d(texture_id, mip_level, width, height, bytes_per_row) {
                    Ok(data) => data,
                    Err(e) => {
                        return Err(CommandExecError {
                            opcode,
                            seq,
                            status: backend_error_to_status(&e),
                        })
                    }
                };
            mem.write(dst_paddr, &data).map_err(|_| CommandExecError {
                opcode,
                seq,
                status: abi::status::OUT_OF_BOUNDS,
            })?;
            Ok(())
        }

        abi::opcode::SET_RENDER_TARGET => {
            if payload.len() < 8 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let texture_id = read_u32(payload, 0);
            backend.set_render_target(texture_id)
        }
        abi::opcode::CLEAR => {
            if payload.len() < 24 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let r = read_f32(payload, 0);
            let g = read_f32(payload, 4);
            let b = read_f32(payload, 8);
            let a = read_f32(payload, 12);
            backend.clear([r, g, b, a])
        }
        abi::opcode::SET_VIEWPORT => {
            if payload.len() < 24 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let x = read_f32(payload, 0);
            let y = read_f32(payload, 4);
            let width = read_f32(payload, 8);
            let height = read_f32(payload, 12);
            backend.set_viewport(Viewport {
                x,
                y,
                width,
                height,
            })
        }

        abi::opcode::SET_PIPELINE => {
            if payload.len() < 8 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let pipeline_id = read_u32(payload, 0);
            backend.set_pipeline(pipeline_id)
        }
        abi::opcode::SET_VERTEX_BUFFER => {
            if payload.len() < 24 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let buffer_id = read_u32(payload, 0);
            let stride = read_u32(payload, 4);
            let offset = read_u64(payload, 8);
            backend.set_vertex_buffer(buffer_id, offset, stride)
        }
        abi::opcode::DRAW => {
            if payload.len() < 16 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let vertex_count = read_u32(payload, 0);
            let first_vertex = read_u32(payload, 4);
            backend.draw(vertex_count, first_vertex)
        }

        abi::opcode::PRESENT => {
            if payload.len() < 8 {
                return Err(CommandExecError {
                    opcode,
                    seq,
                    status: abi::status::INVALID_COMMAND,
                });
            }
            let texture_id = read_u32(payload, 0);
            backend.present(texture_id)
        }

        abi::opcode::FENCE_SIGNAL => {
            // Fence support is emulated via completion records for now.
            // A future ABI extension may include a shared fence table.
            let _ = payload;
            Ok(())
        }

        _ => Err(BackendError::Unsupported),
    };

    match result {
        Ok(()) => Ok((opcode, seq)),
        Err(e) => Err(CommandExecError {
            opcode,
            seq,
            status: backend_error_to_status(&e),
        }),
    }
}

fn backend_error_to_status(err: &BackendError) -> u32 {
    match err {
        BackendError::InvalidResource => abi::status::INVALID_RESOURCE,
        BackendError::InvalidState(_) => abi::status::INVALID_COMMAND,
        BackendError::OutOfBounds => abi::status::OUT_OF_BOUNDS,
        BackendError::Unsupported => abi::status::UNSUPPORTED,
        BackendError::Internal(_) => abi::status::INTERNAL_ERROR,
    }
}

fn encode_completion(opcode: u16, seq: u64, status: u32) -> Vec<u8> {
    let mut out = vec![0u8; abi::GpuCompletion::SIZE];
    out[0..4].copy_from_slice(&abi::GPU_CPL_MAGIC.to_le_bytes());
    out[4..8].copy_from_slice(&(abi::GpuCompletion::SIZE as u32).to_le_bytes());
    out[8..16].copy_from_slice(&seq.to_le_bytes());
    out[16..18].copy_from_slice(&opcode.to_le_bytes());
    out[18..20].copy_from_slice(&0u16.to_le_bytes());
    out[20..24].copy_from_slice(&status.to_le_bytes());
    out
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn read_f32(buf: &[u8], off: usize) -> f32 {
    f32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}
