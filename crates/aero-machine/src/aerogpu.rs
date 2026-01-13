#![forbid(unsafe_code)]

use core::mem::offset_of;

use aero_devices::pci::PciBarMmioHandler;
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_protocol::aerogpu::aerogpu_ring as ring;
use memory::MemoryBus;

const RING_HEAD_OFFSET: u64 = offset_of!(ring::AerogpuRingHeader, head) as u64;
const RING_TAIL_OFFSET: u64 = offset_of!(ring::AerogpuRingHeader, tail) as u64;
const RING_HEADER_SIZE_BYTES: u64 = ring::AerogpuRingHeader::SIZE_BYTES as u64;

const FENCE_PAGE_MAGIC_OFFSET: u64 = offset_of!(ring::AerogpuFencePage, magic) as u64;
const FENCE_PAGE_ABI_VERSION_OFFSET: u64 = offset_of!(ring::AerogpuFencePage, abi_version) as u64;
const FENCE_PAGE_COMPLETED_FENCE_OFFSET: u64 =
    offset_of!(ring::AerogpuFencePage, completed_fence) as u64;

#[derive(Debug, Clone)]
pub struct AeroGpuMmioDevice {
    abi_version: u32,
    features: u64,

    ring_gpa: u64,
    ring_size_bytes: u32,
    ring_control: u32,

    fence_gpa: u64,
    completed_fence: u64,

    irq_status: u32,
    irq_enable: u32,

    scanout0_enable: bool,
    scanout0_width: u32,
    scanout0_height: u32,
    scanout0_format: u32,
    scanout0_pitch_bytes: u32,
    scanout0_fb_gpa: u64,
    scanout0_vblank_seq: u64,
    scanout0_vblank_time_ns: u64,
    scanout0_vblank_period_ns: u32,

    cursor_enable: bool,
    cursor_x: i32,
    cursor_y: i32,
    cursor_hot_x: u32,
    cursor_hot_y: u32,
    cursor_width: u32,
    cursor_height: u32,
    cursor_format: u32,
    cursor_fb_gpa: u64,
    cursor_pitch_bytes: u32,

    doorbell_pending: bool,
    ring_reset_pending: bool,
}

impl Default for AeroGpuMmioDevice {
    fn default() -> Self {
        Self {
            abi_version: pci::AEROGPU_ABI_VERSION_U32,
            // Keep the advertised feature surface conservative: vblank pacing and transfer command
            // execution are not implemented in `aero-machine` yet, but scanout/cursor register
            // storage exists so the Win7 KMD can discover/configure them without crashing.
            features: pci::AEROGPU_FEATURE_FENCE_PAGE
                | pci::AEROGPU_FEATURE_CURSOR
                | pci::AEROGPU_FEATURE_SCANOUT,

            ring_gpa: 0,
            ring_size_bytes: 0,
            ring_control: 0,

            fence_gpa: 0,
            completed_fence: 0,

            irq_status: 0,
            irq_enable: 0,

            scanout0_enable: false,
            scanout0_width: 0,
            scanout0_height: 0,
            scanout0_format: 0,
            scanout0_pitch_bytes: 0,
            scanout0_fb_gpa: 0,
            scanout0_vblank_seq: 0,
            scanout0_vblank_time_ns: 0,
            scanout0_vblank_period_ns: 0,

            cursor_enable: false,
            cursor_x: 0,
            cursor_y: 0,
            cursor_hot_x: 0,
            cursor_hot_y: 0,
            cursor_width: 0,
            cursor_height: 0,
            cursor_format: 0,
            cursor_fb_gpa: 0,
            cursor_pitch_bytes: 0,

            doorbell_pending: false,
            ring_reset_pending: false,
        }
    }
}

impl AeroGpuMmioDevice {
    pub fn reset(&mut self) {
        let features = self.features;
        let abi_version = self.abi_version;
        *self = Self {
            features,
            abi_version,
            ..Default::default()
        };
    }

    pub fn irq_level(&self) -> bool {
        (self.irq_status & self.irq_enable) != 0
    }

    pub fn process(&mut self, mem: &mut dyn MemoryBus, dma_enabled: bool) {
        // Ring control RESET is an MMIO write-side effect, but touching the ring header requires
        // DMA; perform the actual memory update from the machine's device tick path when bus
        // mastering is enabled.
        if self.ring_reset_pending {
            self.ring_reset_pending = false;
            self.doorbell_pending = false;

            self.completed_fence = 0;
            self.irq_status = 0;

            if dma_enabled && self.ring_gpa != 0 {
                let tail = mem.read_u32(self.ring_gpa + RING_TAIL_OFFSET);
                mem.write_u32(self.ring_gpa + RING_HEAD_OFFSET, tail);
            }

            if dma_enabled
                && self.fence_gpa != 0
                && (self.features & pci::AEROGPU_FEATURE_FENCE_PAGE) != 0
            {
                write_fence_page(mem, self.fence_gpa, self.abi_version, self.completed_fence);
            }
        }

        if !dma_enabled {
            // Ring DMA is gated by PCI COMMAND.BME.
            return;
        }

        if !self.doorbell_pending {
            return;
        }
        self.doorbell_pending = false;

        if (self.ring_control & pci::AEROGPU_RING_CONTROL_ENABLE) == 0 {
            return;
        }
        if self.ring_gpa == 0 || self.ring_size_bytes == 0 {
            return;
        }

        let mut hdr_buf = [0u8; ring::AerogpuRingHeader::SIZE_BYTES];
        mem.read_physical(self.ring_gpa, &mut hdr_buf);
        let Ok(ring_hdr) = ring::AerogpuRingHeader::decode_from_le_bytes(&hdr_buf) else {
            return;
        };
        if ring_hdr.validate_prefix().is_err() {
            return;
        }

        // The guest-declared ring size must not exceed the MMIO-programmed ring mapping size. The
        // mapping may be larger due to page rounding / extension space.
        if u64::from(ring_hdr.size_bytes) > u64::from(self.ring_size_bytes) {
            return;
        }

        let mut head = ring_hdr.head;
        let tail = ring_hdr.tail;
        let pending = tail.wrapping_sub(head);
        if pending == 0 {
            return;
        }

        if pending > ring_hdr.entry_count {
            // Driver and device are out of sync; drop all pending work to avoid looping forever.
            mem.write_u32(self.ring_gpa + RING_HEAD_OFFSET, tail);
            return;
        }

        let mut processed = 0u32;
        let max = ring_hdr.entry_count.min(pending);

        while head != tail && processed < max {
            // entry_count is validated as a power-of-two.
            let slot = head & (ring_hdr.entry_count - 1);
            let desc_gpa = self.ring_gpa
                + RING_HEADER_SIZE_BYTES
                + (u64::from(slot) * u64::from(ring_hdr.entry_stride_bytes));

            let mut desc_buf = [0u8; ring::AerogpuSubmitDesc::SIZE_BYTES];
            mem.read_physical(desc_gpa, &mut desc_buf);
            if let Ok(desc) = ring::AerogpuSubmitDesc::decode_from_le_bytes(&desc_buf) {
                // Treat the command stream as a no-op for now. The goal is transport + fence
                // completion so the Win7 KMD doesn't deadlock.
                if desc.signal_fence != 0 && desc.signal_fence > self.completed_fence {
                    self.completed_fence = desc.signal_fence;

                    let wants_irq = (desc.flags & ring::AEROGPU_SUBMIT_FLAG_NO_IRQ) == 0;
                    if wants_irq && (self.irq_enable & pci::AEROGPU_IRQ_FENCE) != 0 {
                        self.irq_status |= pci::AEROGPU_IRQ_FENCE;
                    }
                }
            }

            head = head.wrapping_add(1);
            processed += 1;
        }

        // Publish the new head after processing submissions.
        mem.write_u32(self.ring_gpa + RING_HEAD_OFFSET, head);

        if self.fence_gpa != 0 && (self.features & pci::AEROGPU_FEATURE_FENCE_PAGE) != 0 {
            write_fence_page(mem, self.fence_gpa, self.abi_version, self.completed_fence);
        }
    }

    fn mmio_read_dword(&self, offset: u64) -> u32 {
        match offset {
            x if x == pci::AEROGPU_MMIO_REG_MAGIC as u64 => pci::AEROGPU_MMIO_MAGIC,
            x if x == pci::AEROGPU_MMIO_REG_ABI_VERSION as u64 => self.abi_version,
            x if x == pci::AEROGPU_MMIO_REG_FEATURES_LO as u64 => self.features as u32,
            x if x == pci::AEROGPU_MMIO_REG_FEATURES_HI as u64 => (self.features >> 32) as u32,

            x if x == pci::AEROGPU_MMIO_REG_RING_GPA_LO as u64 => self.ring_gpa as u32,
            x if x == pci::AEROGPU_MMIO_REG_RING_GPA_HI as u64 => (self.ring_gpa >> 32) as u32,
            x if x == pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES as u64 => self.ring_size_bytes,
            x if x == pci::AEROGPU_MMIO_REG_RING_CONTROL as u64 => self.ring_control,

            x if x == pci::AEROGPU_MMIO_REG_FENCE_GPA_LO as u64 => self.fence_gpa as u32,
            x if x == pci::AEROGPU_MMIO_REG_FENCE_GPA_HI as u64 => (self.fence_gpa >> 32) as u32,

            x if x == pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO as u64 => self.completed_fence as u32,
            x if x == pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI as u64 => {
                (self.completed_fence >> 32) as u32
            }

            x if x == pci::AEROGPU_MMIO_REG_IRQ_STATUS as u64 => self.irq_status,
            x if x == pci::AEROGPU_MMIO_REG_IRQ_ENABLE as u64 => self.irq_enable,

            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64 => self.scanout0_enable as u32,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64 => self.scanout0_width,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64 => self.scanout0_height,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64 => self.scanout0_format,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64 => self.scanout0_pitch_bytes,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64 => self.scanout0_fb_gpa as u32,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64 => {
                (self.scanout0_fb_gpa >> 32) as u32
            }

            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO as u64 => {
                self.scanout0_vblank_seq as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI as u64 => {
                (self.scanout0_vblank_seq >> 32) as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO as u64 => {
                self.scanout0_vblank_time_ns as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI as u64 => {
                (self.scanout0_vblank_time_ns >> 32) as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS as u64 => {
                self.scanout0_vblank_period_ns
            }

            x if x == pci::AEROGPU_MMIO_REG_CURSOR_ENABLE as u64 => self.cursor_enable as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_X as u64 => self.cursor_x as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_Y as u64 => self.cursor_y as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HOT_X as u64 => self.cursor_hot_x,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y as u64 => self.cursor_hot_y,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_WIDTH as u64 => self.cursor_width,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT as u64 => self.cursor_height,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FORMAT as u64 => self.cursor_format,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO as u64 => self.cursor_fb_gpa as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI as u64 => (self.cursor_fb_gpa >> 32) as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES as u64 => self.cursor_pitch_bytes,

            _ => 0,
        }
    }

    fn mmio_write_dword(&mut self, offset: u64, value: u32) {
        match offset {
            x if x == pci::AEROGPU_MMIO_REG_RING_GPA_LO as u64 => {
                self.ring_gpa = (self.ring_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            x if x == pci::AEROGPU_MMIO_REG_RING_GPA_HI as u64 => {
                self.ring_gpa = (self.ring_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            x if x == pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES as u64 => {
                self.ring_size_bytes = value;
            }
            x if x == pci::AEROGPU_MMIO_REG_RING_CONTROL as u64 => {
                if (value & pci::AEROGPU_RING_CONTROL_RESET) != 0 {
                    self.ring_reset_pending = true;
                }
                self.ring_control = value & pci::AEROGPU_RING_CONTROL_ENABLE;
            }

            x if x == pci::AEROGPU_MMIO_REG_FENCE_GPA_LO as u64 => {
                self.fence_gpa = (self.fence_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            x if x == pci::AEROGPU_MMIO_REG_FENCE_GPA_HI as u64 => {
                self.fence_gpa =
                    (self.fence_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }

            x if x == pci::AEROGPU_MMIO_REG_DOORBELL as u64 => {
                let _ = value;
                self.doorbell_pending = true;
            }

            x if x == pci::AEROGPU_MMIO_REG_IRQ_ENABLE as u64 => {
                self.irq_enable = value;
                // Clear any IRQ status bits that are now masked so re-enabling doesn't immediately
                // deliver a stale interrupt.
                if (value & pci::AEROGPU_IRQ_FENCE) == 0 {
                    self.irq_status &= !pci::AEROGPU_IRQ_FENCE;
                }
                if (value & pci::AEROGPU_IRQ_SCANOUT_VBLANK) == 0 {
                    self.irq_status &= !pci::AEROGPU_IRQ_SCANOUT_VBLANK;
                }
            }
            x if x == pci::AEROGPU_MMIO_REG_IRQ_ACK as u64 => {
                self.irq_status &= !value;
            }

            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64 => {
                self.scanout0_enable = value != 0;
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64 => {
                self.scanout0_width = value;
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64 => {
                self.scanout0_height = value;
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64 => {
                self.scanout0_format = value;
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64 => {
                self.scanout0_pitch_bytes = value;
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64 => {
                self.scanout0_fb_gpa =
                    (self.scanout0_fb_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64 => {
                self.scanout0_fb_gpa =
                    (self.scanout0_fb_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }

            x if x == pci::AEROGPU_MMIO_REG_CURSOR_ENABLE as u64 => self.cursor_enable = value != 0,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_X as u64 => self.cursor_x = value as i32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_Y as u64 => self.cursor_y = value as i32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HOT_X as u64 => self.cursor_hot_x = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y as u64 => self.cursor_hot_y = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_WIDTH as u64 => self.cursor_width = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT as u64 => self.cursor_height = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FORMAT as u64 => self.cursor_format = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO as u64 => {
                self.cursor_fb_gpa =
                    (self.cursor_fb_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI as u64 => {
                self.cursor_fb_gpa =
                    (self.cursor_fb_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES as u64 => {
                self.cursor_pitch_bytes = value;
            }

            // Ignore writes to read-only / unknown registers.
            _ => {}
        }
    }
}

impl PciBarMmioHandler for AeroGpuMmioDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.clamp(1, 8);

        let mut out = 0u64;
        for i in 0..size {
            let off = offset.wrapping_add(i as u64);
            let aligned = off & !3;
            let shift = ((off & 3) * 8) as u32;
            let dword = self.mmio_read_dword(aligned);
            let byte = ((dword >> shift) & 0xFF) as u64;
            out |= byte << (i * 8);
        }
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.clamp(1, 8);
        let bytes = value.to_le_bytes();

        for i in 0..size {
            let off = offset.wrapping_add(i as u64);
            let aligned = off & !3;
            let shift = ((off & 3) * 8) as u32;
            let mut cur = self.mmio_read_dword(aligned);
            let mask = 0xFFu32 << shift;
            cur = (cur & !mask) | (u32::from(bytes[i]) << shift);
            self.mmio_write_dword(aligned, cur);
        }
    }
}

fn write_fence_page(mem: &mut dyn MemoryBus, gpa: u64, abi_version: u32, completed_fence: u64) {
    mem.write_u32(gpa + FENCE_PAGE_MAGIC_OFFSET, ring::AEROGPU_FENCE_PAGE_MAGIC);
    mem.write_u32(gpa + FENCE_PAGE_ABI_VERSION_OFFSET, abi_version);
    mem.write_u64(gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET, completed_fence);
}
