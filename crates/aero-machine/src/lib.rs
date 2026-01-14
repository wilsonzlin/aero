//! Canonical full-system "machine" integration layer for Aero.
//!
//! This crate composes the canonical CPU core (`aero_cpu_core`), firmware (`firmware::bios`),
//! physical memory bus (`memory`), and device models (`aero-devices` / `aero-platform`) into a
//! single VM-like interface that is usable from both:
//! - native Rust integration tests, and
//! - `wasm32` builds via `crates/aero-wasm`.
//!
//! The intention is to make "which machine runs in the browser?" an explicit, stable answer:
//! **`aero_machine::Machine`**.
//!
//! ## Current limitation: SMP bring-up only (no full multi-vCPU scheduling yet)
//!
//! `aero_machine::Machine` can be configured with `cpu_count > 1` and includes basic SMP plumbing:
//! per-vCPU LAPIC instances/MMIO routing, AP wait-for-SIPI state, INIT+SIPI delivery via the LAPIC
//! ICR, and a bounded cooperative AP execution loop inside [`Machine::run_slice`].
//!
//! This is sufficient for SMP contract/bring-up tests, but it is **not** a full SMP scheduler or
//! parallel vCPU execution environment yet. For robust guest boots today, `cpu_count = 1` is still
//! recommended.
//!
//! See `docs/21-smp.md` for the current SMP status and roadmap.
#![forbid(unsafe_code)]

mod aerogpu;
mod aerogpu_legacy_text;
mod guest_time;
mod shared_disk;
mod shared_iso_disk;
mod vcpu_init;
pub mod virtual_time;

pub use guest_time::{GuestTime, DEFAULT_GUEST_CPU_HZ};
pub use shared_disk::SharedDisk;
pub use shared_iso_disk::SharedIsoDisk;
use shared_iso_disk::SharedIsoDiskWeak;
pub use aero_devices_gpu::{
    AeroGpuBackendCompletion, AeroGpuBackendSubmission, AeroGpuCommandBackend,
    ImmediateAeroGpuBackend, NullAeroGpuBackend,
};

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Cursor, Read, Seek, Write};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_cpu_core_with_assists, BatchExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::interrupts::CpuExit;
use aero_cpu_core::state::{gpr, CpuMode, CpuState, RFLAGS_IF};
use aero_cpu_core::{AssistReason, CpuCore, Exception};
use aero_devices::a20_gate::{A20Gate as A20GateDevice, A20_GATE_PORT};
use aero_devices::acpi_pm::{
    register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, SharedAcpiPmIo,
};
use aero_devices::clock::{Clock, ManualClock};
use aero_devices::debugcon::{register_debugcon, SharedDebugConLog};
use aero_devices::dma::{register_dma8237, Dma8237};
use aero_devices::hpet;
use aero_devices::i8042::{I8042Ports, SharedI8042Controller};
use aero_devices::irq::{IrqLine, PlatformIrqLine};
use aero_devices::pci::{
    bios_post_with_extra_reservations, msix::PCI_CAP_ID_MSIX, register_pci_config_ports,
    MsiCapability, MsixCapability, PciBarDefinition, PciBarKind, PciBarMmioHandler,
    PciBarMmioRouter, PciBarRange, PciBdf, PciConfigPorts, PciConfigSyncedMmioBar, PciCoreSnapshot,
    PciDevice, PciEcamConfig, PciEcamMmio, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
    PciResourceAllocator, PciResourceAllocatorConfig, SharedPciConfigPorts,
};
use aero_devices::pic8259::register_pic8259_on_platform_interrupts;
use aero_devices::pit8254::{register_pit8254, Pit8254, SharedPit8254};
use aero_devices::reset_ctrl::{ResetCtrl, RESET_CTRL_PORT};
use aero_devices::rtc_cmos::{register_rtc_cmos, RtcCmos, SharedRtcCmos};
use aero_devices::serial::{register_serial16550, Serial16550, SharedSerial16550};
use aero_devices::usb::ehci::EhciPciDevice;
use aero_devices::usb::uhci::UhciPciDevice;
use aero_devices::usb::xhci::XhciPciDevice;
pub use aero_devices_input::Ps2MouseButton;
use aero_devices_nvme::{NvmeController, NvmePciDevice};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
use aero_devices_storage::pci_ahci::AhciPciDevice;
use aero_devices_storage::pci_ide::{Piix3IdePciDevice, PRIMARY_PORTS, SECONDARY_PORTS};
use aero_gpu_vga::{
    DisplayOutput as _, PortIO as _, VgaConfig, VgaDevice, VgaLegacyMmioHandler, VgaLfbMmioHandler,
    VgaPortIoDevice,
};
use aero_interrupts::apic::{IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, LAPIC_MMIO_BASE, LAPIC_MMIO_SIZE};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader as IoSnapshotReader, SnapshotResult as IoSnapshotResult,
    SnapshotVersion, SnapshotWriter,
};
use aero_io_snapshot::io::storage::dskc::DiskControllersSnapshot;
use aero_net_backend::{FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats, NetworkBackend};
use aero_net_e1000::E1000Device;
use aero_net_pump::{tick_e1000, tick_virtio_net, VirtioNetBackendAdapter};
use aero_pc_constants::{PCI_MMIO_BASE, PCI_MMIO_SIZE};
use aero_pc_platform::{PciIoBarHandler, PciIoBarRouter};
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::{A20GateHandle, ChipsetState};
use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, InterruptInput, IoApicMmio,
    PlatformInterrupts,
};
use aero_platform::io::{IoPortBus, PortIoDevice as _};
use aero_platform::memory::MemoryBus as PlatformMemoryBus;
use aero_platform::reset::{ResetKind, ResetLatch};
#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
use aero_shared::cursor_state::{CursorState, CursorStateUpdate, CURSOR_FORMAT_B8G8R8A8};
#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
use aero_shared::scanout_state::{
    ScanoutState, ScanoutStateUpdate, SCANOUT_FORMAT_B5G6R5, SCANOUT_FORMAT_B8G8R8X8,
    SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_LEGACY_VBE_LFB, SCANOUT_SOURCE_WDDM,
};
use aero_snapshot as snapshot;
use aero_storage::{MemBackend, RawDisk};
use aero_usb::hid::{
    GamepadReport, UsbHidConsumerControlHandle, UsbHidGamepadHandle, UsbHidKeyboardHandle,
    UsbHidMouseHandle,
};
use aero_usb::hub::UsbHubDevice;
use aero_usb::usb2_port::Usb2PortMux;
use aero_virtio::devices::blk::VirtioBlk;
use aero_virtio::devices::input::{VirtioInput, VirtioInputDeviceKind};
use aero_virtio::devices::net::VirtioNet;
use aero_virtio::memory::{
    GuestMemory as VirtioGuestMemory, GuestMemoryError as VirtioGuestMemoryError,
};
use aero_virtio::pci::{InterruptSink as VirtioInterruptSink, VirtioPciDevice};
use firmware::bda::BiosDataArea;
use firmware::bios::{A20Gate, Bios, BiosBus, BiosConfig, FirmwareMemory};
use memory::{
    DenseMemory, DirtyGuestMemory, DirtyTracker, GuestMemoryError, MapError, MemoryBus as _,
    MmioHandler, SparseMemory,
};

use crate::aerogpu::AeroGpuMmioDevice;

mod pci_firmware;
use pci_firmware::SharedPciConfigPortsBiosAdapter;

/// A minimal `MemoryBus` that reads from a snapshot of AeroGPU BAR1-backed VRAM.
///
/// This is used as a host-side scanout/cursor readback fast-path to avoid routing reads through the
/// PCI MMIO router when the guest points scanout surfaces directly at BAR1.
struct AeroGpuBar1VramReadbackBus<'a> {
    vram: &'a [u8],
    bar1_base: u64,
}

impl<'a> AeroGpuBar1VramReadbackBus<'a> {
    #[inline]
    fn new(vram: &'a [u8], bar1_base: u64) -> Self {
        Self { vram, bar1_base }
    }
}

impl memory::MemoryBus for AeroGpuBar1VramReadbackBus<'_> {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        let Some(off_u64) = paddr.checked_sub(self.bar1_base) else {
            buf.fill(0);
            return;
        };
        let Ok(off) = usize::try_from(off_u64) else {
            buf.fill(0);
            return;
        };
        if off >= self.vram.len() {
            buf.fill(0);
            return;
        }

        let available = self.vram.len() - off;
        let to_copy = available.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.vram[off..off + to_copy]);
        if to_copy < buf.len() {
            buf[to_copy..].fill(0);
        }
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        // Readback-only bus; ignore writes.
    }
}

const SNAPSHOT_DIRTY_PAGE_SIZE: u32 = 4096;
const DEFAULT_E1000_MAC_ADDR: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
const DEFAULT_VIRTIO_NET_MAC_ADDR: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x57];
const FOUR_GIB: u64 = 0x1_0000_0000;
const IA32_APIC_BASE_BSP_BIT: u64 = 1 << 8;
// Use a sparse RAM backend once memory sizes exceed this threshold to avoid accidentally
// allocating multi-GB buffers in tests and constrained environments.
const SPARSE_RAM_THRESHOLD_BYTES: u64 = 512 * 1024 * 1024;

fn sync_msi_capability_into_config(
    cfg: &mut aero_devices::pci::PciConfigSpace,
    enabled: bool,
    addr: u64,
    data: u16,
    mask: u32,
) {
    let Some(off) = cfg.find_capability(aero_devices::pci::msi::PCI_CAP_ID_MSI) else {
        return;
    };
    let base = u16::from(off);

    // Preserve read-only capability bits (64-bit + per-vector masking) by only mutating the MSI
    // Enable bit in Message Control.
    let ctrl = cfg.read(base + 0x02, 2) as u16;
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;

    cfg.write(base + 0x04, 4, addr as u32);
    if is_64bit {
        cfg.write(base + 0x08, 4, (addr >> 32) as u32);
        cfg.write(base + 0x0c, 2, u32::from(data));
        if per_vector_masking {
            cfg.write(base + 0x10, 4, mask);
        }
    } else {
        cfg.write(base + 0x08, 2, u32::from(data));
        if per_vector_masking {
            cfg.write(base + 0x0c, 4, mask);
        }
    }

    let new_ctrl = if enabled {
        ctrl | 0x0001
    } else {
        ctrl & !0x0001
    };
    cfg.write(base + 0x02, 2, u32::from(new_ctrl));
}

fn sync_msix_capability_into_config(
    cfg: &mut aero_devices::pci::PciConfigSpace,
    enabled: bool,
    function_masked: bool,
) {
    let Some(off) = cfg.find_capability(PCI_CAP_ID_MSIX) else {
        return;
    };
    let base = u16::from(off);
    let ctrl = cfg.read(base + 0x02, 2) as u16;
    let mut new_ctrl = ctrl;
    if enabled {
        new_ctrl |= 1 << 15;
    } else {
        new_ctrl &= !(1 << 15);
    }
    if function_masked {
        new_ctrl |= 1 << 14;
    } else {
        new_ctrl &= !(1 << 14);
    }
    cfg.write(base + 0x02, 2, u32::from(new_ctrl));
}

type NvmeDisk = Box<dyn aero_storage::VirtualDisk>;
fn set_cpu_apic_base_bsp_bit(cpu: &mut CpuCore, is_bsp: bool) {
    if is_bsp {
        cpu.state.msr.apic_base |= IA32_APIC_BASE_BSP_BIT;
    } else {
        cpu.state.msr.apic_base &= !IA32_APIC_BASE_BSP_BIT;
    }
}

pub mod pc;
pub use pc::{PcMachine, PcMachineConfig};

trait LegacyVgaFrontend: aero_gpu_vga::PortIO {
    fn set_text_mode_80x25(&mut self);
    fn set_mode_13h(&mut self);
}

impl LegacyVgaFrontend for VgaDevice {
    fn set_text_mode_80x25(&mut self) {
        VgaDevice::set_text_mode_80x25(self);
    }

    fn set_mode_13h(&mut self) {
        VgaDevice::set_mode_13h(self);
    }
}

impl aero_gpu_vga::PortIO for AeroGpuDevice {
    fn port_read(&mut self, port: u16, size: usize) -> u32 {
        let size = match size {
            0 => return 0,
            1 | 2 | 4 => size,
            _ => return u32::MAX,
        };

        let mut out = 0u32;
        for i in 0..size {
            let p = port.wrapping_add(i as u16);
            let b = self.vga_port_read_u8(p) as u32;
            out |= b << (i * 8);
        }
        out
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        let size = match size {
            1 | 2 | 4 => size,
            _ => return,
        };

        for i in 0..size {
            let p = port.wrapping_add(i as u16);
            let b = ((val >> (i * 8)) & 0xFF) as u8;
            self.vga_port_write_u8(p, b);
        }
    }
}

impl LegacyVgaFrontend for AeroGpuDevice {
    fn set_text_mode_80x25(&mut self) {
        // Approximate VGA text mode defaults used by `aero_gpu_vga::VgaDevice::set_text_mode_80x25`.
        self.attr_flip_flop = false;
        self.attr_index = 0;
        self.seq_index = 0;
        self.gc_index = 0;
        self.crtc_index = 0;

        // Attribute mode control: bit0=0 => text.
        self.attr_regs[0x10] = 1 << 2; // line graphics enable
                                       // Enable all 4 color planes by default.
        self.attr_regs[0x12] = 0x0F; // color plane enable
        self.attr_regs[0x14] = 0x00; // color select
                                     // Identity palette mapping for indices 0..15.
        for i in 0..16 {
            self.attr_regs[i] = i as u8;
        }

        // Sequencer memory mode: chain-4 disabled, odd/even enabled.
        self.seq_regs[4] = 0x02;
        // Sequencer map mask: enable planes 0 and 1 for text.
        self.seq_regs[2] = 0x03;

        // Graphics controller misc: memory map = 0b11 => B8000, and odd/even.
        self.gc_regs[6] = 0x0C;
        self.gc_regs[5] = 0x10; // odd/even
        self.gc_regs[4] = 0x00; // read map select

        // Cursor + start address defaults.
        self.crtc_regs[0x0A] = 0x00;
        self.crtc_regs[0x0B] = 0x0F;
        self.crtc_regs[0x0C] = 0x00;
        self.crtc_regs[0x0D] = 0x00;
        self.crtc_regs[0x0E] = 0x00;
        self.crtc_regs[0x0F] = 0x00;
    }

    fn set_mode_13h(&mut self) {
        // Approximate VGA mode 13h defaults used by `aero_gpu_vga::VgaDevice::set_mode_13h`.
        self.attr_flip_flop = false;
        self.attr_index = 0;
        self.seq_index = 0;
        self.gc_index = 0;
        self.crtc_index = 0;

        // Attribute mode control: graphics enable.
        self.attr_regs[0x10] = 0x01;
        self.attr_regs[0x12] = 0x0F;
        self.attr_regs[0x14] = 0x00;
        for i in 0..16 {
            self.attr_regs[i] = i as u8;
        }

        // Sequencer memory mode: enable chain-4 and disable odd/even.
        self.seq_regs[4] = 0x0E;
        self.seq_regs[2] = 0x0F;

        // Graphics controller misc: memory map = 0b01 => A0000 64KB.
        self.gc_regs[6] = 0x04;
        self.gc_regs[5] = 0x40; // 256-color shift register, no odd/even
        self.gc_regs[4] = 0x00;

        // Reset cursor/start address regs to a deterministic baseline.
        self.crtc_regs[0x0C] = 0x00;
        self.crtc_regs[0x0D] = 0x00;
        self.crtc_regs[0x0E] = 0x00;
        self.crtc_regs[0x0F] = 0x00;
    }
}

/// Canonical BIOS boot device selection.
///
/// This is a high-level policy knob used by [`Machine::reset`] to decide which attached media the
/// firmware should attempt to boot from.
///
/// `Cdrom` corresponds to booting from the conventional BIOS CD-ROM drive range (`DL=0xE0..=0xEF`)
/// when used via [`Machine::set_boot_device`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BootDevice {
    /// Boot from the primary HDD (the machine's canonical [`SharedDisk`]).
    #[default]
    Hdd,
    /// Boot from the install media ISO (IDE secondary master ATAPI) as the first CD-ROM drive
    /// number (`DL=0xE0`).
    ///
    /// Note: For a “CD-first when present, otherwise fall back to HDD” boot policy, use the
    /// firmware-level `boot_from_cd_if_present` flag (see [`Machine::set_boot_from_cd_if_present`]).
    Cdrom,
}

/// AeroGPU submission payload drained from the guest-visible AeroGPU ring.
///
/// This is an integration hook for browser/WASM builds where the guest-visible AeroGPU PCI device
/// model runs in-process (inside `aero_machine::Machine`), but command execution happens
/// out-of-process (e.g. in a dedicated GPU worker).
///
/// The `cmd_stream` bytes contain the raw `aerogpu_cmd_stream_header` + packets ("ACMD" stream).
#[derive(Clone, Debug)]
pub struct AerogpuSubmission {
    pub flags: u32,
    pub context_id: u32,
    pub engine_id: u32,
    pub signal_fence: u64,
    pub cmd_stream: Vec<u8>,
    pub alloc_table: Option<Vec<u8>>,
}
/// Configuration for [`Machine`].
///
/// # Platform wiring vs firmware tables
///
/// [`MachineConfig::enable_pc_platform`] controls which device models are actually wired into the
/// machine (PIC/APIC/PIT/RTC/PCI/ACPI PM/HPET). [`MachineConfig::enable_acpi`] controls whether the
/// BIOS publishes ACPI tables describing that PC-like platform.
#[derive(Debug, Clone)]
pub struct MachineConfig {
    /// Guest RAM size in bytes.
    pub ram_size_bytes: u64,
    /// BIOS drive number used for the initial boot attempt (placed in `DL` when jumping to the
    /// boot sector / El Torito boot image).
    ///
    /// Default is `0x80` (first HDD). For the canonical Windows 7 install ISO boot flow, use
    /// `0xE0` (first CD-ROM drive number).
    pub boot_drive: u8,
    /// Number of vCPUs exposed via firmware tables (SMBIOS + ACPI).
    ///
    /// Must be >= 1.
    ///
    /// `aero_machine::Machine` supports configuring `cpu_count > 1` for **SMP bring-up**:
    /// per-vCPU LAPIC state/MMIO routing, AP wait-for-SIPI state, INIT+SIPI delivery via the LAPIC
    /// ICR, and a bounded cooperative AP execution loop inside [`Machine::run_slice`].
    ///
    /// This is sufficient for SMP contract tests, but it is **not** a full SMP scheduler or
    /// parallel vCPU execution environment yet. For real guest boots, prefer `cpu_count=1`.
    ///
    /// See `docs/21-smp.md` for the current SMP status and roadmap.
    pub cpu_count: u8,
    /// Preferred BIOS boot device (HDD vs CD-ROM).
    ///
    /// This is a higher-level selector for choosing a raw BIOS drive number without the caller
    /// needing to know the conventional `DL` ranges.
    ///
    /// - [`BootDevice::Hdd`] corresponds to `boot_drive=0x80` (HDD0).
    /// - [`BootDevice::Cdrom`] corresponds to `boot_drive=0xE0` (CD0).
    ///
    /// Note: This is an **explicit selection** (it does not automatically fall back). For a
    /// “CD-first when present, otherwise HDD” policy, enable
    /// [`Machine::set_boot_from_cd_if_present`].
    ///
    /// When using the provided [`Machine`] setters (`set_boot_device` / `set_boot_drive`), this is
    /// kept in sync with [`MachineConfig::boot_drive`].
    pub boot_device: BootDevice,
    /// Deterministic seed used to generate the SMBIOS Type 1 "System UUID".
    ///
    /// Runtimes that need stable per-VM identities (e.g. Windows guests) should set this to a
    /// stable value. The default (`0`) keeps tests deterministic.
    ///
    /// Forwarded to [`firmware::bios::BiosConfig::smbios_uuid_seed`].
    pub smbios_uuid_seed: u64,
    /// Whether to attach canonical PC platform devices (PIC/APIC/PIT/RTC/PCI/ACPI PM/HPET).
    ///
    /// This is currently opt-in to keep the default machine minimal and deterministic.
    ///
    /// Note: this controls *device wiring*; BIOS publication of ACPI tables is controlled
    /// separately via [`MachineConfig::enable_acpi`].
    pub enable_pc_platform: bool,
    /// Whether the BIOS should build and publish ACPI tables during POST.
    ///
    /// ACPI tables should only be published when they accurately describe the machine's platform
    /// wiring (PIC/APIC/PIT/RTC/PCI/ACPI PM/HPET). In [`Machine`], that wiring is controlled by
    /// [`MachineConfig::enable_pc_platform`], so callers typically want:
    ///
    /// - `enable_pc_platform=true` and `enable_acpi=true` for a PC-like machine, and
    /// - `enable_pc_platform=false` and `enable_acpi=false` for a minimal legacy BIOS machine.
    ///
    /// Note: `Machine` only publishes ACPI tables when *both* `enable_pc_platform` and
    /// `enable_acpi` are set.
    pub enable_acpi: bool,
    /// Whether to attach an Intel ICH9 AHCI SATA controller at the canonical Windows 7 BDF
    /// (`aero_devices::pci::profile::SATA_AHCI_ICH9.bdf`, `00:02.0`).
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_ahci: bool,
    /// Whether to attach an NVMe controller at the canonical Windows 7 BDF
    /// (`aero_devices::pci::profile::NVME_CONTROLLER.bdf`, `00:03.0`).
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_nvme: bool,
    /// Whether to attach an Intel PIIX3 IDE controller at the canonical Windows 7 BDF
    /// (`aero_devices::pci::profile::IDE_PIIX3.bdf`, `00:01.1`).
    ///
    /// Note: When enabled, the PIIX3 ISA bridge function (`aero_devices::pci::profile::ISA_PIIX3`
    /// at `00:01.0`) is also exposed with the multi-function bit set so OSes enumerate function 1
    /// reliably.
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_ide: bool,
    /// Whether to attach a virtio-blk controller at the canonical Windows 7 BDF
    /// (`aero_devices::pci::profile::VIRTIO_BLK.bdf`, `00:09.0`).
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_virtio_blk: bool,
    /// Whether to attach virtio-input keyboard + mouse devices (virtio-pci modern transport).
    ///
    /// This exposes a multi-function PCI device with two functions at stable BDFs:
    /// - Keyboard: `aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD.bdf` (`00:0a.0`)
    /// - Mouse: `aero_devices::pci::profile::VIRTIO_INPUT_MOUSE.bdf` (`00:0a.1`)
    ///
    /// Virtio-input is the intended low-latency “fast path” for keyboard/mouse once the guest
    /// driver is installed (see `docs/08-input-devices.md`).
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_virtio_input: bool,
    /// Whether to attach an Intel PIIX3 UHCI (USB 1.1) controller at the canonical BDF
    /// (`aero_devices::pci::profile::USB_UHCI_PIIX3.bdf`, `00:01.2`).
    ///
    /// Note: Like IDE, UHCI is function 2 of the multi-function PIIX3 device, so enabling this
    /// will also expose the PIIX3 ISA bridge function (`aero_devices::pci::profile::ISA_PIIX3` at
    /// `00:01.0`) with the multi-function bit set so OSes enumerate function 2 reliably.
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_uhci: bool,
    /// Whether to attach an Intel ICH9-family EHCI (USB 2.0) controller at the canonical BDF
    /// (`aero_devices::pci::profile::USB_EHCI_ICH9.bdf`, `00:12.0`).
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_ehci: bool,
    /// Whether to attach an xHCI (USB 3.x) controller at the canonical BDF
    /// (`aero_devices::pci::profile::USB_XHCI_QEMU.bdf`, `00:0d.0`).
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_xhci: bool,
    /// Whether to attach an external USB hub behind the UHCI root hub (root port 0) and populate
    /// it with a fixed set of synthetic USB HID devices (keyboard + mouse + gamepad +
    /// consumer-control).
    ///
    /// The hub has 16 downstream ports to match the browser runtime topology.
    ///
    /// This matches the browser runtime topology described in `docs/08-input-devices.md`:
    /// - UHCI root port 0: external hub
    /// - hub port 1: USB HID keyboard
    /// - hub port 2: USB HID mouse
    /// - hub port 3: USB HID gamepad (Aero's fixed 8-byte report)
    /// - hub port 4: USB HID consumer-control (media keys, Usage Page 0x0C)
    ///
    /// Requires [`MachineConfig::enable_uhci`].
    pub enable_synthetic_usb_hid: bool,
    /// Whether to attach the legacy VGA/VBE device model.
    ///
    /// This is the transitional standalone VGA/VBE path used for BIOS/boot display and VGA-focused
    /// integration tests.
    ///
    /// When enabled, guest physical accesses to the legacy VGA window (`0xA0000..0xC0000`), the
    /// Bochs VBE linear framebuffer (LFB) at the configured base (see
    /// [`MachineConfig::vga_lfb_base`], default: [`aero_gpu_vga::SVGA_LFB_BASE`]), and VGA/VBE port
    /// I/O are routed to an [`aero_gpu_vga::VgaDevice`].
    ///
    /// Note: [`MachineConfig::enable_vga`] and [`MachineConfig::enable_aerogpu`] are mutually
    /// exclusive; [`Machine::new`] rejects configurations that enable both to avoid conflicting
    /// ownership of the VGA legacy ranges.
    ///
    /// Port mappings:
    ///
    /// - Legacy VGA ports: `0x3B0..0x3DF` (covers both mono and color decode ranges, e.g.
    ///   `0x3B4/0x3B5` and `0x3D4/0x3D5`)
    /// - Bochs VBE: [`aero_gpu_vga::VBE_DISPI_INDEX_PORT`] (index),
    ///   [`aero_gpu_vga::VBE_DISPI_DATA_PORT`] (data)
    pub enable_vga: bool,
    /// Optional override for the legacy VGA/VBE Bochs VBE linear framebuffer (LFB) base address.
    ///
    /// This is the guest physical address used for:
    /// - the LFB MMIO mapping when [`MachineConfig::enable_pc_platform`] is `true` (the LFB lives
    ///   inside the ACPI-reported PCI MMIO window, but is not exposed as a separate PCI function),
    /// - the direct MMIO mapping when [`MachineConfig::enable_pc_platform`] is `false`, and
    /// - the BIOS VBE mode info `PhysBasePtr` (so guests learn the correct LFB address).
    ///
    /// When unset, defaults to [`aero_gpu_vga::SVGA_LFB_BASE`].
    ///
    /// Note: This is only used when the standalone legacy VGA/VBE path is active
    /// (`enable_vga=true` and `enable_aerogpu=false`).
    ///
    /// When [`MachineConfig::enable_pc_platform`] is `true`, this base must lie within the
    /// ACPI-reported PCI MMIO window (`aero_pc_constants::PCI_MMIO_BASE..PCI_MMIO_END_EXCLUSIVE`),
    /// otherwise [`Machine::new`] will reject the configuration (the PC platform routes the legacy
    /// LFB mapping through its PCI MMIO window dispatcher).
    pub vga_lfb_base: Option<u32>,
    /// Optional override for the legacy VGA/VBE VRAM layout: the offset within VRAM where the VBE
    /// linear framebuffer (LFB) begins.
    ///
    /// This is forwarded to [`aero_gpu_vga::VgaConfig::lfb_offset`]. It controls how much VRAM is
    /// reserved for legacy VGA planar storage at the start of `vram` before packed-pixel VBE modes
    /// begin.
    ///
    /// When unset, defaults to [`aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET`] (`0x40000`, 256KiB).
    ///
    /// Note: This is only used when the standalone legacy VGA/VBE path is active
    /// (`enable_vga=true` and `enable_aerogpu=false`).
    pub vga_lfb_offset: Option<u32>,
    /// Optional override for the legacy VGA/VBE "VRAM aperture" base address.
    ///
    /// This exists to support BAR-layout experiments where the VBE linear framebuffer base is
    /// derived as:
    ///
    /// `lfb_base = vga_vram_bar_base + vga_lfb_offset`
    ///
    /// When unset, the machine derives a coherent `vram_bar_base` from the configured LFB base and
    /// LFB offset (`vram_bar_base = lfb_base - lfb_offset`).
    ///
    /// Note: This is only used when the standalone legacy VGA/VBE path is active
    /// (`enable_vga=true` and `enable_aerogpu=false`).
    pub vga_vram_bar_base: Option<u32>,
    /// Optional override for the total legacy VGA/VBE VRAM backing size in bytes.
    ///
    /// This controls the size of the emulated VRAM allocation (and the size of the legacy LFB MMIO
    /// aperture when the PC platform is enabled). It must be large enough to accommodate the
    /// legacy VGA plane region plus the VBE framebuffer region.
    ///
    /// When unset, defaults to [`aero_gpu_vga::DEFAULT_VRAM_SIZE`].
    ///
    /// Note: This is only used when the standalone legacy VGA/VBE path is active
    /// (`enable_vga=true` and `enable_aerogpu=false`).
    pub vga_vram_size_bytes: Option<usize>,
    /// Whether to expose the canonical AeroGPU PCI device (`aero_devices::pci::profile::AEROGPU`)
    /// with a dedicated VRAM aperture.
    ///
    /// This exposes the canonical AeroGPU PCI identity at
    /// `aero_devices::pci::profile::AEROGPU.bdf` (currently `00:07.0`, `VID:DID = A3A0:0001`).
    ///
    /// When enabled, the machine wires:
    ///
    /// - BAR0: an MVP AeroGPU MMIO register block (MAGIC/ABI/FEATURES, ring/fence transport,
    ///   IRQ status/enable/ack, scanout0 + cursor registers, and vblank counters) suitable for WDDM
    ///   driver detection and `D3DKMTWaitForVerticalBlankEvent` pacing.
    /// - BAR1: a dedicated VRAM aperture:
    ///   - the legacy VGA window (`0xA0000..0xC0000`) is an alias of `VRAM[0..0x20000)` (128KiB),
    ///   - the first 256KiB is reserved for legacy VGA planar storage (4 × 64KiB planes), and
    ///   - the BIOS VBE linear framebuffer begins at `BAR1_BASE + VBE_LFB_OFFSET` (`0x40000`,
    ///     `AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`).
    ///
    /// This is the foundation required by `docs/16-aerogpu-vga-vesa-compat.md` for
    /// firmware/bootloader compatibility and for the guest WDDM driver to claim scanout.
    ///
    /// Note: `aero-machine` does not execute `AEROGPU_CMD` in-process by default. Instead, this
    /// flag provides the guest-visible transport and a backend boundary for host-driven execution:
    ///
    /// - BAR1-backed VRAM plus minimal legacy VGA decode.
    /// - BAR0 ring/fence transport + scanout/cursor/vblank registers.
    ///   - Ring processing decodes submissions and can capture `AEROGPU_CMD` payloads into a bounded
    ///     queue for host-driven execution (`Machine::aerogpu_drain_submissions`).
    ///   - Fence forward-progress policy is selectable:
    ///     - default (no backend, submission bridge disabled): fences complete automatically
    ///       (bring-up / no-op execution), with optional vblank pacing when the submission contains
    ///       a vsync present.
    ///     - submission bridge enabled (`Machine::aerogpu_enable_submission_bridge`): fences are
    ///       deferred until the host reports completion (`Machine::aerogpu_complete_fence`).
    ///     - in-process backend installed (`Machine::aerogpu_set_backend_*`): fence completion is
    ///       driven by backend completions.
    ///
    /// Host-side scanout presentation (`Machine::display_present`) prefers the WDDM scanout
    /// framebuffer once claimed by a valid scanout0 configuration. Disabling scanout
    /// (`SCANOUT0_ENABLE=0`) blanks output but does not release WDDM ownership until reset.
    ///
    /// Machine snapshots preserve the BAR0 register file and the BAR1 VRAM backing store
    /// deterministically.
    ///
    /// Requires [`MachineConfig::enable_pc_platform`] (PCI enumeration required) and is mutually
    /// exclusive with [`MachineConfig::enable_vga`].
    pub enable_aerogpu: bool,
    /// Whether to attach a COM1 16550 serial device at `0x3F8`.
    pub enable_serial: bool,
    /// Whether to attach an ISA DebugCon logging port at `0xE9`.
    ///
    /// This is a Bochs/QEMU-compatible debug device used for simple early boot logging (e.g. before
    /// the guest initializes a serial console).
    pub enable_debugcon: bool,
    /// Whether to attach a legacy i8042 controller at ports `0x60/0x64`.
    pub enable_i8042: bool,
    /// Whether to attach a "fast A20" gate device at port `0x92`.
    pub enable_a20_gate: bool,
    /// Whether to attach a reset control device at port `0xCF9`.
    pub enable_reset_ctrl: bool,
    /// Whether to attach an Intel E1000 (82540EM-ish) PCI NIC.
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_e1000: bool,
    /// Optional MAC address for the E1000 NIC.
    pub e1000_mac_addr: Option<[u8; 6]>,
    /// Whether to attach a virtio-net PCI NIC (virtio-pci **modern** transport).
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_virtio_net: bool,
    /// Optional MAC address for the virtio-net device.
    pub virtio_net_mac_addr: Option<[u8; 6]>,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            ram_size_bytes: 64 * 1024 * 1024,
            boot_drive: 0x80,
            cpu_count: 1,
            boot_device: BootDevice::Hdd,
            smbios_uuid_seed: 0,
            enable_pc_platform: false,
            enable_acpi: false,
            enable_ahci: false,
            enable_nvme: false,
            enable_ide: false,
            enable_virtio_blk: false,
            enable_virtio_input: false,
            enable_uhci: false,
            enable_ehci: false,
            enable_xhci: false,
            enable_synthetic_usb_hid: false,
            enable_aerogpu: false,
            enable_vga: true,
            vga_lfb_base: None,
            vga_lfb_offset: None,
            vga_vram_bar_base: None,
            vga_vram_size_bytes: None,
            enable_serial: true,
            enable_debugcon: true,
            enable_i8042: true,
            enable_a20_gate: true,
            enable_reset_ctrl: true,
            enable_e1000: false,
            e1000_mac_addr: None,
            enable_virtio_net: false,
            virtio_net_mac_addr: None,
        }
    }
}

impl MachineConfig {
    /// Configuration preset for the canonical Windows 7 storage topology.
    ///
    /// This enables the controller set described in `docs/05-storage-topology-win7.md`:
    ///
    /// - AHCI (ICH9) at `00:02.0`
    /// - IDE (PIIX3) at `00:01.1` (with the accompanying PIIX3 ISA function at `00:01.0` so OSes
    ///   enumerate the multi-function device correctly)
    ///
    /// Other devices are set to explicit, stable defaults to avoid drift:
    ///
    /// - PC platform enabled (PIC/APIC/PIT/RTC/PCI/ACPI PM/HPET) and ACPI tables published
    /// - `cpu_count` defaults to 1 (override as desired)
    /// - serial (`COM1`) enabled
    /// - i8042 enabled (keyboard/mouse)
    /// - fast A20 gate enabled (port `0x92`)
    /// - reset control enabled (port `0xCF9`)
    /// - VGA enabled (transitional; for AeroGPU PCI identity, use [`MachineConfig::win7_graphics`])
    /// - E1000 disabled
    #[must_use]
    pub fn win7_storage(ram_size_bytes: u64) -> Self {
        Self {
            ram_size_bytes,
            boot_drive: 0x80,
            cpu_count: 1,
            boot_device: BootDevice::Hdd,
            smbios_uuid_seed: 0,
            enable_pc_platform: true,
            enable_acpi: true,
            enable_ahci: true,
            enable_nvme: false,
            enable_ide: true,
            enable_virtio_blk: false,
            enable_virtio_input: false,
            enable_uhci: false,
            enable_ehci: false,
            enable_xhci: false,
            enable_synthetic_usb_hid: false,
            enable_aerogpu: false,
            enable_vga: true,
            vga_lfb_base: None,
            vga_lfb_offset: None,
            vga_vram_bar_base: None,
            vga_vram_size_bytes: None,
            enable_serial: true,
            enable_debugcon: true,
            enable_i8042: true,
            enable_a20_gate: true,
            enable_reset_ctrl: true,
            enable_e1000: false,
            e1000_mac_addr: None,
            enable_virtio_net: false,
            virtio_net_mac_addr: None,
        }
    }

    /// Configuration preset for the canonical Windows 7 storage topology with AeroGPU enabled.
    ///
    /// This is equivalent to [`MachineConfig::win7_storage`], but:
    /// - exposes the canonical AeroGPU PCI identity at `00:07.0` (`A3A0:0001`), and
    /// - disables the transitional standalone VGA/VBE path (`enable_vga=false`).
    ///
    /// Note: `enable_aerogpu` currently wires BAR1-backed VRAM plus legacy VGA aliasing and an MVP
    /// BAR0 register block (ring/fence transport + scanout/cursor + vblank pacing + submission
    /// capture).
    ///
    /// By default (no backend, submission bridge disabled), the device model completes fences
    /// without executing the command stream so early guests don't deadlock during bring-up.
    /// Command execution can be supplied by:
    ///
    /// - the browser submission bridge (`Machine::aerogpu_drain_submissions` +
    ///   `Machine::aerogpu_complete_fence`), or
    /// - an in-process backend (`Machine::aerogpu_set_backend_*`, including the feature-gated
    ///   native wgpu backend).
    ///
    /// Host-side scanout presentation (`Machine::display_present`) can present WDDM scanout once it
    /// is claimed by a valid scanout0 configuration.
    #[must_use]
    pub fn win7_graphics(ram_size_bytes: u64) -> Self {
        let mut cfg = Self::win7_storage(ram_size_bytes);
        cfg.enable_aerogpu = true;
        cfg.enable_vga = false;
        cfg
    }

    /// Configuration preset for the canonical Windows 7 install / recovery flow (boot from CD).
    ///
    /// This is equivalent to [`MachineConfig::win7_storage_defaults`], but configures firmware to
    /// boot from the first CD-ROM drive (`boot_drive = 0xE0`) so El Torito install media boots
    /// without additional boilerplate.
    ///
    /// Note: this preset only configures the BIOS boot drive number. Callers should still attach a
    /// bootable ISO to the canonical install-media attachment point (PIIX3 IDE secondary master
    /// ATAPI) and, for the full `Machine` integration, also provide an ISO backend for firmware
    /// INT 13h CD reads (see [`Machine::configure_win7_install_boot`]).
    #[must_use]
    pub fn win7_install_defaults(ram_size_bytes: u64) -> Self {
        let mut cfg = Self::win7_storage_defaults(ram_size_bytes);
        cfg.boot_drive = 0xE0;
        cfg.boot_device = BootDevice::Cdrom;
        cfg
    }

    /// Alias for [`MachineConfig::win7_storage`].
    ///
    /// This exists as a more explicit "defaults" naming for callers that want to start from the
    /// canonical Windows 7 storage controller set and then tweak other fields (for example, opt
    /// into the E1000 NIC by setting [`MachineConfig::enable_e1000`] to `true`).
    ///
    /// This preset leaves [`MachineConfig::cpu_count`] at 1 by default; callers can override it if
    /// they want to run with more vCPUs.
    #[must_use]
    pub fn win7_storage_defaults(ram_size_bytes: u64) -> Self {
        Self::win7_storage(ram_size_bytes)
    }

    /// Configuration preset for the canonical browser runtime machine.
    ///
    /// This is the "batteries included" configuration that the wasm-bindgen wrapper
    /// (`crates/aero-wasm`) uses for `new Machine(ramSize)`.
    ///
    /// This starts from [`MachineConfig::win7_storage_defaults`] (canonical Windows 7 storage
    /// topology) and then enables the guest-visible devices expected by the browser runtime:
    ///
    /// - E1000 NIC enabled (virtio-net disabled)
    /// - UHCI (USB 1.1) enabled
    /// - AeroGPU enabled (`00:07.0`, `A3A0:0001`) for Windows driver binding
    ///
    /// See `docs/05-storage-topology-win7.md` for the normative storage BDFs and media attachment
    /// mapping.
    #[must_use]
    pub fn browser_defaults(ram_size_bytes: u64) -> Self {
        let mut cfg = Self::win7_storage_defaults(ram_size_bytes);

        // Browser runtime expects a guest-visible NIC and (optionally) USB.
        cfg.enable_e1000 = true;
        cfg.enable_virtio_net = false;
        cfg.enable_uhci = true;
        cfg.enable_xhci = false;

        // Keep Win7 storage topology explicit even if `win7_storage_defaults` evolves.
        cfg.enable_ahci = true;
        cfg.enable_ide = true;
        cfg.enable_nvme = false;
        cfg.enable_virtio_blk = false;

        // Keep deterministic core devices explicit.
        cfg.enable_vga = false;
        cfg.enable_aerogpu = true;
        cfg.enable_serial = true;
        cfg.enable_i8042 = true;
        cfg.enable_a20_gate = true;
        cfg.enable_reset_ctrl = true;

        // Enforce the required platform topology for these devices.
        cfg.enable_pc_platform = true;
        cfg.cpu_count = 1;
        cfg.boot_drive = 0x80;

        cfg
    }
}

/// A single-step/run invocation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunExit {
    /// The slice completed because `max_insts` was reached.
    Completed { executed: u64 },
    /// The CPU executed `HLT`.
    Halted { executed: u64 },
    /// The guest requested a reset (e.g. via port `0xCF9`).
    ResetRequested { kind: ResetKind, executed: u64 },
    /// Execution stopped because the CPU core needs host assistance.
    Assist { reason: AssistReason, executed: u64 },
    /// Execution stopped due to an exception/fault.
    Exception { exception: Exception, executed: u64 },
    /// Execution stopped due to a fatal CPU exit condition (e.g. triple fault).
    CpuExit { exit: CpuExit, executed: u64 },
}

impl RunExit {
    /// Number of guest instructions executed in this slice (best-effort).
    pub fn executed(&self) -> u64 {
        match *self {
            RunExit::Completed { executed }
            | RunExit::Halted { executed }
            | RunExit::ResetRequested { executed, .. }
            | RunExit::Assist { executed, .. }
            | RunExit::Exception { executed, .. }
            | RunExit::CpuExit { executed, .. } => executed,
        }
    }
}

/// Errors returned when constructing or configuring a [`Machine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineError {
    InvalidCpuCount(u8),
    InvalidDiskSize(usize),
    DiskBackend(String),
    GuestMemoryTooLarge(u64),
    GuestMemorySizeMismatch {
        expected: u64,
        actual: u64,
    },
    /// Legacy VGA/VBE LFB range falls outside the ACPI-reported PCI MMIO window, which means the
    /// linear framebuffer will not be reachable via the PCI MMIO router when the PC platform is
    /// enabled.
    VgaLfbOutsidePciMmioWindow {
        requested_base: u32,
        aligned_base: u32,
        size: u32,
    },
    AhciRequiresPcPlatform,
    NvmeRequiresPcPlatform,
    IdeRequiresPcPlatform,
    VirtioBlkRequiresPcPlatform,
    VirtioInputRequiresPcPlatform,
    UhciRequiresPcPlatform,
    SyntheticUsbHidRequiresUhci,
    EhciRequiresPcPlatform,
    XhciRequiresPcPlatform,
    AeroGpuRequiresPcPlatform,
    AeroGpuConflictsWithVga,
    AeroGpuNotEnabled,
    E1000RequiresPcPlatform,
    VirtioNetRequiresPcPlatform,
    MultipleNicsEnabled,
}

impl fmt::Display for MachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachineError::InvalidCpuCount(count) => {
                write!(
                    f,
                    "invalid cpu_count={count}; must be >= 1. Note: SMP is still bring-up only (not a robust multi-vCPU environment yet); use cpu_count=1 for real guest boots. See docs/21-smp.md#status-today and docs/09-bios-firmware.md#smp-boot-bsp--aps"
                )
            }
            MachineError::InvalidDiskSize(len) => write!(
                f,
                "disk image length {len} is not a multiple of {} (BIOS sector size)",
                aero_storage::SECTOR_SIZE
            ),
            MachineError::DiskBackend(msg) => write!(f, "disk backend error: {msg}"),
            MachineError::GuestMemoryTooLarge(size) => write!(
                f,
                "guest RAM size {size} bytes does not fit in the current platform's usize"
            ),
            MachineError::GuestMemorySizeMismatch { expected, actual } => write!(
                f,
                "guest RAM backing size mismatch: expected {expected} bytes, got {actual} bytes"
            ),
            MachineError::VgaLfbOutsidePciMmioWindow {
                requested_base,
                aligned_base,
                size,
            } => {
                let requested_base = *requested_base;
                let aligned_base = *aligned_base;
                let size = *size;
                let window_start = PCI_MMIO_BASE;
                let window_end = PCI_MMIO_BASE + PCI_MMIO_SIZE;
                let lfb_end = u64::from(aligned_base).saturating_add(u64::from(size));
                write!(
                    f,
                    "invalid vga_lfb_base for enable_pc_platform=true: requested={requested_base:#010x}, aligned={aligned_base:#010x}, size={size:#x} => LFB window [{aligned_base:#010x}, {lfb_end:#010x}) must fit inside PCI MMIO window [{window_start:#010x}, {window_end:#010x})"
                )
            }
            MachineError::AeroGpuRequiresPcPlatform => {
                write!(
                    f,
                    "enable_aerogpu requires enable_pc_platform=true (PCI enumeration required)"
                )
            }
            MachineError::AhciRequiresPcPlatform => {
                write!(f, "enable_ahci requires enable_pc_platform=true")
            }
            MachineError::NvmeRequiresPcPlatform => {
                write!(f, "enable_nvme requires enable_pc_platform=true")
            }
            MachineError::IdeRequiresPcPlatform => {
                write!(f, "enable_ide requires enable_pc_platform=true")
            }
            MachineError::VirtioBlkRequiresPcPlatform => {
                write!(f, "enable_virtio_blk requires enable_pc_platform=true")
            }
            MachineError::VirtioInputRequiresPcPlatform => {
                write!(f, "enable_virtio_input requires enable_pc_platform=true")
            }
            MachineError::UhciRequiresPcPlatform => {
                write!(f, "enable_uhci requires enable_pc_platform=true")
            }
            MachineError::SyntheticUsbHidRequiresUhci => {
                write!(f, "enable_synthetic_usb_hid requires enable_uhci=true")
            }
            MachineError::EhciRequiresPcPlatform => {
                write!(f, "enable_ehci requires enable_pc_platform=true")
            }
            MachineError::XhciRequiresPcPlatform => {
                write!(f, "enable_xhci requires enable_pc_platform=true")
            }
            MachineError::AeroGpuConflictsWithVga => {
                write!(
                    f,
                    "cannot enable both enable_aerogpu and enable_vga (choose exactly one GPU device)"
                )
            }
            MachineError::AeroGpuNotEnabled => {
                write!(f, "aerogpu device is not enabled (enable_aerogpu=false)")
            }
            MachineError::E1000RequiresPcPlatform => {
                write!(f, "enable_e1000 requires enable_pc_platform=true")
            }
            MachineError::VirtioNetRequiresPcPlatform => {
                write!(f, "enable_virtio_net requires enable_pc_platform=true")
            }
            MachineError::MultipleNicsEnabled => write!(
                f,
                "cannot enable both enable_e1000 and enable_virtio_net (choose exactly one NIC)"
            ),
        }
    }
}

impl std::error::Error for MachineError {}

struct SystemMemory {
    a20: A20GateHandle,
    bus: PlatformMemoryBus,
    dirty: DirtyTracker,
    mapped_roms: HashMap<u64, usize>,
    mapped_mmio: Vec<(u64, u64)>,
}

impl SystemMemory {
    // Reset/mapping strategy (Strategy C: idempotent mapping helpers):
    //
    // - The physical memory bus (RAM + ROM + MMIO routing) is constructed once in `Machine::new`
    //   and persists for the lifetime of the machine.
    // - `Machine::reset()` may be called multiple times, and BIOS POST / platform wiring may
    //   attempt to (re-)map the same ROM/MMIO windows each time.
    // - `aero_platform::memory::MemoryBus::map_rom/map_mmio` are strict and reject overlaps, so we
    //   provide idempotent mapping helpers (`FirmwareMemory::map_rom` and `map_mmio_once`) that
    //   treat identical re-maps as no-ops while still panicking on unexpected overlaps.
    fn new(ram_size_bytes: u64, a20: A20GateHandle) -> Result<Self, MachineError> {
        // Keep the RAM backing store contiguous in "RAM-offset space" `[0, ram_size_bytes)`, even
        // when the guest physical address space contains the PCI/ECAM/MMIO hole below 4GiB.
        //
        // For large configured sizes, prefer a sparse backend so unit tests can configure RAM just
        // above the ECAM base without allocating multiple GiB.
        let backing: Box<dyn memory::GuestMemory> = if ram_size_bytes <= SPARSE_RAM_THRESHOLD_BYTES
        {
            let ram = DenseMemory::new(ram_size_bytes)
                .map_err(|_| MachineError::GuestMemoryTooLarge(ram_size_bytes))?;
            Box::new(ram)
        } else {
            let ram = SparseMemory::new(ram_size_bytes)
                .map_err(|_| MachineError::GuestMemoryTooLarge(ram_size_bytes))?;
            Box::new(ram)
        };

        // Dirty tracking must be in *RAM-offset space* so dirty page indices match the contiguous
        // RAM image used by snapshots even when the PC platform remaps high memory above 4GiB.
        let (backing, dirty) = DirtyGuestMemory::new(backing, SNAPSHOT_DIRTY_PAGE_SIZE);

        // `PlatformMemoryBus::with_ram` wraps the provided RAM backend with the PC high-memory
        // layout (PCI/ECAM hole + >4GiB remap) when `ram_size_bytes > PCIE_ECAM_BASE`.
        let filter = AddressFilter::new(a20.clone());
        let bus = PlatformMemoryBus::with_ram(filter, Box::new(backing));

        Ok(Self {
            a20,
            bus,
            dirty,
            mapped_roms: HashMap::new(),
            mapped_mmio: Vec::new(),
        })
    }

    fn new_with_backing(
        backing: Box<dyn memory::GuestMemory>,
        a20: A20GateHandle,
    ) -> Result<Self, MachineError> {
        // Dirty tracking must be in *RAM-offset space* so dirty page indices match the contiguous
        // RAM image used by snapshots even when the PC platform remaps high memory above 4GiB.
        let (backing, dirty) = DirtyGuestMemory::new(backing, SNAPSHOT_DIRTY_PAGE_SIZE);

        // `PlatformMemoryBus::with_ram` wraps the provided RAM backend with the PC high-memory
        // layout (PCI/ECAM hole + >4GiB remap) when `ram_size_bytes > PCIE_ECAM_BASE`.
        let filter = AddressFilter::new(a20.clone());
        let bus = PlatformMemoryBus::with_ram(filter, Box::new(backing));

        Ok(Self {
            a20,
            bus,
            dirty,
            mapped_roms: HashMap::new(),
            mapped_mmio: Vec::new(),
        })
    }

    fn take_dirty_pages(&mut self) -> Vec<u64> {
        self.dirty.take_dirty_pages()
    }

    fn clear_dirty(&mut self) {
        self.dirty.clear_dirty();
    }

    /// Map an MMIO region on the persistent physical memory bus exactly once.
    ///
    /// The machine's physical memory bus lives across `Machine::reset()` calls, so MMIO mappings
    /// are expected to be persistent. Callers may invoke this during every reset; identical
    /// mappings are treated as idempotent, while unexpected overlaps still panic to avoid silently
    /// corrupting the address space.
    #[allow(dead_code)]
    #[track_caller]
    fn map_mmio_once<F>(&mut self, start: u64, len: u64, build: F)
    where
        F: FnOnce() -> Box<dyn memory::MmioHandler>,
    {
        if len == 0 {
            return;
        }

        let end = match start.checked_add(len) {
            Some(end) => end,
            None => panic!("MMIO mapping overflow at 0x{start:016x} (len=0x{len:x})"),
        };

        // Fast path: mapping already exists (and was recorded by this helper).
        if self
            .mapped_mmio
            .iter()
            .any(|&(s, e)| s == start && e == end)
        {
            return;
        }

        let handler = build();
        match self.bus.map_mmio(start, len, handler) {
            Ok(()) => self.mapped_mmio.push((start, end)),
            Err(MapError::Overlap) => {
                // This should not happen for well-behaved callers because we short-circuit based
                // on `mapped_mmio` above. If it does, something attempted to create a conflicting
                // mapping; panic rather than silently corrupt the address space.
                panic!("unexpected MMIO mapping overlap at 0x{start:016x} (len=0x{len:x})");
            }
            Err(MapError::AddressOverflow) => {
                panic!("MMIO mapping overflow at 0x{start:016x} (len=0x{len:x})")
            }
        };
    }
}

impl A20Gate for SystemMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20.set_enabled(enabled);
    }

    fn a20_enabled(&self) -> bool {
        self.a20.enabled()
    }
}

impl FirmwareMemory for SystemMemory {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();

        if let Some(prev_len) = self.mapped_roms.get(&base).copied() {
            // BIOS resets may re-map the same ROM windows. Treat identical re-maps as no-ops, but
            // reject unexpected overlaps to avoid silently corrupting the address space.
            if prev_len != len {
                panic!("unexpected ROM mapping overlap at 0x{base:016x}");
            }
            return;
        }

        match self.bus.map_rom(base, rom) {
            Ok(()) => {
                self.mapped_roms.insert(base, len);
            }
            Err(MapError::Overlap) => {
                // This should not happen for well-behaved callers because we short-circuit based
                // on `mapped_roms` above. If it does, something attempted to create a conflicting
                // mapping; panic rather than silently corrupt the address space.
                panic!("unexpected ROM mapping overlap at 0x{base:016x}");
            }
            Err(MapError::AddressOverflow) => {
                panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})")
            }
        };
    }
}

impl memory::MemoryBus for SystemMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.bus.read_physical(paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.bus.write_physical(paddr, buf);
    }
}

impl aero_mmu::MemoryBus for SystemMemory {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        memory::MemoryBus::read_u8(self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        memory::MemoryBus::read_u16(self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        memory::MemoryBus::read_u32(self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        memory::MemoryBus::read_u64(self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        memory::MemoryBus::write_u8(self, paddr, value)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        memory::MemoryBus::write_u16(self, paddr, value)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        memory::MemoryBus::write_u32(self, paddr, value)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        memory::MemoryBus::write_u64(self, paddr, value)
    }
}

/// Per-vCPU physical memory bus adapter used for CPU execution.
///
/// The LAPIC MMIO window lives at a fixed physical address (`0xFEE0_0000`), but is architecturally
/// per-CPU: each vCPU sees a different LAPIC register page at that shared address. Our core RAM/MMIO
/// bus (`SystemMemory`) is shared across CPUs, so we intercept LAPIC MMIO accesses here and route
/// them explicitly based on the currently executing vCPU's APIC ID.
enum ApCpus<'a> {
    /// Full contiguous slice of application processor cores (APIC IDs 1..=N).
    ///
    /// This is the common case: the currently executing CPU is the BSP (APIC ID 0), so AP state is
    /// disjoint from the running core.
    All(&'a mut [CpuCore]),
    /// Application processor cores split around the currently executing AP.
    ///
    /// `before` covers APIC IDs `1..=before.len()`, `after` covers
    /// `before.len()+2..=len()`. The excluded APIC ID is the currently executing AP
    /// (`before.len()+1`) and is intentionally not accessible (to avoid aliasing the running
    /// `&mut CpuCore`).
    Split {
        before: &'a mut [CpuCore],
        after: &'a mut [CpuCore],
    },
}

impl ApCpus<'_> {
    fn get_mut(&mut self, idx: usize) -> Option<&mut CpuCore> {
        match self {
            ApCpus::All(ap_cpus) => ap_cpus.get_mut(idx),
            ApCpus::Split { before, after } => {
                if idx < before.len() {
                    before.get_mut(idx)
                } else if idx == before.len() {
                    None
                } else {
                    after.get_mut(idx - before.len() - 1)
                }
            }
        }
    }

    fn for_each_mut_excluding_current<F: FnMut(usize, &mut CpuCore)>(&mut self, mut f: F) {
        match self {
            ApCpus::All(ap_cpus) => {
                for (idx, cpu) in ap_cpus.iter_mut().enumerate() {
                    f(idx, cpu);
                }
            }
            ApCpus::Split { before, after } => {
                for (idx, cpu) in before.iter_mut().enumerate() {
                    f(idx, cpu);
                }
                let base = before.len() + 1;
                for (idx, cpu) in after.iter_mut().enumerate() {
                    f(base + idx, cpu);
                }
            }
        }
    }

    fn for_each_mut_excluding_index<F: FnMut(usize, &mut CpuCore)>(
        &mut self,
        exclude_idx: usize,
        mut f: F,
    ) {
        match self {
            ApCpus::All(ap_cpus) => {
                if exclude_idx >= ap_cpus.len() {
                    for (idx, cpu) in ap_cpus.iter_mut().enumerate() {
                        f(idx, cpu);
                    }
                    return;
                }

                let (before, rest) = ap_cpus.split_at_mut(exclude_idx);
                for (idx, cpu) in before.iter_mut().enumerate() {
                    f(idx, cpu);
                }
                let after = &mut rest[1..];
                let base = exclude_idx + 1;
                for (idx, cpu) in after.iter_mut().enumerate() {
                    f(base + idx, cpu);
                }
            }
            // In the `Split` case, the excluded CPU is not accessible, so excluding an index is
            // either redundant (if it's the current AP) or safe to handle via a skip.
            ApCpus::Split { before, after } => {
                for (idx, cpu) in before.iter_mut().enumerate() {
                    if idx == exclude_idx {
                        continue;
                    }
                    f(idx, cpu);
                }
                let base = before.len() + 1;
                for (idx, cpu) in after.iter_mut().enumerate() {
                    let full_idx = base + idx;
                    if full_idx == exclude_idx {
                        continue;
                    }
                    f(full_idx, cpu);
                }
            }
        }
    }
}

struct PerCpuSystemMemoryBus<'a> {
    apic_id: u8,
    interrupts: Option<Rc<RefCell<PlatformInterrupts>>>,
    ap_cpus: ApCpus<'a>,
    mem: &'a mut SystemMemory,
}

impl<'a> PerCpuSystemMemoryBus<'a> {
    fn new(
        apic_id: u8,
        interrupts: Option<Rc<RefCell<PlatformInterrupts>>>,
        ap_cpus: ApCpus<'a>,
        mem: &'a mut SystemMemory,
    ) -> Self {
        Self {
            apic_id,
            interrupts,
            ap_cpus,
            mem,
        }
    }

    fn maybe_deliver_ipi(&mut self, icr_low: u32, icr_high: u32) {
        self.maybe_deliver_init_ipi(icr_low, icr_high);
        self.maybe_deliver_startup_ipi(icr_low, icr_high);
        self.maybe_deliver_fixed_ipi(icr_low, icr_high);
    }

    fn maybe_deliver_init_ipi(&mut self, icr_low: u32, icr_high: u32) {
        // Intel SDM Vol. 3: ICR delivery mode.
        // 0b101 = INIT.
        let delivery_mode = (icr_low >> 8) & 0b111;
        if delivery_mode != 0b101 {
            return;
        }
        // Only model INIT "assert". INIT deassert is part of the INIT sequence but does not
        // perform the reset.
        let level_assert = ((icr_low >> 14) & 1) != 0;
        if !level_assert {
            return;
        }

        let shorthand = (icr_low >> 18) & 0b11;
        match shorthand {
            // No shorthand: use ICR_HIGH destination field (xAPIC physical destination).
            0 => {
                let dest = (icr_high >> 24) as u8;
                self.reset_ap_by_apic_id(dest);
            }
            // Self. Self-INIT is undefined; ignore.
            1 => {}
            // All including self: reset all APs (ignore self).
            2 => self.reset_all_aps(),
            // All excluding self: reset all APs except the sender (if it is an AP).
            3 => self.reset_all_aps_excluding_sender(),
            _ => {}
        }
    }

    fn maybe_deliver_startup_ipi(&mut self, icr_low: u32, icr_high: u32) {
        // Intel SDM Vol. 3: ICR delivery mode.
        // 0b110 = STARTUP (SIPI).
        let delivery_mode = (icr_low >> 8) & 0b111;
        if delivery_mode != 0b110 {
            return;
        }
        // STARTUP IPI is edge-triggered and does not have a deassert phase. Treat the ICR "level"
        // bit as don't-care and deliver whenever `delivery_mode` is STARTUP.

        let vector = (icr_low & 0xFF) as u8;
        let shorthand = (icr_low >> 18) & 0b11;
        match shorthand {
            0 => {
                let dest = (icr_high >> 24) as u8;
                if dest == 0xFF {
                    self.start_all_aps_excluding_sender(vector);
                } else {
                    self.start_ap_by_apic_id(dest, vector);
                }
            }
            // Self.
            1 => self.start_ap_by_apic_id(self.apic_id, vector),
            // All including self.
            2 => self.start_all_aps(vector),
            // All excluding self.
            3 => self.start_all_aps_excluding_sender(vector),
            _ => {}
        }
    }

    fn maybe_deliver_fixed_ipi(&mut self, icr_low: u32, icr_high: u32) {
        // Intel SDM Vol. 3: ICR delivery mode.
        // 0b000 = Fixed.
        let delivery_mode = (icr_low >> 8) & 0b111;
        if delivery_mode != 0b000 {
            return;
        }
        // Fixed IPIs are edge-triggered; treat the ICR "Level" bit as don't-care.
        let vector = (icr_low & 0xFF) as u8;
        let shorthand = (icr_low >> 18) & 0b11;
        match shorthand {
            0 => {
                let dest = (icr_high >> 24) as u8;
                if dest == 0xFF {
                    self.inject_fixed_all_excluding_sender(vector);
                } else {
                    self.inject_fixed_to_apic_id(dest, vector);
                }
            }
            // Self.
            1 => self.inject_fixed_to_apic_id(self.apic_id, vector),
            // All including self.
            2 => self.inject_fixed_to_all(vector),
            // All excluding self.
            3 => self.inject_fixed_all_excluding_sender(vector),
            _ => {}
        }
    }

    fn inject_fixed_to_apic_id(&mut self, apic_id: u8, vector: u8) {
        let Some(interrupts) = &self.interrupts else {
            return;
        };
        let Some(lapic) = interrupts.borrow().lapic_by_apic_id(apic_id) else {
            return;
        };
        lapic.inject_fixed_interrupt(vector);
    }

    fn inject_fixed_to_all(&mut self, vector: u8) {
        let Some(interrupts) = &self.interrupts else {
            return;
        };
        let cpu_count = interrupts.borrow().cpu_count();
        for apic_id in 0..cpu_count {
            self.inject_fixed_to_apic_id(apic_id as u8, vector);
        }
    }

    fn inject_fixed_all_excluding_sender(&mut self, vector: u8) {
        let Some(interrupts) = &self.interrupts else {
            return;
        };
        let cpu_count = interrupts.borrow().cpu_count();
        for apic_id in 0..cpu_count {
            let apic_id = apic_id as u8;
            if apic_id == self.apic_id {
                continue;
            }
            self.inject_fixed_to_apic_id(apic_id, vector);
        }
    }

    fn reset_ap_by_apic_id(&mut self, apic_id: u8) {
        if apic_id == 0 {
            // Destination 0 = BSP (ignore).
            return;
        }
        if apic_id == 0xFF {
            // Destination 0xFF is commonly used as a broadcast.
            self.reset_all_aps_excluding_sender();
            return;
        }

        let idx = (apic_id as usize).saturating_sub(1);
        if let Some(cpu) = self.ap_cpus.get_mut(idx) {
            vcpu_init::reset_ap_vcpu_to_init_state(cpu);
            set_cpu_apic_base_bsp_bit(cpu, false);
            // After INIT, application processors wait for a SIPI before executing.
            cpu.state.halted = true;
        }
    }

    fn reset_all_aps(&mut self) {
        self.ap_cpus.for_each_mut_excluding_current(|_idx, cpu| {
            vcpu_init::reset_ap_vcpu_to_init_state(cpu);
            set_cpu_apic_base_bsp_bit(cpu, false);
            cpu.state.halted = true;
        });
    }

    fn reset_all_aps_excluding_sender(&mut self) {
        if let Some(sender_idx) = self.apic_id.checked_sub(1).map(|v| v as usize) {
            self.ap_cpus
                .for_each_mut_excluding_index(sender_idx, |_idx, cpu| {
                    vcpu_init::reset_ap_vcpu_to_init_state(cpu);
                    set_cpu_apic_base_bsp_bit(cpu, false);
                    cpu.state.halted = true;
                });
        } else {
            self.reset_all_aps();
        }
    }
    fn start_ap_by_apic_id(&mut self, apic_id: u8, vector: u8) {
        if apic_id == 0 {
            // SIPI to BSP: ignore.
            return;
        }
        if apic_id == 0xFF {
            // Treat as broadcast.
            self.start_all_aps_excluding_sender(vector);
            return;
        }
        let idx = (apic_id as usize).saturating_sub(1);
        let Some(cpu) = self.ap_cpus.get_mut(idx) else {
            return;
        };
        vcpu_init::init_ap_vcpu_from_sipi(cpu, vector);
        set_cpu_apic_base_bsp_bit(cpu, false);
    }

    fn start_all_aps(&mut self, vector: u8) {
        self.ap_cpus.for_each_mut_excluding_current(|_idx, cpu| {
            vcpu_init::init_ap_vcpu_from_sipi(cpu, vector);
            set_cpu_apic_base_bsp_bit(cpu, false);
        });
    }

    fn start_all_aps_excluding_sender(&mut self, vector: u8) {
        if let Some(sender_idx) = self.apic_id.checked_sub(1).map(|v| v as usize) {
            self.ap_cpus
                .for_each_mut_excluding_index(sender_idx, |_idx, cpu| {
                    vcpu_init::init_ap_vcpu_from_sipi(cpu, vector);
                    set_cpu_apic_base_bsp_bit(cpu, false);
                });
        } else {
            self.start_all_aps(vector);
        }
    }
}

impl memory::MemoryBus for PerCpuSystemMemoryBus<'_> {
    fn read_physical(&mut self, mut paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        let Some(interrupts) = self.interrupts.clone() else {
            // No PC platform attached: fall back to the shared bus.
            self.mem.read_physical(paddr, buf);
            return;
        };

        let lapic_start = LAPIC_MMIO_BASE;
        let lapic_end = LAPIC_MMIO_BASE + LAPIC_MMIO_SIZE;

        let mut offset = 0usize;
        while offset < buf.len() {
            let remaining = buf.len() - offset;
            if paddr >= lapic_start && paddr < lapic_end {
                let chunk_len = ((lapic_end - paddr) as usize).min(remaining);
                interrupts.borrow().lapic_mmio_read_for_apic(
                    self.apic_id,
                    paddr - lapic_start,
                    &mut buf[offset..offset + chunk_len],
                );
                paddr = paddr.wrapping_add(chunk_len as u64);
                offset += chunk_len;
                continue;
            }

            // Not in the LAPIC window: forward to the shared SystemMemory bus, but avoid crossing
            // into the LAPIC range so the next iteration can intercept.
            let chunk_len = if paddr < lapic_start {
                let until_lapic = (lapic_start - paddr) as usize;
                remaining.min(until_lapic)
            } else {
                remaining
            };
            self.mem
                .read_physical(paddr, &mut buf[offset..offset + chunk_len]);
            paddr = paddr.wrapping_add(chunk_len as u64);
            offset += chunk_len;
        }
    }

    fn write_physical(&mut self, mut paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        let Some(interrupts) = self.interrupts.clone() else {
            // No PC platform attached: fall back to the shared bus.
            self.mem.write_physical(paddr, buf);
            return;
        };

        let lapic_start = LAPIC_MMIO_BASE;
        let lapic_end = LAPIC_MMIO_BASE + LAPIC_MMIO_SIZE;

        let mut offset = 0usize;
        while offset < buf.len() {
            let remaining = buf.len() - offset;
            if paddr >= lapic_start && paddr < lapic_end {
                let chunk_len = ((lapic_end - paddr) as usize).min(remaining);
                let lapic_offset = paddr - lapic_start;
                interrupts.borrow().lapic_mmio_write_for_apic(
                    self.apic_id,
                    lapic_offset,
                    &buf[offset..offset + chunk_len],
                );

                // Detect IPIs issued via the LAPIC ICR so the machine can model INIT/SIPI effects
                // on vCPU architectural state (resetting/starting APs).
                //
                // Note: The platform interrupt fabric also registers an ICR notifier on each
                // `LocalApic` so fixed vectors are injected into destination LAPICs and LAPIC
                // register reset semantics are applied. That path does not touch vCPU state.
                const ICR_LOW_OFF: u64 = 0x300;
                const ICR_HIGH_OFF: u64 = 0x310;
                let end_off = lapic_offset.wrapping_add(chunk_len as u64);
                let wrote_icr_low = lapic_offset < ICR_LOW_OFF + 4 && end_off > ICR_LOW_OFF;
                if wrote_icr_low {
                    let icr_low = {
                        let mut tmp = [0u8; 4];
                        interrupts.borrow().lapic_mmio_read_for_apic(
                            self.apic_id,
                            ICR_LOW_OFF,
                            &mut tmp,
                        );
                        u32::from_le_bytes(tmp)
                    };
                    let icr_high = {
                        let mut tmp = [0u8; 4];
                        interrupts.borrow().lapic_mmio_read_for_apic(
                            self.apic_id,
                            ICR_HIGH_OFF,
                            &mut tmp,
                        );
                        u32::from_le_bytes(tmp)
                    };
                    self.maybe_deliver_ipi(icr_low, icr_high);
                }
                paddr = paddr.wrapping_add(chunk_len as u64);
                offset += chunk_len;
                continue;
            }

            let chunk_len = if paddr < lapic_start {
                let until_lapic = (lapic_start - paddr) as usize;
                remaining.min(until_lapic)
            } else {
                remaining
            };
            self.mem
                .write_physical(paddr, &buf[offset..offset + chunk_len]);
            paddr = paddr.wrapping_add(chunk_len as u64);
            offset += chunk_len;
        }
    }
}

impl aero_mmu::MemoryBus for PerCpuSystemMemoryBus<'_> {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        memory::MemoryBus::read_u8(self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        memory::MemoryBus::read_u16(self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        memory::MemoryBus::read_u32(self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        memory::MemoryBus::read_u64(self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        memory::MemoryBus::write_u8(self, paddr, value)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        memory::MemoryBus::write_u16(self, paddr, value)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        memory::MemoryBus::write_u32(self, paddr, value)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        memory::MemoryBus::write_u64(self, paddr, value)
    }
}

struct StrictIoPortBus<'a> {
    io: &'a mut IoPortBus,
}

impl aero_cpu_core::paging_bus::IoBus for StrictIoPortBus<'_> {
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        match size {
            0 => Ok(0),
            1 | 2 | 4 => Ok(u64::from(self.io.read(port, size as u8))),
            _ => Err(Exception::InvalidOpcode),
        }
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        match size {
            0 => Ok(()),
            1 | 2 | 4 => {
                self.io.write(port, size as u8, val as u32);
                Ok(())
            }
            _ => Err(Exception::InvalidOpcode),
        }
    }
}

struct MachineCpuBus<'a> {
    a20: A20GateHandle,
    reset: ResetLatch,
    inner: aero_cpu_core::PagingBus<PerCpuSystemMemoryBus<'a>, StrictIoPortBus<'a>>,
}

impl aero_cpu_core::mem::CpuBus for MachineCpuBus<'_> {
    #[inline]
    fn sync(&mut self, state: &CpuState) {
        self.inner.sync(state);
    }

    #[inline]
    fn invlpg(&mut self, vaddr: u64) {
        self.inner.invlpg(vaddr);
    }

    #[inline]
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.inner.read_u8(vaddr)
    }

    #[inline]
    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        self.inner.read_u16(vaddr)
    }

    #[inline]
    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        self.inner.read_u32(vaddr)
    }

    #[inline]
    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.inner.read_u64(vaddr)
    }

    #[inline]
    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        self.inner.read_u128(vaddr)
    }

    #[inline]
    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.inner.write_u8(vaddr, val)
    }

    #[inline]
    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.inner.write_u16(vaddr, val)
    }

    #[inline]
    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.inner.write_u32(vaddr, val)
    }

    #[inline]
    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.inner.write_u64(vaddr, val)
    }

    #[inline]
    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.inner.write_u128(vaddr, val)
    }

    #[inline]
    fn atomic_rmw<T, R>(&mut self, addr: u64, f: impl FnOnce(T) -> (T, R)) -> Result<R, Exception>
    where
        T: aero_cpu_core::mem::CpuBusValue,
        Self: Sized,
    {
        self.inner.atomic_rmw(addr, f)
    }

    #[inline]
    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        self.inner.read_bytes(vaddr, dst)
    }

    #[inline]
    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.inner.write_bytes(vaddr, src)
    }

    #[inline]
    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        self.inner.preflight_write_bytes(vaddr, len)
    }

    #[inline]
    fn supports_bulk_copy(&self) -> bool {
        self.a20.enabled() && self.inner.supports_bulk_copy()
    }

    #[inline]
    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        if len == 0 || dst == src {
            return Ok(true);
        }
        if !self.a20.enabled() {
            return Ok(false);
        }
        self.inner.bulk_copy(dst, src, len)
    }

    #[inline]
    fn supports_bulk_set(&self) -> bool {
        self.a20.enabled() && self.inner.supports_bulk_set()
    }

    #[inline]
    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        if repeat == 0 || pattern.is_empty() {
            return Ok(true);
        }
        if !self.a20.enabled() {
            return Ok(false);
        }
        self.inner.bulk_set(dst, pattern, repeat)
    }

    #[inline]
    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        // Reset requests (e.g. port 0xCF9, i8042 reset line, A20 gate reset pulse) are latched by
        // device models during port I/O. Tier-0 batches may otherwise continue executing past the
        // reset-triggering instruction until the next outer-loop boundary.
        //
        // Use an error on instruction fetch to stop execution at the next instruction boundary
        // once a reset is pending. The enclosing run loop converts the latched reset into a
        // `RunExit::ResetRequested` result.
        if self.reset.peek().is_some() {
            return Err(Exception::Unimplemented("reset requested"));
        }
        self.inner.fetch(vaddr, max_len)
    }

    #[inline]
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.inner.io_read(port, size)
    }

    #[inline]
    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.inner.io_write(port, size, val)
    }
}

struct AhciPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl AhciPciConfigDevice {
    fn new() -> Self {
        let mut cfg = aero_devices::pci::profile::SATA_AHCI_ICH9.build_config_space();
        cfg.set_bar_definition(
            aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX,
            PciBarDefinition::Mmio32 {
                size: aero_devices::pci::profile::AHCI_ABAR_SIZE_U32,
                prefetchable: false,
            },
        );
        Self { cfg }
    }
}

impl PciDevice for AhciPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct NvmePciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl NvmePciConfigDevice {
    fn new() -> Self {
        let mut cfg = aero_devices::pci::profile::NVME_CONTROLLER.build_config_space();
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: NvmeController::bar0_len(),
                prefetchable: false,
            },
        );
        Self { cfg }
    }
}

impl PciDevice for NvmePciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct AeroGpuPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl AeroGpuPciConfigDevice {
    fn new() -> Self {
        let cfg = aero_devices::pci::profile::AEROGPU.build_config_space();
        debug_assert_eq!(
            cfg.bar_definition(aero_devices::pci::profile::AEROGPU_BAR0_INDEX),
            Some(PciBarDefinition::Mmio32 {
                size: u32::try_from(aero_devices::pci::profile::AEROGPU_BAR0_SIZE)
                    .expect("AeroGPU BAR0 size should fit in u32"),
                prefetchable: false,
            }),
            "unexpected AeroGPU BAR0 definition"
        );
        debug_assert_eq!(
            cfg.bar_definition(aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX),
            Some(PciBarDefinition::Mmio32 {
                size: u32::try_from(aero_devices::pci::profile::AEROGPU_VRAM_SIZE)
                    .expect("AeroGPU VRAM size should fit in u32"),
                prefetchable: true,
            }),
            "unexpected AeroGPU BAR1 definition"
        );
        Self { cfg }
    }
}

impl PciDevice for AeroGpuPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct E1000PciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl E1000PciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::NIC_E1000_82540EM.build_config_space(),
        }
    }
}

impl PciDevice for E1000PciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct VirtioNetPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl VirtioNetPciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::VIRTIO_NET.build_config_space(),
        }
    }
}

impl PciDevice for VirtioNetPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct VirtioInputKeyboardPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl VirtioInputKeyboardPciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD.build_config_space(),
        }
    }
}

impl PciDevice for VirtioInputKeyboardPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct VirtioInputMousePciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl VirtioInputMousePciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::VIRTIO_INPUT_MOUSE.build_config_space(),
        }
    }
}

impl PciDevice for VirtioInputMousePciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct VirtioBlkPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl VirtioBlkPciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::VIRTIO_BLK.build_config_space(),
        }
    }
}

impl PciDevice for VirtioBlkPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

#[derive(Debug, Default)]
struct NoopVirtioInterruptSink;

impl VirtioInterruptSink for NoopVirtioInterruptSink {
    fn raise_legacy_irq(&mut self) {}

    fn lower_legacy_irq(&mut self) {}

    fn signal_msix(&mut self, _message: MsiMessage) {}
}

/// Virtio interrupt sink that delivers MSI(-X) messages into the machine's platform interrupt
/// controller.
///
/// Note: legacy INTx is *not* driven through this sink in `Machine`; INTx is polled and routed via
/// [`Machine::sync_pci_intx_sources_to_interrupts`]. This keeps INTx level tracking centralized and
/// deterministic while still allowing modern MSI-X delivery.
#[derive(Debug, Clone)]
struct VirtioMsixInterruptSink {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl VirtioMsixInterruptSink {
    fn new(interrupts: Rc<RefCell<PlatformInterrupts>>) -> Self {
        Self { interrupts }
    }
}

impl VirtioInterruptSink for VirtioMsixInterruptSink {
    fn raise_legacy_irq(&mut self) {
        // Legacy INTx delivery is handled via `Machine::sync_pci_intx_sources_to_interrupts` polling.
    }

    fn lower_legacy_irq(&mut self) {
        // Legacy INTx delivery is handled via `Machine::sync_pci_intx_sources_to_interrupts` polling.
    }

    fn signal_msix(&mut self, message: MsiMessage) {
        self.interrupts.borrow_mut().trigger_msi(message);
    }
}

fn sync_virtio_msix_from_platform(dev: &mut VirtioPciDevice, enabled: bool, function_masked: bool) {
    let Some(off) = dev.config_mut().find_capability(PCI_CAP_ID_MSIX) else {
        return;
    };

    // Preserve the read-only table size bits and only synchronize the guest-writable enable/mask
    // bits.
    let ctrl = dev.config_mut().read(u16::from(off) + 0x02, 2) as u16;
    let mut new_ctrl = ctrl & !((1 << 15) | (1 << 14));
    if enabled {
        new_ctrl |= 1 << 15;
    }
    if function_masked {
        new_ctrl |= 1 << 14;
    }
    if new_ctrl != ctrl {
        // Route this through `VirtioPciDevice::config_write` instead of writing directly to the
        // underlying `PciConfigSpace` so we preserve virtio-transport side effects:
        // - INTx gating when MSI-X is toggled
        // - pending MSI-X vector redelivery when Function Mask is cleared
        dev.config_write(u16::from(off) + 0x02, &new_ctrl.to_le_bytes());
    }
}

struct VirtioPciBar0Mmio {
    pci_cfg: SharedPciConfigPorts,
    bdf: PciBdf,
    dev: Rc<RefCell<VirtioPciDevice>>,
}

impl VirtioPciBar0Mmio {
    fn new(pci_cfg: SharedPciConfigPorts, dev: Rc<RefCell<VirtioPciDevice>>, bdf: PciBdf) -> Self {
        Self { pci_cfg, bdf, dev }
    }

    fn sync_pci_command(&mut self) {
        let (command, msix_enabled, msix_masked) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            match pci_cfg.bus_mut().device_config(self.bdf) {
                Some(cfg) => {
                    let msix = cfg.capability::<MsixCapability>();
                    (
                        cfg.command(),
                        msix.is_some_and(|msix| msix.enabled()),
                        msix.is_some_and(|msix| msix.function_masked()),
                    )
                }
                None => (0, false, false),
            }
        };

        let mut dev = self.dev.borrow_mut();
        dev.set_pci_command(command);
        sync_virtio_msix_from_platform(&mut dev, msix_enabled, msix_masked);
    }

    fn all_ones(size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if size >= 8 {
            return u64::MAX;
        }
        (1u64 << (size * 8)) - 1
    }
}

impl PciBarMmioHandler for VirtioPciBar0Mmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.sync_pci_command();
        match size {
            1 | 2 | 4 | 8 => {
                let mut buf = [0u8; 8];
                self.dev.borrow_mut().bar0_read(offset, &mut buf[..size]);
                u64::from_le_bytes(buf)
            }
            _ => Self::all_ones(size),
        }
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return;
        }
        self.sync_pci_command();
        let bytes = value.to_le_bytes();
        self.dev.borrow_mut().bar0_write(offset, &bytes[..size]);
    }
}

/// Guest-memory adapter to allow virtio devices to DMA against the canonical [`SystemMemory`] RAM.
///
/// Virtio queue descriptor addresses are guest-physical. We intentionally DMA only against guest
/// RAM (not ROM/MMIO) and apply A20 masking when disabled so legacy behavior remains consistent.
struct VirtioDmaMemory<'a> {
    a20_enabled: bool,
    bus: &'a mut PlatformMemoryBus,
}

impl<'a> VirtioDmaMemory<'a> {
    fn new(mem: &'a mut SystemMemory) -> Self {
        Self {
            a20_enabled: mem.a20.enabled(),
            bus: &mut mem.bus,
        }
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }
}

impl VirtioGuestMemory for VirtioDmaMemory<'_> {
    fn len(&self) -> u64 {
        self.bus.ram().size()
    }

    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), VirtioGuestMemoryError> {
        if dst.is_empty() {
            return Ok(());
        }
        if self.a20_enabled {
            self.bus.ram().read_into(addr, dst).map_err(|_| {
                VirtioGuestMemoryError::OutOfBounds {
                    addr,
                    len: dst.len(),
                }
            })?;
            return Ok(());
        }

        // A20-disabled slow path: translate each byte so accesses that cross the 1MiB boundary wrap
        // like real hardware.
        for (i, slot) in dst.iter_mut().enumerate() {
            let a = self.translate_a20(addr.wrapping_add(i as u64));
            let mut tmp = [0u8; 1];
            self.bus
                .ram()
                .read_into(a, &mut tmp)
                .map_err(|_| VirtioGuestMemoryError::OutOfBounds { addr: a, len: 1 })?;
            *slot = tmp[0];
        }
        Ok(())
    }

    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), VirtioGuestMemoryError> {
        if src.is_empty() {
            return Ok(());
        }
        if self.a20_enabled {
            self.bus.ram_mut().write_from(addr, src).map_err(|_| {
                VirtioGuestMemoryError::OutOfBounds {
                    addr,
                    len: src.len(),
                }
            })?;
            return Ok(());
        }

        // A20-disabled slow path: translate each byte individually.
        for (i, byte) in src.iter().copied().enumerate() {
            let a = self.translate_a20(addr.wrapping_add(i as u64));
            self.bus
                .ram_mut()
                .write_from(a, &[byte])
                .map_err(|_| VirtioGuestMemoryError::OutOfBounds { addr: a, len: 1 })?;
        }
        Ok(())
    }
}
struct Piix3IsaPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl Piix3IsaPciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::ISA_PIIX3.build_config_space(),
        }
    }
}

impl PciDevice for Piix3IsaPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}
struct IdePciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl IdePciConfigDevice {
    fn new() -> Self {
        // Preserve legacy compatibility port assignments (0x1F0/0x170, etc.) so software that
        // expects a "PC-like" IDE controller sees deterministic defaults.
        //
        // See `docs/05-storage-topology-win7.md`.
        let mut cfg = aero_devices::pci::profile::IDE_PIIX3.build_config_space();
        cfg.set_bar_base(0, u64::from(PRIMARY_PORTS.cmd_base));
        cfg.set_bar_base(1, 0x3F4); // alt-status/dev-ctl at +2 => 0x3F6
        cfg.set_bar_base(2, u64::from(SECONDARY_PORTS.cmd_base));
        cfg.set_bar_base(3, 0x374); // alt-status/dev-ctl at +2 => 0x376
        cfg.set_bar_base(4, u64::from(Piix3IdePciDevice::DEFAULT_BUS_MASTER_BASE));
        Self { cfg }
    }
}

impl PciDevice for IdePciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct UhciPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl UhciPciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::USB_UHCI_PIIX3.build_config_space(),
        }
    }
}

impl PciDevice for UhciPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

struct EhciPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl EhciPciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::USB_EHCI_ICH9.build_config_space(),
        }
    }
}

impl PciDevice for EhciPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}
struct XhciPciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl XhciPciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::USB_XHCI_QEMU.build_config_space(),
        }
    }
}

impl PciDevice for XhciPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}
// -----------------------------------------------------------------------------
// VGA / SVGA integration (legacy VGA + Bochs VBE_DISPI)
// -----------------------------------------------------------------------------

// NOTE: PCI BDF allocation is currently spread across the repo (e.g. AHCI uses `00:02.0` via
// `aero_devices::pci::profile::SATA_AHCI_ICH9`).
//
// IMPORTANT: `00:07.0` is reserved for the canonical AeroGPU PCI identity contract
// (`VID:DID = A3A0:0001`, `PCI\VEN_A3A0&DEV_0001`). See `docs/abi/aerogpu-pci-identity.md`.
//
// The VGA/VBE device model used for boot display (`aero_gpu_vga`) is *not* AeroGPU and must not
// occupy that BDF.
//
// When `MachineConfig::enable_vga` is enabled (and `enable_aerogpu` is not), the legacy VGA/VBE
// linear framebuffer (LFB) is routed inside the ACPI-reported PCI MMIO window without occupying a
// dedicated PCI function (see `MachineConfig::vga_lfb_base`).
// -----------------------------------------------------------------------------
// AeroGPU legacy VGA compatibility (VRAM backing store + aliasing)
// -----------------------------------------------------------------------------

/// Size of the legacy VGA memory window (`0xA0000..0xC0000`) in bytes.
///
/// This is aliased to `VRAM[0..LEGACY_VGA_WINDOW_SIZE]` when AeroGPU is enabled.
const LEGACY_VGA_WINDOW_SIZE: usize = aero_gpu_vga::VGA_LEGACY_MEM_LEN as usize;

/// Offset within VRAM where the VBE linear framebuffer (LFB) begins.
///
/// This keeps the LFB aligned to 64KiB and leaves the first 256KiB reserved for legacy VGA planar
/// storage (4 × 64KiB planes), matching `aero_gpu_vga`'s VRAM layout.
#[allow(dead_code)]
pub const VBE_LFB_OFFSET: usize =
    aero_protocol::aerogpu::aerogpu_pci::AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES as usize;
const _: () = {
    assert!(
        VBE_LFB_OFFSET == aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET,
        "AeroGPU BAR1 VBE LFB offset must match the VGA/VBE VRAM layout"
    );
};

// Allocated VRAM backing store size.
//
// The canonical PCI profile exposes a 64MiB prefetchable BAR for AeroGPU VRAM (large enough for a
// 4K scanout). In wasm32 builds, the runtime heap is deliberately capped to the runtime-reserved
// region (`crates/aero-wasm/src/runtime_alloc.rs`), which can make eagerly allocating the full
// 64MiB VRAM backing store (and then taking snapshots that copy portions of it) exceed the
// available heap during wasm-bindgen tests.
//
// The current AeroGPU legacy VGA/VBE compatibility path only requires the low portion of VRAM
// (legacy window + VBE LFB). Allocate a smaller backing store on wasm32 to keep the canonical
// browser `Machine::new` usable in constrained test environments.
#[cfg(target_arch = "wasm32")]
const AEROGPU_VRAM_ALLOC_SIZE: usize = 32 * 1024 * 1024;
#[cfg(not(target_arch = "wasm32"))]
const AEROGPU_VRAM_ALLOC_SIZE: usize = aero_devices::pci::profile::AEROGPU_VRAM_SIZE as usize;

/// Minimal AeroGPU runtime state required for VRAM-backed legacy VGA/VBE compatibility.
///
/// Note: The versioned BAR0 MMIO register block (ring transport + scanout + vblank pacing) is
/// implemented separately in [`AeroGpuMmioDevice`] (`crates/aero-machine/src/aerogpu.rs`).
struct AeroGpuDevice {
    /// Dedicated VRAM backing store exposed via AeroGPU BAR1.
    vram: Vec<u8>,
    /// Number of BAR1 MMIO reads routed through the PCI MMIO window.
    ///
    /// This is a debug/testing hook used to assert scanout presentation uses the direct VRAM
    /// fast-path instead of going through the MMIO router for every pixel.
    vram_mmio_reads: Cell<u64>,

    /// Whether a VBE mode is currently active.
    ///
    /// When set, the `0xA0000..0xAFFFF` banked window is mapped to the VBE framebuffer region
    /// starting at `VBE_LFB_OFFSET`, rather than the legacy VGA planar/text backing region.
    vbe_mode_active: bool,
    /// Current VBE bank index (units of 64KiB windows).
    vbe_bank: u16,

    // ---------------------------------------------------------------------
    // Bochs VBE_DISPI register file (`0x01CE/0x01CF`) (minimal)
    // ---------------------------------------------------------------------
    vbe_dispi_index: u16,
    vbe_dispi_xres: u16,
    vbe_dispi_yres: u16,
    vbe_dispi_bpp: u16,
    vbe_dispi_enable: u16,
    vbe_dispi_virt_width: u16,
    vbe_dispi_virt_height: u16,
    vbe_dispi_x_offset: u16,
    vbe_dispi_y_offset: u16,
    /// Whether the guest has written to VBE_DISPI registers directly.
    ///
    /// When false, BIOS INT 10h VBE mode sets mirror into these registers so guests that probe the
    /// Bochs VBE_DISPI interface observe coherent state.
    vbe_dispi_guest_owned: bool,

    // ---------------------------------------------------------------------
    // Minimal VGA port state (permissive)
    // ---------------------------------------------------------------------
    misc_output: u8,

    seq_index: u8,
    seq_regs: [u8; 256],

    gc_index: u8,
    gc_regs: [u8; 256],

    crtc_index: u8,
    crtc_regs: [u8; 256],

    attr_index: u8,
    attr_regs: [u8; 256],
    attr_flip_flop: bool,

    // ---------------------------------------------------------------------
    // VGA DAC / palette (0x3C6..=0x3C9)
    // ---------------------------------------------------------------------
    pel_mask: u8,
    dac_write_index: u8,
    dac_write_subindex: u8,
    dac_write_latch: [u8; 3],
    dac_read_index: u8,
    dac_read_subindex: u8,
    /// Stored as VGA-native 6-bit components (0..=63).
    dac_palette: [[u8; 3]; 256],
}

impl AeroGpuDevice {
    fn vbe_dispi_enabled(&self) -> bool {
        (self.vbe_dispi_enable & 0x0001) != 0
    }

    fn vbe_active(&self) -> bool {
        self.vbe_mode_active || self.vbe_dispi_enabled()
    }

    fn default_dac_palette() -> [[u8; 3]; 256] {
        let mut out = [[0u8; 3]; 256];
        // Map an 8-bit (0..=255) channel value to a VGA 6-bit DAC value (0..=63) with rounding.
        let to_6bit = |v: u8| -> u8 { ((u16::from(v) * 63 + 127) / 255) as u8 };

        // Standard EGA 16-color palette encoded as VGA DAC 6-bit components.
        //
        // This matches the VGA device model's default palette for indices 0..15.
        const EGA_6BIT: [[u8; 3]; 16] = [
            [0, 0, 0],    // 0 black
            [0, 0, 42],   // 1 blue
            [0, 42, 0],   // 2 green
            [0, 42, 42],  // 3 cyan
            [42, 0, 0],   // 4 red
            [42, 0, 42],  // 5 magenta
            [42, 21, 0],  // 6 brown
            [42, 42, 42], // 7 light grey
            [21, 21, 21], // 8 dark grey
            [21, 21, 63], // 9 bright blue
            [21, 63, 21], // 10 bright green
            [21, 63, 63], // 11 bright cyan
            [63, 21, 21], // 12 bright red
            [63, 21, 63], // 13 bright magenta
            [63, 63, 21], // 14 yellow
            [63, 63, 63], // 15 white
        ];
        out[..16].copy_from_slice(&EGA_6BIT);

        // 6x6x6 color cube (indices 16..231), similar to the classic VGA palette.
        let mut idx = 16usize;
        for r in 0..6u8 {
            for g in 0..6u8 {
                for b in 0..6u8 {
                    let scale8 = |v: u8| -> u8 { ((u16::from(v) * 255) / 5) as u8 };
                    out[idx] = [to_6bit(scale8(r)), to_6bit(scale8(g)), to_6bit(scale8(b))];
                    idx += 1;
                }
            }
        }

        // Grayscale ramp (232..255).
        for i in 0..24u8 {
            let v8 = ((u16::from(i) * 255) / 23) as u8;
            let v6 = to_6bit(v8);
            out[232 + i as usize] = [v6, v6, v6];
        }
        out
    }

    fn default_attr_regs() -> [u8; 256] {
        // VGA text mode defaults (matching `aero_gpu_vga::VgaDevice::set_text_mode_80x25`):
        // - Mode Control: bit0=0 => text; bit2=line graphics enable.
        // - Color Plane Enable: enable all 4 planes so color indices are not masked to 0.
        // - Color Select: select palette page 0.
        let mut regs = [0u8; 256];
        regs[0x10] = 1 << 2;
        regs[0x12] = 0x0F;
        regs[0x14] = 0x00;
        // Identity palette mapping for indices 0..15.
        for i in 0..16u8 {
            regs[i as usize] = i;
        }
        regs
    }

    fn new() -> Self {
        Self {
            vram: vec![0u8; AEROGPU_VRAM_ALLOC_SIZE],
            vram_mmio_reads: Cell::new(0),
            vbe_mode_active: false,
            vbe_bank: 0,
            vbe_dispi_index: 0,
            vbe_dispi_xres: 0,
            vbe_dispi_yres: 0,
            vbe_dispi_bpp: 0,
            vbe_dispi_enable: 0,
            vbe_dispi_virt_width: 0,
            vbe_dispi_virt_height: 0,
            vbe_dispi_x_offset: 0,
            vbe_dispi_y_offset: 0,
            vbe_dispi_guest_owned: false,
            misc_output: 0,
            seq_index: 0,
            seq_regs: [0; 256],
            gc_index: 0,
            gc_regs: [0; 256],
            crtc_index: 0,
            crtc_regs: [0; 256],
            attr_index: 0,
            attr_regs: Self::default_attr_regs(),
            attr_flip_flop: false,
            pel_mask: 0xFF,
            dac_write_index: 0,
            dac_write_subindex: 0,
            dac_write_latch: [0; 3],
            dac_read_index: 0,
            dac_read_subindex: 0,
            dac_palette: Self::default_dac_palette(),
        }
    }

    fn reset(&mut self) {
        self.vram.fill(0);
        self.vram_mmio_reads.set(0);
        self.vbe_mode_active = false;
        self.vbe_bank = 0;
        self.vbe_dispi_index = 0;
        self.vbe_dispi_xres = 0;
        self.vbe_dispi_yres = 0;
        self.vbe_dispi_bpp = 0;
        self.vbe_dispi_enable = 0;
        self.vbe_dispi_virt_width = 0;
        self.vbe_dispi_virt_height = 0;
        self.vbe_dispi_x_offset = 0;
        self.vbe_dispi_y_offset = 0;
        self.vbe_dispi_guest_owned = false;
        self.misc_output = 0;
        self.seq_index = 0;
        self.seq_regs.fill(0);
        self.gc_index = 0;
        self.gc_regs.fill(0);
        self.crtc_index = 0;
        self.crtc_regs.fill(0);
        self.attr_index = 0;
        self.attr_regs = Self::default_attr_regs();
        self.attr_flip_flop = false;
        self.pel_mask = 0xFF;
        self.dac_write_index = 0;
        self.dac_write_subindex = 0;
        self.dac_write_latch = [0; 3];
        self.dac_read_index = 0;
        self.dac_read_subindex = 0;
        self.dac_palette = Self::default_dac_palette();
    }

    fn vbe_dispi_read_reg(&self, index: u16) -> u16 {
        match index {
            0x0000 => 0xB0C5, // Bochs VBE_DISPI ID
            0x0001 => self.vbe_dispi_xres,
            0x0002 => self.vbe_dispi_yres,
            0x0003 => self.vbe_dispi_bpp,
            0x0004 => self.vbe_dispi_enable,
            0x0005 => self.vbe_bank,
            0x0006 => self.vbe_dispi_virt_width,
            0x0007 => self.vbe_dispi_virt_height,
            0x0008 => self.vbe_dispi_x_offset,
            0x0009 => self.vbe_dispi_y_offset,
            0x000A => {
                let fb_base = VBE_LFB_OFFSET;
                u16::try_from(self.vram.len().saturating_sub(fb_base) / (64 * 1024))
                    .unwrap_or(u16::MAX)
            }
            _ => 0,
        }
    }

    fn vbe_dispi_write_reg(&mut self, index: u16, value: u16) {
        self.vbe_dispi_guest_owned = true;
        match index {
            0x0001 => {
                self.vbe_dispi_xres = value;
            }
            0x0002 => {
                self.vbe_dispi_yres = value;
            }
            0x0003 => {
                self.vbe_dispi_bpp = value;
            }
            0x0004 => {
                self.vbe_dispi_enable = value;
            }
            0x0005 => {
                self.vbe_bank = value;
            }
            0x0006 => {
                self.vbe_dispi_virt_width = value;
            }
            0x0007 => {
                self.vbe_dispi_virt_height = value;
            }
            0x0008 => {
                self.vbe_dispi_x_offset = value;
            }
            0x0009 => {
                self.vbe_dispi_y_offset = value;
            }
            _ => {}
        }
    }

    fn write_dac_data(&mut self, value: u8) {
        let idx = self.dac_write_index as usize;
        let component = (self.dac_write_subindex as usize) % 3;
        self.dac_write_latch[component] = value;
        self.dac_write_subindex = (self.dac_write_subindex + 1) % 3;
        if self.dac_write_subindex != 0 {
            return;
        }

        // Real VGA hardware uses a 6-bit DAC (0..=63), but a lot of software writes 8-bit values.
        // Be permissive by detecting 8-bit mode per RGB triplet.
        let is_8bit = self.dac_write_latch.iter().any(|&v| v > 0x3F);
        let to_6bit = |v: u8| -> u8 {
            if is_8bit {
                v >> 2
            } else {
                v & 0x3F
            }
        };

        let r = to_6bit(self.dac_write_latch[0]);
        let g = to_6bit(self.dac_write_latch[1]);
        let b = to_6bit(self.dac_write_latch[2]);
        self.dac_palette[idx] = [r, g, b];
        self.dac_write_index = self.dac_write_index.wrapping_add(1);
    }

    fn read_dac_data(&mut self) -> u8 {
        let idx = self.dac_read_index as usize;
        let component = (self.dac_read_subindex as usize).min(2);
        let out = self.dac_palette[idx][component];
        self.dac_read_subindex = (self.dac_read_subindex + 1) % 3;
        if self.dac_read_subindex == 0 {
            self.dac_read_index = self.dac_read_index.wrapping_add(1);
        }
        out
    }

    fn read_linear(buf: &[u8], offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = match size {
            1 | 2 | 4 | 8 => size,
            _ => size.clamp(1, 8),
        };

        let off = usize::try_from(offset).unwrap_or(usize::MAX);
        if off >= buf.len() {
            return 0;
        }

        let end = off.saturating_add(size).min(buf.len());
        let len = end - off;

        let mut tmp = [0u8; 8];
        tmp[..len].copy_from_slice(&buf[off..end]);
        u64::from_le_bytes(tmp)
    }

    fn write_linear(buf: &mut [u8], offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = match size {
            1 | 2 | 4 | 8 => size,
            _ => size.clamp(1, 8),
        };

        let off = usize::try_from(offset).unwrap_or(usize::MAX);
        if off >= buf.len() {
            return;
        }

        let end = off.saturating_add(size).min(buf.len());
        let len = end - off;
        let bytes = value.to_le_bytes();
        buf[off..end].copy_from_slice(&bytes[..len]);
    }

    fn vram_read(&self, offset: u64, size: usize) -> u64 {
        self.vram_mmio_reads
            .set(self.vram_mmio_reads.get().wrapping_add(1));
        Self::read_linear(&self.vram, offset, size)
    }

    fn vram_write(&mut self, offset: u64, size: usize, value: u64) {
        Self::write_linear(&mut self.vram, offset, size, value);
    }

    fn vram_mmio_read_count(&self) -> u64 {
        self.vram_mmio_reads.get()
    }

    fn legacy_vga_read(&self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = match size {
            1 | 2 | 4 | 8 => size,
            _ => size.clamp(1, 8),
        };

        let mut out = 0u64;
        for i in 0..size {
            // Guard against u64 wrap-around on malformed offsets.
            let Some(off) = offset.checked_add(i as u64) else {
                continue;
            };
            let byte = self.legacy_vga_read_u8(off);
            out |= (byte as u64) << (i * 8);
        }
        out
    }

    fn legacy_vga_write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = match size {
            1 | 2 | 4 | 8 => size,
            _ => size.clamp(1, 8),
        };

        for i in 0..size {
            // Guard against u64 wrap-around on malformed offsets.
            let Some(off) = offset.checked_add(i as u64) else {
                continue;
            };
            let byte = ((value >> (i * 8)) & 0xFF) as u8;
            self.legacy_vga_write_u8(off, byte);
        }
    }

    fn legacy_vga_read_u8(&self, window_off: u64) -> u8 {
        let off = usize::try_from(window_off).unwrap_or(usize::MAX);

        // VBE banked window at 0xA0000 (64KiB). When VBE is active, map it into the VBE framebuffer
        // region, not the legacy VGA alias region.
        if self.vbe_active() && off < 64 * 1024 {
            let bank_base = usize::from(self.vbe_bank) * 64 * 1024;
            let vbe_off = VBE_LFB_OFFSET
                .checked_add(bank_base)
                .and_then(|base| base.checked_add(off))
                .unwrap_or(usize::MAX);
            return self.vram.get(vbe_off).copied().unwrap_or(0);
        }

        // Default legacy VGA alias: `0xA0000..0xC0000` <-> `VRAM[0..LEGACY_VGA_WINDOW_SIZE]`.
        if off < LEGACY_VGA_WINDOW_SIZE {
            return self.vram.get(off).copied().unwrap_or(0);
        }

        0
    }

    fn legacy_vga_write_u8(&mut self, window_off: u64, value: u8) {
        let off = usize::try_from(window_off).unwrap_or(usize::MAX);

        if self.vbe_active() && off < 64 * 1024 {
            let bank_base = usize::from(self.vbe_bank) * 64 * 1024;
            let vbe_off = VBE_LFB_OFFSET
                .checked_add(bank_base)
                .and_then(|base| base.checked_add(off))
                .unwrap_or(usize::MAX);
            if let Some(slot) = self.vram.get_mut(vbe_off) {
                *slot = value;
            }
            return;
        }

        if off < LEGACY_VGA_WINDOW_SIZE {
            if let Some(slot) = self.vram.get_mut(off) {
                *slot = value;
            }
        }
    }

    fn vga_port_read_u8(&mut self, port: u16) -> u8 {
        match port {
            // Attribute controller: reads happen via 0x3C1.
            0x03C1 => self.attr_regs[usize::from(self.attr_index)],
            // VGA DAC.
            0x03C6 => self.pel_mask,
            0x03C7 => self.dac_read_index,
            0x03C8 => self.dac_write_index,
            0x03C9 => self.read_dac_data(),
            // Misc Output readback (0x3CC).
            //
            // Real hardware reads Misc Output at 0x3CC (and 0x3C2 is Input Status 0). Accept reads
            // from either port for maximum guest compatibility.
            0x03CC | 0x03C2 => self.misc_output,
            // Sequencer.
            0x03C4 => self.seq_index,
            0x03C5 => self.seq_regs[usize::from(self.seq_index)],
            // Graphics controller.
            0x03CE => self.gc_index,
            0x03CF => self.gc_regs[usize::from(self.gc_index)],
            // CRTC (color and mono aliases).
            0x03B4 | 0x03D4 => self.crtc_index,
            0x03B5 | 0x03D5 => self.crtc_regs[usize::from(self.crtc_index)],
            // Input Status 1: reading resets the attribute controller flip-flop.
            0x03DA | 0x03BA => {
                self.attr_flip_flop = false;
                0xFF
            }
            _ => 0xFF,
        }
    }

    fn vga_port_write_u8(&mut self, port: u16, value: u8) {
        match port {
            // Attribute controller (index/data flip-flop).
            0x03C0 => {
                if !self.attr_flip_flop {
                    // VGA Attribute Controller index uses bits 0..4; bit 5 controls
                    // "palette access"/display enable on real hardware. Mask to 0x1F for guest
                    // compatibility (many guests write index | 0x20).
                    self.attr_index = value & 0x1F;
                    self.attr_flip_flop = true;
                } else {
                    self.attr_regs[usize::from(self.attr_index)] = value;
                    self.attr_flip_flop = false;
                }
            }
            // Attribute controller data writes via 0x3C1 are ignored for now.
            0x03C1 => {}

            // Misc Output.
            0x03C2 => self.misc_output = value,

            // VGA DAC.
            0x03C6 => self.pel_mask = value,
            0x03C7 => {
                self.dac_read_index = value;
                self.dac_read_subindex = 0;
            }
            0x03C8 => {
                self.dac_write_index = value;
                self.dac_write_subindex = 0;
            }
            0x03C9 => self.write_dac_data(value),

            // Sequencer.
            0x03C4 => self.seq_index = value,
            0x03C5 => self.seq_regs[usize::from(self.seq_index)] = value,

            // Graphics controller.
            0x03CE => self.gc_index = value,
            0x03CF => self.gc_regs[usize::from(self.gc_index)] = value,

            // CRTC (color and mono aliases).
            0x03B4 | 0x03D4 => self.crtc_index = value,
            0x03B5 | 0x03D5 => self.crtc_regs[usize::from(self.crtc_index)] = value,

            _ => {}
        }
    }
}

impl IoSnapshot for AeroGpuDevice {
    const DEVICE_ID: [u8; 4] = *b"AGPU";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_VRAM: u16 = 100;
        const TAG_VBE_MODE_ACTIVE: u16 = 130;
        const TAG_VBE_BANK: u16 = 131;
        const TAG_VBE_DISPI_INDEX: u16 = 132;
        const TAG_VBE_DISPI_XRES: u16 = 133;
        const TAG_VBE_DISPI_YRES: u16 = 134;
        const TAG_VBE_DISPI_BPP: u16 = 135;
        const TAG_VBE_DISPI_ENABLE: u16 = 136;
        const TAG_VBE_DISPI_VIRT_WIDTH: u16 = 137;
        const TAG_VBE_DISPI_VIRT_HEIGHT: u16 = 138;
        const TAG_VBE_DISPI_X_OFFSET: u16 = 139;
        const TAG_VBE_DISPI_Y_OFFSET: u16 = 140;
        const TAG_VBE_DISPI_GUEST_OWNED: u16 = 141;

        const TAG_VGA_MISC_OUTPUT: u16 = 101;
        const TAG_VGA_SEQ_INDEX: u16 = 102;
        const TAG_VGA_SEQ_REGS: u16 = 103;
        const TAG_VGA_GC_INDEX: u16 = 104;
        const TAG_VGA_GC_REGS: u16 = 105;
        const TAG_VGA_CRTC_INDEX: u16 = 106;
        const TAG_VGA_CRTC_REGS: u16 = 107;
        const TAG_VGA_ATTR_INDEX: u16 = 108;
        const TAG_VGA_ATTR_REGS: u16 = 109;
        const TAG_VGA_ATTR_FLIP_FLOP: u16 = 110;

        const TAG_VGA_PEL_MASK: u16 = 120;
        const TAG_VGA_DAC_WRITE_INDEX: u16 = 121;
        const TAG_VGA_DAC_WRITE_SUBINDEX: u16 = 122;
        const TAG_VGA_DAC_WRITE_LATCH: u16 = 123;
        const TAG_VGA_DAC_READ_INDEX: u16 = 124;
        const TAG_VGA_DAC_READ_SUBINDEX: u16 = 125;
        const TAG_VGA_DAC_PALETTE: u16 = 126;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        // Note: AeroGPU exposes a large VRAM aperture (64MiB) via BAR1, but `aero-snapshot` bounds
        // individual device entries to 64MiB. Persist only the legacy VGA compatibility window
        // (`0xA0000..0xC0000` alias) so full-machine snapshots remain representable.
        let vram_len = LEGACY_VGA_WINDOW_SIZE.min(self.vram.len());
        w.field_bytes(TAG_VRAM, self.vram[..vram_len].to_vec());

        w.field_bool(TAG_VBE_MODE_ACTIVE, self.vbe_mode_active);
        w.field_u32(TAG_VBE_BANK, u32::from(self.vbe_bank));
        w.field_u16(TAG_VBE_DISPI_INDEX, self.vbe_dispi_index);
        w.field_u16(TAG_VBE_DISPI_XRES, self.vbe_dispi_xres);
        w.field_u16(TAG_VBE_DISPI_YRES, self.vbe_dispi_yres);
        w.field_u16(TAG_VBE_DISPI_BPP, self.vbe_dispi_bpp);
        w.field_u16(TAG_VBE_DISPI_ENABLE, self.vbe_dispi_enable);
        w.field_u16(TAG_VBE_DISPI_VIRT_WIDTH, self.vbe_dispi_virt_width);
        w.field_u16(TAG_VBE_DISPI_VIRT_HEIGHT, self.vbe_dispi_virt_height);
        w.field_u16(TAG_VBE_DISPI_X_OFFSET, self.vbe_dispi_x_offset);
        w.field_u16(TAG_VBE_DISPI_Y_OFFSET, self.vbe_dispi_y_offset);
        w.field_bool(TAG_VBE_DISPI_GUEST_OWNED, self.vbe_dispi_guest_owned);

        w.field_u8(TAG_VGA_MISC_OUTPUT, self.misc_output);
        w.field_u8(TAG_VGA_SEQ_INDEX, self.seq_index);
        w.field_bytes(TAG_VGA_SEQ_REGS, self.seq_regs.to_vec());
        w.field_u8(TAG_VGA_GC_INDEX, self.gc_index);
        w.field_bytes(TAG_VGA_GC_REGS, self.gc_regs.to_vec());
        w.field_u8(TAG_VGA_CRTC_INDEX, self.crtc_index);
        w.field_bytes(TAG_VGA_CRTC_REGS, self.crtc_regs.to_vec());
        w.field_u8(TAG_VGA_ATTR_INDEX, self.attr_index);
        w.field_bytes(TAG_VGA_ATTR_REGS, self.attr_regs.to_vec());
        w.field_bool(TAG_VGA_ATTR_FLIP_FLOP, self.attr_flip_flop);

        w.field_u8(TAG_VGA_PEL_MASK, self.pel_mask);
        w.field_u8(TAG_VGA_DAC_WRITE_INDEX, self.dac_write_index);
        w.field_u8(TAG_VGA_DAC_WRITE_SUBINDEX, self.dac_write_subindex);
        w.field_bytes(TAG_VGA_DAC_WRITE_LATCH, self.dac_write_latch.to_vec());
        w.field_u8(TAG_VGA_DAC_READ_INDEX, self.dac_read_index);
        w.field_u8(TAG_VGA_DAC_READ_SUBINDEX, self.dac_read_subindex);

        let mut palette = Vec::with_capacity(256 * 3);
        for rgb in &self.dac_palette {
            palette.extend_from_slice(rgb);
        }
        w.field_bytes(TAG_VGA_DAC_PALETTE, palette);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> IoSnapshotResult<()> {
        const TAG_VRAM: u16 = 100;
        const TAG_VBE_MODE_ACTIVE: u16 = 130;
        const TAG_VBE_BANK: u16 = 131;
        const TAG_VBE_DISPI_INDEX: u16 = 132;
        const TAG_VBE_DISPI_XRES: u16 = 133;
        const TAG_VBE_DISPI_YRES: u16 = 134;
        const TAG_VBE_DISPI_BPP: u16 = 135;
        const TAG_VBE_DISPI_ENABLE: u16 = 136;
        const TAG_VBE_DISPI_VIRT_WIDTH: u16 = 137;
        const TAG_VBE_DISPI_VIRT_HEIGHT: u16 = 138;
        const TAG_VBE_DISPI_X_OFFSET: u16 = 139;
        const TAG_VBE_DISPI_Y_OFFSET: u16 = 140;
        const TAG_VBE_DISPI_GUEST_OWNED: u16 = 141;

        const TAG_VGA_MISC_OUTPUT: u16 = 101;
        const TAG_VGA_SEQ_INDEX: u16 = 102;
        const TAG_VGA_SEQ_REGS: u16 = 103;
        const TAG_VGA_GC_INDEX: u16 = 104;
        const TAG_VGA_GC_REGS: u16 = 105;
        const TAG_VGA_CRTC_INDEX: u16 = 106;
        const TAG_VGA_CRTC_REGS: u16 = 107;
        const TAG_VGA_ATTR_INDEX: u16 = 108;
        const TAG_VGA_ATTR_REGS: u16 = 109;
        const TAG_VGA_ATTR_FLIP_FLOP: u16 = 110;

        const TAG_VGA_PEL_MASK: u16 = 120;
        const TAG_VGA_DAC_WRITE_INDEX: u16 = 121;
        const TAG_VGA_DAC_WRITE_SUBINDEX: u16 = 122;
        const TAG_VGA_DAC_WRITE_LATCH: u16 = 123;
        const TAG_VGA_DAC_READ_INDEX: u16 = 124;
        const TAG_VGA_DAC_READ_SUBINDEX: u16 = 125;
        const TAG_VGA_DAC_PALETTE: u16 = 126;

        let r = IoSnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        self.reset();

        if let Some(vram) = r.bytes(TAG_VRAM) {
            let cap = LEGACY_VGA_WINDOW_SIZE.min(self.vram.len());
            let len = vram.len().min(cap);
            self.vram[..len].copy_from_slice(&vram[..len]);
        }

        self.vbe_mode_active = r.bool(TAG_VBE_MODE_ACTIVE)?.unwrap_or(false);
        self.vbe_bank = r.u32(TAG_VBE_BANK)?.unwrap_or(0).min(u32::from(u16::MAX)) as u16;
        self.vbe_dispi_index = r.u16(TAG_VBE_DISPI_INDEX)?.unwrap_or(0);
        self.vbe_dispi_xres = r.u16(TAG_VBE_DISPI_XRES)?.unwrap_or(0);
        self.vbe_dispi_yres = r.u16(TAG_VBE_DISPI_YRES)?.unwrap_or(0);
        self.vbe_dispi_bpp = r.u16(TAG_VBE_DISPI_BPP)?.unwrap_or(0);
        self.vbe_dispi_enable = r.u16(TAG_VBE_DISPI_ENABLE)?.unwrap_or(0);
        self.vbe_dispi_virt_width = r.u16(TAG_VBE_DISPI_VIRT_WIDTH)?.unwrap_or(0);
        self.vbe_dispi_virt_height = r.u16(TAG_VBE_DISPI_VIRT_HEIGHT)?.unwrap_or(0);
        self.vbe_dispi_x_offset = r.u16(TAG_VBE_DISPI_X_OFFSET)?.unwrap_or(0);
        self.vbe_dispi_y_offset = r.u16(TAG_VBE_DISPI_Y_OFFSET)?.unwrap_or(0);
        self.vbe_dispi_guest_owned = r.bool(TAG_VBE_DISPI_GUEST_OWNED)?.unwrap_or(false);

        self.misc_output = r.u8(TAG_VGA_MISC_OUTPUT)?.unwrap_or(0);
        self.seq_index = r.u8(TAG_VGA_SEQ_INDEX)?.unwrap_or(0);
        if let Some(seq_regs) = r.bytes(TAG_VGA_SEQ_REGS) {
            let len = seq_regs.len().min(self.seq_regs.len());
            self.seq_regs[..len].copy_from_slice(&seq_regs[..len]);
        }
        self.gc_index = r.u8(TAG_VGA_GC_INDEX)?.unwrap_or(0);
        if let Some(gc_regs) = r.bytes(TAG_VGA_GC_REGS) {
            let len = gc_regs.len().min(self.gc_regs.len());
            self.gc_regs[..len].copy_from_slice(&gc_regs[..len]);
        }
        self.crtc_index = r.u8(TAG_VGA_CRTC_INDEX)?.unwrap_or(0);
        if let Some(crtc_regs) = r.bytes(TAG_VGA_CRTC_REGS) {
            let len = crtc_regs.len().min(self.crtc_regs.len());
            self.crtc_regs[..len].copy_from_slice(&crtc_regs[..len]);
        }
        self.attr_index = r.u8(TAG_VGA_ATTR_INDEX)?.unwrap_or(0);
        if let Some(attr_regs) = r.bytes(TAG_VGA_ATTR_REGS) {
            let len = attr_regs.len().min(self.attr_regs.len());
            self.attr_regs[..len].copy_from_slice(&attr_regs[..len]);
        }
        self.attr_flip_flop = r.bool(TAG_VGA_ATTR_FLIP_FLOP)?.unwrap_or(false);

        self.pel_mask = r.u8(TAG_VGA_PEL_MASK)?.unwrap_or(0xFF);
        self.dac_write_index = r.u8(TAG_VGA_DAC_WRITE_INDEX)?.unwrap_or(0);
        self.dac_write_subindex = r.u8(TAG_VGA_DAC_WRITE_SUBINDEX)?.unwrap_or(0);
        if let Some(latch) = r.bytes(TAG_VGA_DAC_WRITE_LATCH) {
            let len = latch.len().min(self.dac_write_latch.len());
            self.dac_write_latch[..len].copy_from_slice(&latch[..len]);
        }
        self.dac_read_index = r.u8(TAG_VGA_DAC_READ_INDEX)?.unwrap_or(0);
        self.dac_read_subindex = r.u8(TAG_VGA_DAC_READ_SUBINDEX)?.unwrap_or(0);

        if let Some(palette) = r.bytes(TAG_VGA_DAC_PALETTE) {
            for (i, chunk) in palette.chunks(3).enumerate().take(self.dac_palette.len()) {
                let mut rgb = [0u8; 3];
                let len = chunk.len().min(3);
                rgb[..len].copy_from_slice(&chunk[..len]);
                self.dac_palette[i] = rgb;
            }
        }

        Ok(())
    }
}

struct AeroGpuBar1Mmio {
    dev: Rc<RefCell<AeroGpuDevice>>,
}

impl PciBarMmioHandler for AeroGpuBar1Mmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.dev.borrow().vram_read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.dev.borrow_mut().vram_write(offset, size, value);
    }
}

struct AeroGpuLegacyVgaMmio {
    dev: Rc<RefCell<AeroGpuDevice>>,
}

impl MmioHandler for AeroGpuLegacyVgaMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.dev.borrow().legacy_vga_read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.dev.borrow_mut().legacy_vga_write(offset, size, value);
    }
}

struct AeroGpuVgaPortWindow {
    dev: Rc<RefCell<AeroGpuDevice>>,
    clock: ManualClock,
}

impl AeroGpuVgaPortWindow {
    // Model VGA Input Status 1 vblank bit (bit 3) as a deterministic 60Hz pulse based on the
    // machine's deterministic `ManualClock`.
    //
    // This avoids real-mode software hanging in retrace polling loops like:
    //   while (inb(0x3DA) & 0x08) {}
    //   while (!(inb(0x3DA) & 0x08)) {}
    const VBLANK_PERIOD_NS: u64 = 16_666_667;
    const VBLANK_PULSE_NS: u64 = Self::VBLANK_PERIOD_NS / 20;

    fn input_status1_value(&self) -> u8 {
        let now_ns = aero_interrupts::clock::Clock::now_ns(&self.clock);
        let pos = now_ns % Self::VBLANK_PERIOD_NS;
        let in_vblank = pos < Self::VBLANK_PULSE_NS;
        let v = if in_vblank { 0x08 } else { 0x00 };
        // Bit 3: vertical retrace. Bit 0: display enable (rough approximation).
        v | (v >> 3)
    }
}

impl aero_platform::io::PortIoDevice for AeroGpuVgaPortWindow {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let size = match size {
            0 => return 0,
            1 | 2 | 4 => size as usize,
            _ => return u32::MAX,
        };

        let mut out = 0u32;
        let mut dev = self.dev.borrow_mut();
        for i in 0..size {
            let p = port.wrapping_add(i as u16);
            let b = match p {
                // Input Status 1: reading resets the attribute controller flip-flop.
                0x03DA | 0x03BA => {
                    dev.attr_flip_flop = false;
                    u32::from(self.input_status1_value())
                }
                _ => u32::from(dev.vga_port_read_u8(p)),
            };
            out |= b << (i * 8);
        }
        out
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };

        let mut dev = self.dev.borrow_mut();
        for i in 0..size {
            let p = port.wrapping_add(i as u16);
            let b = ((value >> (i * 8)) & 0xFF) as u8;
            dev.vga_port_write_u8(p, b);
        }
    }
}

struct AeroGpuVbeDispiPortWindow {
    dev: Rc<RefCell<AeroGpuDevice>>,
}

impl aero_platform::io::PortIoDevice for AeroGpuVbeDispiPortWindow {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        // The Bochs VBE_DISPI interface is 16-bit register based (INDEX/DATA). Support only 16-bit
        // accesses for now; this matches how most real-mode drivers interact with the interface.
        if size == 0 {
            return 0;
        }
        if size != 2 {
            return u32::MAX;
        }

        let dev = self.dev.borrow();
        match port {
            aero_gpu_vga::VBE_DISPI_INDEX_PORT => u32::from(dev.vbe_dispi_index),
            aero_gpu_vga::VBE_DISPI_DATA_PORT => {
                u32::from(dev.vbe_dispi_read_reg(dev.vbe_dispi_index))
            }
            _ => 0,
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        if size != 2 {
            return;
        }

        let value = (value & 0xFFFF) as u16;
        let mut dev = self.dev.borrow_mut();
        match port {
            aero_gpu_vga::VBE_DISPI_INDEX_PORT => dev.vbe_dispi_index = value,
            aero_gpu_vga::VBE_DISPI_DATA_PORT => {
                let index = dev.vbe_dispi_index;
                dev.vbe_dispi_write_reg(index, value);
            }
            _ => {}
        }
    }
}

// -----------------------------------------------------------------------------
// AeroGPU snapshot encoding (machine-level, non-io-snapshot)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AeroGpuVgaDacSnapshotV1 {
    pel_mask: u8,
    palette: [[u8; 3]; 256],
}

#[derive(Debug, Clone)]
struct AeroGpuSnapshotV1 {
    bar0: crate::aerogpu::AeroGpuMmioSnapshotV1,
    vram: Vec<u8>,
    vga_dac: Option<AeroGpuVgaDacSnapshotV1>,
}

// AeroGPU snapshots can be surprisingly large (VRAM can be tens of MiB), and wasm builds in
// particular have limited headroom when taking an in-memory `Vec<u8>` snapshot.
//
// V1 snapshots stored a contiguous VRAM prefix. V2 switches to a sparse page list so that the
// common case (mostly-zero VRAM, e.g. headless tests) produces a small snapshot while preserving
// exact VRAM contents when pages are non-zero.
const AEROGPU_SNAPSHOT_VERSION_V2: u16 = 2;

fn encode_aerogpu_snapshot_v2(vram: &AeroGpuDevice, bar0: &AeroGpuMmioDevice) -> Vec<u8> {
    // Preserve at most the default legacy VGA/VBE VRAM backing size (matches the V1 policy).
    let vram_len = vram.vram.len().min(aero_gpu_vga::DEFAULT_VRAM_SIZE);
    let vram_len_u32: u32 = vram_len.try_into().unwrap_or(u32::MAX);

    // Sparse encoding: store non-zero pages.
    const PAGE_SIZE: usize = 4096;
    let page_size_u32: u32 = PAGE_SIZE as u32;

    let regs = bar0.snapshot_v1();

    // Conservative header reservation; payload grows with the number of non-zero pages.
    let mut out = Vec::with_capacity(256);

    // BAR0 register file (same field order as V1).
    out.extend_from_slice(&regs.abi_version.to_le_bytes());
    out.extend_from_slice(&regs.features.to_le_bytes());

    out.extend_from_slice(&regs.ring_gpa.to_le_bytes());
    out.extend_from_slice(&regs.ring_size_bytes.to_le_bytes());
    out.extend_from_slice(&regs.ring_control.to_le_bytes());

    out.extend_from_slice(&regs.fence_gpa.to_le_bytes());
    out.extend_from_slice(&regs.completed_fence.to_le_bytes());

    out.extend_from_slice(&regs.irq_status.to_le_bytes());
    out.extend_from_slice(&regs.irq_enable.to_le_bytes());

    out.extend_from_slice(&regs.scanout0_enable.to_le_bytes());
    out.extend_from_slice(&regs.scanout0_width.to_le_bytes());
    out.extend_from_slice(&regs.scanout0_height.to_le_bytes());
    out.extend_from_slice(&regs.scanout0_format.to_le_bytes());
    out.extend_from_slice(&regs.scanout0_pitch_bytes.to_le_bytes());
    out.extend_from_slice(&regs.scanout0_fb_gpa.to_le_bytes());

    out.extend_from_slice(&regs.scanout0_vblank_seq.to_le_bytes());
    out.extend_from_slice(&regs.scanout0_vblank_time_ns.to_le_bytes());
    out.extend_from_slice(&regs.scanout0_vblank_period_ns.to_le_bytes());

    out.extend_from_slice(&regs.cursor_enable.to_le_bytes());
    out.extend_from_slice(&regs.cursor_x.to_le_bytes());
    out.extend_from_slice(&regs.cursor_y.to_le_bytes());
    out.extend_from_slice(&regs.cursor_hot_x.to_le_bytes());
    out.extend_from_slice(&regs.cursor_hot_y.to_le_bytes());
    out.extend_from_slice(&regs.cursor_width.to_le_bytes());
    out.extend_from_slice(&regs.cursor_height.to_le_bytes());
    out.extend_from_slice(&regs.cursor_format.to_le_bytes());
    out.extend_from_slice(&regs.cursor_fb_gpa.to_le_bytes());
    out.extend_from_slice(&regs.cursor_pitch_bytes.to_le_bytes());

    // Host-only WDDM scanout ownership latch.
    out.push(regs.wddm_scanout_active as u8);

    // VRAM sparse page list:
    // - total length (bytes)
    // - page_size (bytes)
    // - page_count
    out.extend_from_slice(&vram_len_u32.to_le_bytes());
    out.extend_from_slice(&page_size_u32.to_le_bytes());
    let page_count_off = out.len();
    out.extend_from_slice(&0u32.to_le_bytes()); // patched after scanning

    let mut page_count: u32 = 0;
    for (idx, chunk) in vram.vram[..vram_len].chunks(PAGE_SIZE).enumerate() {
        if chunk.iter().all(|&b| b == 0) {
            continue;
        }
        page_count = page_count.saturating_add(1);
        let idx_u32: u32 = idx.try_into().unwrap_or(u32::MAX);
        let len_u32: u32 = chunk.len().try_into().unwrap_or(u32::MAX);
        out.extend_from_slice(&idx_u32.to_le_bytes());
        out.extend_from_slice(&len_u32.to_le_bytes());
        out.extend_from_slice(chunk);
    }

    out[page_count_off..page_count_off + 4].copy_from_slice(&page_count.to_le_bytes());

    // Optional trailing BAR0 error payload (ABI 1.3+).
    //
    // Keep this *after* the variable-length VRAM payload so older snapshot decoders that stop
    // after VRAM bytes remain forward-compatible.
    out.extend_from_slice(&regs.error_code.to_le_bytes());
    out.extend_from_slice(&regs.error_fence.to_le_bytes());
    out.extend_from_slice(&regs.error_count.to_le_bytes());
    // Preserve any pending scanout0 FB_GPA update (LO written, HI not yet committed). This keeps
    // snapshot/restore deterministic even if the VM is checkpointed between the two dword writes.
    out.extend_from_slice(&regs.scanout0_fb_gpa_pending_lo.to_le_bytes());
    out.extend_from_slice(&(regs.scanout0_fb_gpa_lo_pending as u32).to_le_bytes());
    out.extend_from_slice(&regs.cursor_fb_gpa_pending_lo.to_le_bytes());
    out.extend_from_slice(&(regs.cursor_fb_gpa_lo_pending as u32).to_le_bytes());

    // Optional trailing VGA DAC state (palette + PEL mask).
    //
    // This is required for deterministic 8bpp VBE output when the guest programs colors via VGA
    // DAC ports (`0x3C8/0x3C9`). The BIOS VBE palette snapshot does not capture port-driven DAC
    // updates, so store the DAC state here.
    out.extend_from_slice(b"DACP");
    out.push(vram.pel_mask);
    for entry in &vram.dac_palette {
        out.extend_from_slice(entry);
    }
    // Optional trailing VGA Attribute Controller state (palette mapping registers).
    //
    // The AeroGPU "legacy text scanout" path resolves text-mode colors via the VGA Attribute
    // Controller registers. Preserve these so snapshot/restore keeps text colors deterministic when
    // the guest programs 0x3C0/0x3C1.
    out.extend_from_slice(b"ATRG");
    // Attribute Controller indices are 5 bits wide (0x00..=0x1F). Masking on write means only the
    // first 32 bytes are observable/meaningful.
    out.extend_from_slice(&vram.attr_regs[..0x20]);

    // Optional trailing VGA DAC latches (indices + partial RGB triplet).
    //
    // This preserves deterministic snapshot/restore semantics even if the VM is checkpointed
    // mid-update (e.g. after writing `DAC_WRITE_INDEX` / one `DAC_DATA` byte but before the full
    // RGB triplet is complete).
    out.extend_from_slice(b"DACI");
    out.push(vram.dac_read_index);
    out.push(vram.dac_read_subindex);
    out.push(vram.dac_write_index);
    out.push(vram.dac_write_subindex);
    out.extend_from_slice(&vram.dac_write_latch);

    // Optional trailing VGA Attribute Controller flip-flop state.
    //
    // Similar to `DACI`, this keeps the `0x3C0` index/data sequencing deterministic across
    // snapshot/restore.
    out.extend_from_slice(b"ATST");
    out.push(vram.attr_index);
    out.push(vram.attr_flip_flop as u8);
    // Optional trailing VGA port register file (misc + seq/gc/crtc indices + regs).
    //
    // While the AeroGPU legacy VGA frontend is intentionally permissive and does not implement a
    // full VGA pipeline, guests may still read back port-programmed state after snapshot/restore.
    // Preserve these registers for determinism.
    out.extend_from_slice(b"VREG");
    out.push(vram.misc_output);
    out.push(vram.seq_index);
    out.extend_from_slice(&vram.seq_regs);
    out.push(vram.gc_index);
    out.extend_from_slice(&vram.gc_regs);
    out.push(vram.crtc_index);
    out.extend_from_slice(&vram.crtc_regs);

    // Optional trailing execution state (pending fences/submissions).
    //
    // This captures host-only state that is not visible through BAR0 registers but is required to
    // resume the device deterministically after restoring a machine snapshot (e.g. vsync-paced
    // fence completions and the WASM submission drain queue).
    out.extend_from_slice(b"EXEC");
    let exec_state = bar0.save_exec_snapshot_state_v1();
    let exec_len_u32: u32 = exec_state.len().try_into().unwrap_or(u32::MAX);
    out.extend_from_slice(&exec_len_u32.to_le_bytes());
    out.extend_from_slice(&exec_state);

    // Deterministic vblank timebase. Stored as trailing fields so older decoders can ignore them.
    out.extend_from_slice(&regs.now_ns.to_le_bytes());
    out.extend_from_slice(&regs.next_vblank_ns.unwrap_or(0).to_le_bytes());

    // Optional trailing Bochs VBE_DISPI register state.
    //
    // Some guests program VBE modes via the Bochs/QEMU VBE_DISPI ports (`0x01CE/0x01CF`) rather
    // than via BIOS INT 10h. Preserve this register file so snapshot/restore is deterministic for
    // those guests.
    //
    // Keep this *after* the fixed-size vblank timebase fields so older decoders (which stop after
    // reading the timebase) remain forward-compatible.
    out.extend_from_slice(b"BVBE");
    out.push(vram.vbe_dispi_guest_owned as u8);
    out.push(0); // reserved
    out.extend_from_slice(&vram.vbe_dispi_index.to_le_bytes());
    out.extend_from_slice(&vram.vbe_dispi_xres.to_le_bytes());
    out.extend_from_slice(&vram.vbe_dispi_yres.to_le_bytes());
    out.extend_from_slice(&vram.vbe_dispi_bpp.to_le_bytes());
    out.extend_from_slice(&vram.vbe_dispi_enable.to_le_bytes());
    out.extend_from_slice(&vram.vbe_bank.to_le_bytes());
    out.extend_from_slice(&vram.vbe_dispi_virt_width.to_le_bytes());
    out.extend_from_slice(&vram.vbe_dispi_virt_height.to_le_bytes());
    out.extend_from_slice(&vram.vbe_dispi_x_offset.to_le_bytes());
    out.extend_from_slice(&vram.vbe_dispi_y_offset.to_le_bytes());

    out
}

fn decode_aerogpu_snapshot_v1(bytes: &[u8]) -> Option<AeroGpuSnapshotV1> {
    fn read_u8(bytes: &[u8], off: &mut usize) -> Option<u8> {
        let b = *bytes.get(*off)?;
        *off += 1;
        Some(b)
    }

    fn read_u32(bytes: &[u8], off: &mut usize) -> Option<u32> {
        let end = off.checked_add(4)?;
        let slice = bytes.get(*off..end)?;
        *off = end;
        Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    fn read_u64(bytes: &[u8], off: &mut usize) -> Option<u64> {
        let end = off.checked_add(8)?;
        let slice = bytes.get(*off..end)?;
        *off = end;
        Some(u64::from_le_bytes([
            slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
        ]))
    }

    let mut off = 0usize;

    let abi_version = read_u32(bytes, &mut off)?;
    let features = read_u64(bytes, &mut off)?;

    let ring_gpa = read_u64(bytes, &mut off)?;
    let ring_size_bytes = read_u32(bytes, &mut off)?;
    let ring_control = read_u32(bytes, &mut off)?;

    let fence_gpa = read_u64(bytes, &mut off)?;
    let completed_fence = read_u64(bytes, &mut off)?;

    let irq_status = read_u32(bytes, &mut off)?;
    let irq_enable = read_u32(bytes, &mut off)?;

    let scanout0_enable = read_u32(bytes, &mut off)?;
    let scanout0_width = read_u32(bytes, &mut off)?;
    let scanout0_height = read_u32(bytes, &mut off)?;
    let scanout0_format = read_u32(bytes, &mut off)?;
    let scanout0_pitch_bytes = read_u32(bytes, &mut off)?;
    let scanout0_fb_gpa = read_u64(bytes, &mut off)?;

    let scanout0_vblank_seq = read_u64(bytes, &mut off)?;
    let scanout0_vblank_time_ns = read_u64(bytes, &mut off)?;
    let scanout0_vblank_period_ns = read_u32(bytes, &mut off)?;

    let cursor_enable = read_u32(bytes, &mut off)?;
    let cursor_x = read_u32(bytes, &mut off)?;
    let cursor_y = read_u32(bytes, &mut off)?;
    let cursor_hot_x = read_u32(bytes, &mut off)?;
    let cursor_hot_y = read_u32(bytes, &mut off)?;
    let cursor_width = read_u32(bytes, &mut off)?;
    let cursor_height = read_u32(bytes, &mut off)?;
    let cursor_format = read_u32(bytes, &mut off)?;
    let cursor_fb_gpa = read_u64(bytes, &mut off)?;
    let cursor_pitch_bytes = read_u32(bytes, &mut off)?;

    let wddm_scanout_active = read_u8(bytes, &mut off)? != 0;

    let vram_len = read_u32(bytes, &mut off)? as usize;
    let end = off.checked_add(vram_len)?;
    let vram = bytes.get(off..end)?.to_vec();
    off = end;

    // Trailing BAR0 error payload (ABI 1.3+). This was added after the initial snapshot v1 format,
    // so treat it as optional and default to zero when absent.
    let mut has_error_payload = false;
    let (error_code, error_fence, error_count) = if bytes.len() >= off.saturating_add(16) {
        has_error_payload = true;
        let error_code = read_u32(bytes, &mut off).unwrap_or(0);
        let error_fence = read_u64(bytes, &mut off).unwrap_or(0);
        let error_count = read_u32(bytes, &mut off).unwrap_or(0);
        (error_code, error_fence, error_count)
    } else {
        (0, 0, 0)
    };

    // Optional scanout0/cursor FB_GPA pending payloads (added after the error fields).
    //
    // These are currently encoded as raw `u32` pairs `(pending_lo, pending_flag_u32)`. The
    // snapshot format is forward-compatible (trailing bytes may be added), so restoring older
    // snapshots must not misinterpret tagged trailing payloads (e.g. `DACP`) as pending state.
    //
    // To detect presence, peek at the `pending_flag_u32` and only accept it when it is 0 or 1.
    //
    // Older snapshots may have tagged trailing payloads (e.g. `DACP`) immediately after the error
    // payload, while newer snapshots insert one or two pending pairs before the tags. Avoid
    // mis-parsing tag payloads as pending state by preferring tag detection at the expected offset
    // *after* the pending pairs.
    fn known_tag(bytes: &[u8], off: usize) -> bool {
        let Some(tag) = bytes.get(off..off.saturating_add(4)) else {
            return false;
        };

        if tag == b"DACP" {
            const PALETTE_LEN: usize = 256 * 3;
            const TOTAL_LEN: usize = 4 + 1 + PALETTE_LEN;
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"ATRG" {
            const TOTAL_LEN: usize = 4 + 0x20;
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"DACI" {
            const TOTAL_LEN: usize = 4 + (1 + 1 + 1 + 1 + 3);
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"ATST" {
            const TOTAL_LEN: usize = 4 + 2;
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"VREG" {
            const REG_LEN: usize = 256;
            const TOTAL_LEN: usize = 4 + (1 + 1 + REG_LEN + 1 + REG_LEN + 1 + REG_LEN);
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"EXEC" {
            let len_bytes = match bytes.get(off.saturating_add(4)..off.saturating_add(8)) {
                Some(b) => b,
                None => return false,
            };
            let byte_len =
                u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
                    as usize;
            let payload_end = off.saturating_add(8).saturating_add(byte_len);
            return bytes.len() >= payload_end;
        }
        if tag == b"BVBE" {
            const PAYLOAD_LEN: usize = 2 + 10 * 2;
            const TOTAL_LEN: usize = 4 + PAYLOAD_LEN;
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        false
    }
    fn peek_flag(bytes: &[u8], off: usize) -> Option<u32> {
        bytes
            .get(off..off.saturating_add(4))
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    let mut scanout0_fb_gpa_pending_lo = 0;
    let mut scanout0_fb_gpa_lo_pending = false;
    let mut cursor_fb_gpa_pending_lo = 0;
    let mut cursor_fb_gpa_lo_pending = false;

    if has_error_payload {
        // Prefer a deterministic tag-based probe when a known trailing section tag is present.
        //
        // Layout (after error payload):
        // - legacy: [TAG...]
        // - pending1: [u32 lo][u32 flag] [TAG...]
        // - pending2: [u32 lo][u32 flag] [u32 lo][u32 flag] [TAG...]
        let pending_pair_count = if known_tag(bytes, off.saturating_add(16)) {
            2
        } else if known_tag(bytes, off.saturating_add(8)) {
            1
        } else if known_tag(bytes, off) {
            0
        } else {
            usize::MAX
        };

        if pending_pair_count == 2
            && bytes.len() >= off.saturating_add(16)
            && peek_flag(bytes, off.saturating_add(4)).is_some_and(|flag| flag <= 1)
            && peek_flag(bytes, off.saturating_add(12)).is_some_and(|flag| flag <= 1)
        {
            scanout0_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
            scanout0_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
            cursor_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
            cursor_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
        } else if pending_pair_count == 1
            && bytes.len() >= off.saturating_add(8)
            && peek_flag(bytes, off.saturating_add(4)).is_some_and(|flag| flag <= 1)
        {
            scanout0_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
            scanout0_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
        } else if pending_pair_count == 0 {
            // No pending pairs; tags begin immediately.
        } else {
            // Fallback: probe for the raw `(pending_lo, pending_flag_u32)` pair(s).
            if bytes.len() >= off.saturating_add(8)
                && !known_tag(bytes, off)
                && peek_flag(bytes, off.saturating_add(4)).is_some_and(|flag| flag <= 1)
            {
                scanout0_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
                scanout0_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
            }
            if bytes.len() >= off.saturating_add(8)
                && !known_tag(bytes, off)
                && peek_flag(bytes, off.saturating_add(4)).is_some_and(|flag| flag <= 1)
            {
                cursor_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
                cursor_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
            }
        }
    }
    let vga_dac = {
        const TAG: &[u8; 4] = b"DACP";
        const PALETTE_LEN: usize = 256 * 3;
        const TOTAL_LEN: usize = 4 + 1 + PALETTE_LEN;

        if bytes.get(off..off.saturating_add(4)) == Some(TAG.as_slice())
            && bytes.len() >= off.saturating_add(TOTAL_LEN)
        {
            let pel_mask = bytes.get(off + 4).copied().unwrap_or(0xFF);
            let pal_bytes = bytes.get((off + 5)..(off + 5 + PALETTE_LEN)).unwrap_or(&[]);
            let mut palette = [[0u8; 3]; 256];
            for (idx, entry) in palette.iter_mut().enumerate() {
                let base = idx * 3;
                if base + 2 < pal_bytes.len() {
                    *entry = [pal_bytes[base], pal_bytes[base + 1], pal_bytes[base + 2]];
                }
            }
            off = off.saturating_add(TOTAL_LEN);
            Some(AeroGpuVgaDacSnapshotV1 { pel_mask, palette })
        } else {
            None
        }
    };

    // Forward-compatible: older snapshots do not have these trailing fields.
    let now_ns = read_u64(bytes, &mut off).unwrap_or(scanout0_vblank_time_ns);
    let next_raw = read_u64(bytes, &mut off).unwrap_or(0);
    let mut next_vblank_ns = if next_raw == 0 { None } else { Some(next_raw) };
    if next_vblank_ns.is_none() && scanout0_enable != 0 {
        let period_ns = u64::from(scanout0_vblank_period_ns);
        if period_ns != 0 {
            next_vblank_ns = Some(scanout0_vblank_time_ns.saturating_add(period_ns));
        }
    }
    // Forward-compatible: ignore trailing bytes from future versions.

    Some(AeroGpuSnapshotV1 {
        bar0: crate::aerogpu::AeroGpuMmioSnapshotV1 {
            abi_version,
            features,
            now_ns,
            next_vblank_ns,
            ring_gpa,
            ring_size_bytes,
            ring_control,
            fence_gpa,
            completed_fence,
            irq_status,
            irq_enable,
            error_code,
            error_fence,
            error_count,
            scanout0_enable,
            scanout0_width,
            scanout0_height,
            scanout0_format,
            scanout0_pitch_bytes,
            scanout0_fb_gpa,
            scanout0_fb_gpa_pending_lo,
            scanout0_fb_gpa_lo_pending,
            scanout0_vblank_seq,
            scanout0_vblank_time_ns,
            scanout0_vblank_period_ns,
            cursor_enable,
            cursor_x,
            cursor_y,
            cursor_hot_x,
            cursor_hot_y,
            cursor_width,
            cursor_height,
            cursor_format,
            cursor_fb_gpa,
            cursor_fb_gpa_pending_lo,
            cursor_fb_gpa_lo_pending,
            cursor_pitch_bytes,
            wddm_scanout_active,
        },
        vram,
        vga_dac,
    })
}

fn apply_aerogpu_snapshot_v2(
    bytes: &[u8],
    vram: &mut AeroGpuDevice,
    bar0: &mut AeroGpuMmioDevice,
) -> Option<bool> {
    fn read_u8(bytes: &[u8], off: &mut usize) -> Option<u8> {
        let b = *bytes.get(*off)?;
        *off += 1;
        Some(b)
    }

    fn read_u32(bytes: &[u8], off: &mut usize) -> Option<u32> {
        let end = off.checked_add(4)?;
        let slice = bytes.get(*off..end)?;
        *off = end;
        Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    fn read_u64(bytes: &[u8], off: &mut usize) -> Option<u64> {
        let end = off.checked_add(8)?;
        let slice = bytes.get(*off..end)?;
        *off = end;
        Some(u64::from_le_bytes([
            slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
        ]))
    }

    let mut off = 0usize;

    let abi_version = read_u32(bytes, &mut off)?;
    let features = read_u64(bytes, &mut off)?;

    let ring_gpa = read_u64(bytes, &mut off)?;
    let ring_size_bytes = read_u32(bytes, &mut off)?;
    let ring_control = read_u32(bytes, &mut off)?;

    let fence_gpa = read_u64(bytes, &mut off)?;
    let completed_fence = read_u64(bytes, &mut off)?;

    let irq_status = read_u32(bytes, &mut off)?;
    let irq_enable = read_u32(bytes, &mut off)?;

    let scanout0_enable = read_u32(bytes, &mut off)?;
    let scanout0_width = read_u32(bytes, &mut off)?;
    let scanout0_height = read_u32(bytes, &mut off)?;
    let scanout0_format = read_u32(bytes, &mut off)?;
    let scanout0_pitch_bytes = read_u32(bytes, &mut off)?;
    let scanout0_fb_gpa = read_u64(bytes, &mut off)?;

    let scanout0_vblank_seq = read_u64(bytes, &mut off)?;
    let scanout0_vblank_time_ns = read_u64(bytes, &mut off)?;
    let scanout0_vblank_period_ns = read_u32(bytes, &mut off)?;

    let cursor_enable = read_u32(bytes, &mut off)?;
    let cursor_x = read_u32(bytes, &mut off)?;
    let cursor_y = read_u32(bytes, &mut off)?;
    let cursor_hot_x = read_u32(bytes, &mut off)?;
    let cursor_hot_y = read_u32(bytes, &mut off)?;
    let cursor_width = read_u32(bytes, &mut off)?;
    let cursor_height = read_u32(bytes, &mut off)?;
    let cursor_format = read_u32(bytes, &mut off)?;
    let cursor_fb_gpa = read_u64(bytes, &mut off)?;
    let cursor_pitch_bytes = read_u32(bytes, &mut off)?;

    let wddm_scanout_active = read_u8(bytes, &mut off)? != 0;

    let vram_len = read_u32(bytes, &mut off)? as usize;
    let page_size = read_u32(bytes, &mut off)? as usize;
    if page_size == 0 {
        return None;
    }
    let page_count = read_u32(bytes, &mut off)? as usize;

    // Reset unsnapshotted state to a deterministic baseline before applying sparse pages.
    vram.reset();

    for _ in 0..page_count {
        let page_idx = read_u32(bytes, &mut off)? as usize;
        let page_len = read_u32(bytes, &mut off)? as usize;
        if page_len == 0 || page_len > page_size {
            return None;
        }
        let page_offset = page_idx.checked_mul(page_size)?;
        let end = page_offset.checked_add(page_len)?;
        if end > vram_len {
            return None;
        }
        let src_end = off.checked_add(page_len)?;
        let payload = bytes.get(off..src_end)?;
        off = src_end;

        let dst_end = page_offset.checked_add(page_len)?;
        if dst_end > vram.vram.len() {
            return None;
        }
        vram.vram[page_offset..dst_end].copy_from_slice(payload);
    }

    // Trailing BAR0 error payload (ABI 1.3+). This was added after the initial v2 encoding,
    // so treat it as optional and default to zero when absent.
    let mut has_error_payload = false;
    let (error_code, error_fence, error_count) = if bytes.len() >= off.saturating_add(16) {
        has_error_payload = true;
        let error_code = read_u32(bytes, &mut off).unwrap_or(0);
        let error_fence = read_u64(bytes, &mut off).unwrap_or(0);
        let error_count = read_u32(bytes, &mut off).unwrap_or(0);
        (error_code, error_fence, error_count)
    } else {
        (0, 0, 0)
    };

    // Optional scanout0/cursor FB_GPA pending payloads (added after the error fields).
    //
    // See `decode_aerogpu_snapshot_v1` for rationale on the `pending_flag_u32` probe and tag-based
    // detection.
    fn known_tag(bytes: &[u8], off: usize) -> bool {
        let Some(tag) = bytes.get(off..off.saturating_add(4)) else {
            return false;
        };

        if tag == b"DACP" {
            const PALETTE_LEN: usize = 256 * 3;
            const TOTAL_LEN: usize = 4 + 1 + PALETTE_LEN;
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"ATRG" {
            const TOTAL_LEN: usize = 4 + 0x20;
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"DACI" {
            const TOTAL_LEN: usize = 4 + (1 + 1 + 1 + 1 + 3);
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"ATST" {
            const TOTAL_LEN: usize = 4 + 2;
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"VREG" {
            const REG_LEN: usize = 256;
            const TOTAL_LEN: usize = 4 + (1 + 1 + REG_LEN + 1 + REG_LEN + 1 + REG_LEN);
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        if tag == b"EXEC" {
            let len_bytes = match bytes.get(off.saturating_add(4)..off.saturating_add(8)) {
                Some(b) => b,
                None => return false,
            };
            let byte_len =
                u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
                    as usize;
            let payload_end = off.saturating_add(8).saturating_add(byte_len);
            return bytes.len() >= payload_end;
        }
        if tag == b"BVBE" {
            const PAYLOAD_LEN: usize = 2 + 10 * 2;
            const TOTAL_LEN: usize = 4 + PAYLOAD_LEN;
            return bytes.len() >= off.saturating_add(TOTAL_LEN);
        }
        false
    }
    fn peek_flag(bytes: &[u8], off: usize) -> Option<u32> {
        bytes
            .get(off..off.saturating_add(4))
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    let mut scanout0_fb_gpa_pending_lo = 0;
    let mut scanout0_fb_gpa_lo_pending = false;
    let mut cursor_fb_gpa_pending_lo = 0;
    let mut cursor_fb_gpa_lo_pending = false;

    if has_error_payload {
        let pending_pair_count = if known_tag(bytes, off.saturating_add(16)) {
            2
        } else if known_tag(bytes, off.saturating_add(8)) {
            1
        } else if known_tag(bytes, off) {
            0
        } else {
            usize::MAX
        };

        if pending_pair_count == 2
            && bytes.len() >= off.saturating_add(16)
            && peek_flag(bytes, off.saturating_add(4)).is_some_and(|flag| flag <= 1)
            && peek_flag(bytes, off.saturating_add(12)).is_some_and(|flag| flag <= 1)
        {
            scanout0_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
            scanout0_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
            cursor_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
            cursor_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
        } else if pending_pair_count == 1
            && bytes.len() >= off.saturating_add(8)
            && peek_flag(bytes, off.saturating_add(4)).is_some_and(|flag| flag <= 1)
        {
            scanout0_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
            scanout0_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
        } else if pending_pair_count == 0 {
            // No pending pairs; tags begin immediately.
        } else {
            if bytes.len() >= off.saturating_add(8)
                && !known_tag(bytes, off)
                && peek_flag(bytes, off.saturating_add(4)).is_some_and(|flag| flag <= 1)
            {
                scanout0_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
                scanout0_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
            }
            if bytes.len() >= off.saturating_add(8)
                && !known_tag(bytes, off)
                && peek_flag(bytes, off.saturating_add(4)).is_some_and(|flag| flag <= 1)
            {
                cursor_fb_gpa_pending_lo = read_u32(bytes, &mut off).unwrap_or(0);
                cursor_fb_gpa_lo_pending = read_u32(bytes, &mut off).unwrap_or(0) != 0;
            }
        }
    }
    let mut restored_dac = false;
    let mut exec_state: Option<&[u8]> = None;
    // Optional trailing sections:
    // - `DACP`: VGA DAC state (PEL mask + palette)
    // - `ATRG`: VGA Attribute Controller palette mapping regs
    //
    // Keep these after the variable-length VRAM payload for forward compatibility: older decoders
    // stop after the page list, while newer versions can parse known tags and ignore the rest.
    while let Some(tag) = bytes.get(off..off.saturating_add(4)) {
        if tag == b"DACP" {
            const PALETTE_LEN: usize = 256 * 3;
            const TOTAL_LEN: usize = 4 + 1 + PALETTE_LEN;
            if bytes.len() < off.saturating_add(TOTAL_LEN) {
                break;
            }
            let pel_mask = bytes.get(off + 4).copied().unwrap_or(0xFF);
            let pal_bytes = bytes.get((off + 5)..(off + 5 + PALETTE_LEN)).unwrap_or(&[]);
            let mut palette = [[0u8; 3]; 256];
            for (idx, entry) in palette.iter_mut().enumerate() {
                let base = idx * 3;
                if base + 2 < pal_bytes.len() {
                    *entry = [pal_bytes[base], pal_bytes[base + 1], pal_bytes[base + 2]];
                }
            }
            vram.pel_mask = pel_mask;
            vram.dac_palette = palette;
            restored_dac = true;
            off = off.saturating_add(TOTAL_LEN);
            continue;
        }

        if tag == b"ATRG" {
            const ATTR_LEN: usize = 0x20;
            const TOTAL_LEN: usize = 4 + ATTR_LEN;
            if bytes.len() < off.saturating_add(TOTAL_LEN) {
                break;
            }
            if let Some(regs) = bytes.get((off + 4)..(off + 4 + ATTR_LEN)) {
                vram.attr_regs[..ATTR_LEN].copy_from_slice(regs);
            }
            off = off.saturating_add(TOTAL_LEN);
            continue;
        }

        if tag == b"DACI" {
            const PAYLOAD_LEN: usize = 1 + 1 + 1 + 1 + 3;
            const TOTAL_LEN: usize = 4 + PAYLOAD_LEN;
            if bytes.len() < off.saturating_add(TOTAL_LEN) {
                break;
            }
            if let Some(payload) = bytes.get((off + 4)..(off + 4 + PAYLOAD_LEN)) {
                vram.dac_read_index = payload[0];
                vram.dac_read_subindex = payload[1];
                vram.dac_write_index = payload[2];
                vram.dac_write_subindex = payload[3];
                vram.dac_write_latch.copy_from_slice(&payload[4..7]);
            }
            off = off.saturating_add(TOTAL_LEN);
            continue;
        }

        if tag == b"ATST" {
            const PAYLOAD_LEN: usize = 2;
            const TOTAL_LEN: usize = 4 + PAYLOAD_LEN;
            if bytes.len() < off.saturating_add(TOTAL_LEN) {
                break;
            }
            if let Some(payload) = bytes.get((off + 4)..(off + 4 + PAYLOAD_LEN)) {
                vram.attr_index = payload[0] & 0x1F;
                vram.attr_flip_flop = payload[1] != 0;
            }
            off = off.saturating_add(TOTAL_LEN);
            continue;
        }

        if tag == b"VREG" {
            const REG_LEN: usize = 256;
            const PAYLOAD_LEN: usize = 1 + 1 + REG_LEN + 1 + REG_LEN + 1 + REG_LEN;
            const TOTAL_LEN: usize = 4 + PAYLOAD_LEN;
            if bytes.len() < off.saturating_add(TOTAL_LEN) {
                break;
            }
            if let Some(payload) = bytes.get((off + 4)..(off + 4 + PAYLOAD_LEN)) {
                let mut idx = 0usize;
                vram.misc_output = payload[idx];
                idx += 1;
                vram.seq_index = payload[idx];
                idx += 1;
                vram.seq_regs.copy_from_slice(&payload[idx..idx + REG_LEN]);
                idx += REG_LEN;
                vram.gc_index = payload[idx];
                idx += 1;
                vram.gc_regs.copy_from_slice(&payload[idx..idx + REG_LEN]);
                idx += REG_LEN;
                vram.crtc_index = payload[idx];
                idx += 1;
                vram.crtc_regs.copy_from_slice(&payload[idx..idx + REG_LEN]);
            }
            off = off.saturating_add(TOTAL_LEN);
            continue;
        }

        if tag == b"EXEC" {
            // Format:
            // - tag "EXEC"
            // - u32 byte_len
            // - payload bytes (AeroGpuMmioDevice::save_exec_snapshot_state_v1)
            if bytes.len() < off.saturating_add(8) {
                break;
            }
            let len_bytes = bytes.get(off + 4..off + 8).unwrap_or(&[0u8, 0u8, 0u8, 0u8]);
            let byte_len =
                u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
                    as usize;
            let payload_off = off.saturating_add(8);
            let payload_end = payload_off.saturating_add(byte_len);
            if bytes.len() < payload_end {
                break;
            }
            exec_state = bytes.get(payload_off..payload_end);
            off = payload_end;
            continue;
        }

        break;
    }
    // Trailing deterministic vblank timebase (added after the error/DAC payloads).
    let now_ns = read_u64(bytes, &mut off).unwrap_or(scanout0_vblank_time_ns);
    let next_raw = read_u64(bytes, &mut off).unwrap_or(0);
    let mut next_vblank_ns = if next_raw == 0 { None } else { Some(next_raw) };
    if next_vblank_ns.is_none() && scanout0_enable != 0 {
        let period_ns = u64::from(scanout0_vblank_period_ns);
        if period_ns != 0 {
            next_vblank_ns = Some(scanout0_vblank_time_ns.saturating_add(period_ns));
        }
    }
    // Optional trailing Bochs VBE_DISPI register file.
    //
    // This is stored after the vblank timebase fields so older decoders can ignore it.
    if bytes.get(off..off.saturating_add(4)) == Some(b"BVBE".as_slice()) {
        const PAYLOAD_LEN: usize = 2 + 10 * 2;
        const TOTAL_LEN: usize = 4 + PAYLOAD_LEN;
        if bytes.len() >= off.saturating_add(TOTAL_LEN) {
            if let Some(payload) = bytes.get((off + 4)..(off + 4 + PAYLOAD_LEN)) {
                let mut idx = 0usize;
                vram.vbe_dispi_guest_owned = payload[idx] != 0;
                idx += 2; // guest_owned + reserved

                let mut read_u16 = || -> u16 {
                    if idx + 1 >= payload.len() {
                        return 0;
                    }
                    let v = u16::from_le_bytes([payload[idx], payload[idx + 1]]);
                    idx += 2;
                    v
                };

                vram.vbe_dispi_index = read_u16();
                vram.vbe_dispi_xres = read_u16();
                vram.vbe_dispi_yres = read_u16();
                vram.vbe_dispi_bpp = read_u16();
                vram.vbe_dispi_enable = read_u16();
                vram.vbe_bank = read_u16();
                vram.vbe_dispi_virt_width = read_u16();
                vram.vbe_dispi_virt_height = read_u16();
                vram.vbe_dispi_x_offset = read_u16();
                vram.vbe_dispi_y_offset = read_u16();
            }
        }
    }

    // Forward-compatible: ignore trailing bytes from future versions (including unknown tags).

    bar0.reset();
    bar0.restore_snapshot_v1(&crate::aerogpu::AeroGpuMmioSnapshotV1 {
        abi_version,
        features,
        now_ns,
        next_vblank_ns,
        ring_gpa,
        ring_size_bytes,
        ring_control,
        fence_gpa,
        completed_fence,
        irq_status,
        irq_enable,
        error_code,
        error_fence,
        error_count,
        scanout0_enable,
        scanout0_width,
        scanout0_height,
        scanout0_format,
        scanout0_pitch_bytes,
        scanout0_fb_gpa,
        scanout0_fb_gpa_pending_lo,
        scanout0_fb_gpa_lo_pending,
        scanout0_vblank_seq,
        scanout0_vblank_time_ns,
        scanout0_vblank_period_ns,
        cursor_enable,
        cursor_x,
        cursor_y,
        cursor_hot_x,
        cursor_hot_y,
        cursor_width,
        cursor_height,
        cursor_format,
        cursor_fb_gpa,
        cursor_fb_gpa_pending_lo,
        cursor_fb_gpa_lo_pending,
        cursor_pitch_bytes,
        wddm_scanout_active,
    });

    if let Some(exec_state) = exec_state {
        if bar0.load_exec_snapshot_state_v1(exec_state).is_err() {
            return None;
        }
    }

    Some(restored_dac)
}

// -----------------------------------------------------------------------------
// PC platform MMIO adapters (LAPIC / IOAPIC / HPET)
// -----------------------------------------------------------------------------
struct HpetMmio {
    hpet: Rc<RefCell<hpet::Hpet<ManualClock>>>,
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl MmioHandler for HpetMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return 0;
        }
        let mut hpet = self.hpet.borrow_mut();
        let mut interrupts = self.interrupts.borrow_mut();
        hpet.mmio_read(offset, size, &mut *interrupts)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return;
        }
        let mut hpet = self.hpet.borrow_mut();
        let mut interrupts = self.interrupts.borrow_mut();
        hpet.mmio_write(offset, size, value, &mut *interrupts);
    }
}

/// Per-port `PortIoDevice` view into a shared PIIX3 IDE controller.
struct IdePort {
    pci_cfg: SharedPciConfigPorts,
    ide: Rc<RefCell<Piix3IdePciDevice>>,
    bdf: PciBdf,
    port: u16,
}

impl IdePort {
    fn sync_config(&self) {
        let (command, bar4_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let bus = pci_cfg.bus_mut();
            let cfg = bus.device_config(self.bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar4_base = cfg.and_then(|cfg| cfg.bar_range(4)).map(|range| range.base);
            (command, bar4_base)
        };

        let mut ide = self.ide.borrow_mut();
        ide.config_mut().set_command(command);
        if let Some(bar4_base) = bar4_base {
            ide.config_mut().set_bar_base(4, bar4_base);
        }
    }
}

impl aero_platform::io::PortIoDevice for IdePort {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        self.sync_config();
        self.ide.borrow_mut().io_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        self.sync_config();
        self.ide.borrow_mut().io_write(port, size, value);
    }
}

/// Bus Master IDE (BAR4) handler registered via the machine's PCI I/O window.
///
/// `offset` is interpreted as the device-relative offset within BAR4.
struct IdeBusMasterBar {
    pci_cfg: SharedPciConfigPorts,
    ide: Rc<RefCell<Piix3IdePciDevice>>,
    bdf: PciBdf,
}

impl IdeBusMasterBar {
    fn sync_config(&self) {
        let (command, bar4_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let bus = pci_cfg.bus_mut();
            let cfg = bus.device_config(self.bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar4_base = cfg.and_then(|cfg| cfg.bar_range(4)).map(|range| range.base);
            (command, bar4_base)
        };

        let mut ide = self.ide.borrow_mut();
        ide.config_mut().set_command(command);
        if let Some(bar4_base) = bar4_base {
            ide.config_mut().set_bar_base(4, bar4_base);
        }
    }
}

impl PciIoBarHandler for IdeBusMasterBar {
    fn io_read(&mut self, offset: u64, size: usize) -> u32 {
        self.sync_config();
        let base = { self.ide.borrow().bus_master_base() };
        let abs_port = base.wrapping_add(offset as u16);
        self.ide.borrow_mut().io_read(abs_port, size as u8)
    }

    fn io_write(&mut self, offset: u64, size: usize, value: u32) {
        self.sync_config();
        let base = { self.ide.borrow().bus_master_base() };
        let abs_port = base.wrapping_add(offset as u16);
        self.ide.borrow_mut().io_write(abs_port, size as u8, value);
    }
}

struct UhciIoBar {
    pci_cfg: SharedPciConfigPorts,
    uhci: Rc<RefCell<UhciPciDevice>>,
    bdf: PciBdf,
}

impl UhciIoBar {
    fn sync_config(&self) {
        let (command, bar4_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let bus = pci_cfg.bus_mut();
            let cfg = bus.device_config(self.bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar4_base = cfg
                .and_then(|cfg| cfg.bar_range(UhciPciDevice::IO_BAR_INDEX))
                .map(|range| range.base);
            (command, bar4_base)
        };

        let mut uhci = self.uhci.borrow_mut();
        uhci.config_mut().set_command(command);
        if let Some(bar4_base) = bar4_base {
            uhci.config_mut()
                .set_bar_base(UhciPciDevice::IO_BAR_INDEX, bar4_base);
        }
    }
}

impl PciIoBarHandler for UhciIoBar {
    fn io_read(&mut self, offset: u64, size: usize) -> u32 {
        self.sync_config();
        let offset = u16::try_from(offset).unwrap_or(0);
        self.uhci
            .borrow_mut()
            .controller_mut()
            .io_read(offset, size)
    }

    fn io_write(&mut self, offset: u64, size: usize, value: u32) {
        self.sync_config();
        let offset = u16::try_from(offset).unwrap_or(0);
        self.uhci
            .borrow_mut()
            .controller_mut()
            .io_write(offset, size, value);
    }
}

struct E1000PciIoBar {
    dev: Rc<RefCell<E1000Device>>,
}

impl PciIoBarHandler for E1000PciIoBar {
    fn io_read(&mut self, offset: u64, size: usize) -> u32 {
        let offset = u32::try_from(offset).unwrap_or(0);
        self.dev.borrow_mut().io_read(offset, size)
    }

    fn io_write(&mut self, offset: u64, size: usize, value: u32) {
        let offset = u32::try_from(offset).unwrap_or(0);
        self.dev.borrow_mut().io_write_reg(offset, size, value);
    }
}

type SharedPciIoBarRouter = Rc<RefCell<PciIoBarRouter>>;

#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
#[derive(Clone)]
enum SharedStateHandle<T: 'static> {
    Arc(Arc<T>),
    Static(&'static T),
}

#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
impl<T: 'static> core::ops::Deref for SharedStateHandle<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Arc(inner) => inner.as_ref(),
            Self::Static(inner) => inner,
        }
    }
}

struct PciIoBarWindow {
    router: SharedPciIoBarRouter,
}

impl PciIoBarWindow {
    fn read_all_ones(size: u8) -> u32 {
        match size {
            0 => 0,
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl aero_platform::io::PortIoDevice for PciIoBarWindow {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let mask = Self::read_all_ones(size);
        let size_usize = match size {
            1 | 2 | 4 => size as usize,
            _ => return mask,
        };
        self.router
            .borrow_mut()
            .dispatch_read(port, size_usize)
            .map(|v| v & mask)
            .unwrap_or(mask)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        let size_usize = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };
        let _ = self
            .router
            .borrow_mut()
            .dispatch_write(port, size_usize, value);
    }
}

#[derive(Clone)]
enum InstallMedia {
    /// The machine is the sole owner of the ISO backend (e.g. IDE is disabled, so BIOS is the only
    /// consumer).
    Strong(SharedIsoDisk),
    /// Only keep a weak reference to the ISO backend so guest-initiated ejects (ATAPI START STOP
    /// UNIT) can drop the final strong reference and release exclusive file handles (e.g. OPFS
    /// `SyncAccessHandle`).
    Weak(SharedIsoDiskWeak),
}

impl InstallMedia {
    fn upgrade(&self) -> Option<SharedIsoDisk> {
        match self {
            InstallMedia::Strong(disk) => Some(disk.clone()),
            InstallMedia::Weak(weak) => weak.upgrade(),
        }
    }
}

struct LegacyVgaLfbWindow {
    base: u64,
    size: u64,
    handler: VgaLfbMmioHandler,
}

/// MMIO dispatcher for the full ACPI-reported PCI MMIO window.
///
/// This primarily routes PCI BARs via [`PciBarMmioRouter`], but can also host fixed MMIO mappings
/// that live inside the PCI MMIO address range (such as the standalone VGA/VBE LFB when
/// `enable_vga=true`).
struct PciMmioWindow {
    window_base: u64,
    router: PciBarMmioRouter,
    legacy_vga_lfb: Option<LegacyVgaLfbWindow>,
}

impl MmioHandler for PciMmioWindow {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if size > 8 {
            return u64::MAX;
        }

        if let (Some(lfb), Some(paddr)) = (
            self.legacy_vga_lfb.as_mut(),
            self.window_base.checked_add(offset),
        ) {
            let size_u64 = u64::try_from(size).unwrap_or(0);
            let access_end = paddr.saturating_add(size_u64);
            let lfb_end = lfb.base.saturating_add(lfb.size);
            if paddr >= lfb.base && access_end <= lfb_end {
                return MmioHandler::read(&mut lfb.handler, paddr - lfb.base, size);
            }
        }

        MmioHandler::read(&mut self.router, offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        if let (Some(lfb), Some(paddr)) = (
            self.legacy_vga_lfb.as_mut(),
            self.window_base.checked_add(offset),
        ) {
            let size_u64 = u64::try_from(size).unwrap_or(0);
            let access_end = paddr.saturating_add(size_u64);
            let lfb_end = lfb.base.saturating_add(lfb.size);
            if paddr >= lfb.base && access_end <= lfb_end {
                MmioHandler::write(&mut lfb.handler, paddr - lfb.base, size, value);
                return;
            }
        }

        MmioHandler::write(&mut self.router, offset, size, value);
    }
}

/// Canonical Aero machine: CPU + physical memory + port I/O devices + firmware.
pub struct Machine {
    cfg: MachineConfig,
    chipset: ChipsetState,
    reset_latch: ResetLatch,

    cpu: CpuCore,
    ap_cpus: Vec<CpuCore>,
    assist: AssistContext,
    mmu: aero_mmu::Mmu,
    mem: SystemMemory,
    io: IoPortBus,

    // Host-facing display scanout cache (populated by `display_present`).
    display_fb: Vec<u32>,
    display_width: u32,
    display_height: u32,

    // Optional shared scanout descriptor used by the browser presentation pipeline.
    //
    // This is only available when publishing into an external shared scanout header is supported:
    // native builds always support this, and the wasm32 build supports it when built with the
    // `wasm-threaded` feature.
    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    scanout_state: Option<SharedStateHandle<ScanoutState>>,

    // Optional shared hardware cursor descriptor used by the browser presentation pipeline.
    //
    // This is only available when publishing into an external shared header is supported: native
    // builds and the wasm32 `wasm-threaded` build.
    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    cursor_state: Option<SharedStateHandle<CursorState>>,

    // ---------------------------------------------------------------------
    // Host-managed storage overlay references (snapshot DISKS section)
    // ---------------------------------------------------------------------
    //
    // Aero snapshots intentionally do not embed any disk contents; only references to the host's
    // chosen base images + writable overlays are stored.
    //
    // Some storage controller device models also drop/detach host backends during `load_state()`,
    // so snapshot restore typically requires the host/coordinator to re-open and re-attach the
    // correct overlays/media. (Even for controllers that *do* preserve their in-memory backend
    // pointer, snapshots still never embed disk bytes.)
    //
    // These fields exist to make that host contract explicit and deterministic.
    ahci_port0_overlay: Option<snapshot::DiskOverlayRef>,
    ide_secondary_master_atapi_overlay: Option<snapshot::DiskOverlayRef>,
    ide_primary_master_overlay: Option<snapshot::DiskOverlayRef>,
    restored_disk_overlays: Option<snapshot::DiskOverlayRefs>,

    // Optional PC platform devices. These are behind `Rc<RefCell<_>>` so their host wiring
    // survives snapshot restore (devices reset their internal state but preserve callbacks/irq
    // lines).
    platform_clock: Option<ManualClock>,
    interrupts: Option<Rc<RefCell<PlatformInterrupts>>>,
    pit: Option<SharedPit8254>,
    rtc: Option<SharedRtcCmos<ManualClock, PlatformIrqLine>>,
    pci_cfg: Option<SharedPciConfigPorts>,
    pci_intx: Option<Rc<RefCell<PciIntxRouter>>>,
    acpi_pm: Option<SharedAcpiPmIo<ManualClock>>,
    hpet: Option<Rc<RefCell<hpet::Hpet<ManualClock>>>>,
    e1000: Option<Rc<RefCell<E1000Device>>>,
    virtio_net: Option<Rc<RefCell<VirtioPciDevice>>>,
    virtio_input_keyboard: Option<Rc<RefCell<VirtioPciDevice>>>,
    virtio_input_mouse: Option<Rc<RefCell<VirtioPciDevice>>>,
    vga: Option<Rc<RefCell<VgaDevice>>>,
    aerogpu: Option<Rc<RefCell<AeroGpuDevice>>>,
    aerogpu_mmio: Option<Rc<RefCell<AeroGpuMmioDevice>>>,
    ahci: Option<Rc<RefCell<AhciPciDevice>>>,
    nvme: Option<Rc<RefCell<NvmePciDevice>>>,
    ide: Option<Rc<RefCell<Piix3IdePciDevice>>>,
    virtio_blk: Option<Rc<RefCell<VirtioPciDevice>>>,
    uhci: Option<Rc<RefCell<UhciPciDevice>>>,
    ehci: Option<Rc<RefCell<EhciPciDevice>>>,
    xhci: Option<Rc<RefCell<XhciPciDevice>>>,
    /// Optional synthetic USB HID devices behind an external hub on UHCI root port 0.
    usb_hid_keyboard: Option<UsbHidKeyboardHandle>,
    usb_hid_mouse: Option<UsbHidMouseHandle>,
    usb_hid_gamepad: Option<UsbHidGamepadHandle>,
    usb_hid_consumer_control: Option<UsbHidConsumerControlHandle>,
    /// ISA IRQ line handles used to deliver legacy IDE interrupts (IRQ14/15) without over/under-
    /// counting assertions when the machine polls device state.
    ide_irq14_line: Option<PlatformIrqLine>,
    ide_irq15_line: Option<PlatformIrqLine>,
    uhci_ns_remainder: u64,
    ehci_ns_remainder: u64,
    xhci_ns_remainder: u64,
    bios: Bios,
    disk: SharedDisk,
    install_media: Option<InstallMedia>,
    /// Host-selected BIOS boot drive number exposed in `DL` when transferring control to the boot
    /// sector.
    boot_drive: u8,
    /// Whether the machine should automatically keep its canonical [`SharedDisk`] attached to the
    /// canonical AHCI boot slot (port 0).
    ///
    /// By default, [`Machine`] attaches `SharedDisk` to AHCI port 0 so BIOS INT13 and AHCI DMA see
    /// consistent bytes.
    ///
    /// When a host/test explicitly attaches a different drive to AHCI port 0 via
    /// [`Machine::attach_ahci_drive_port0`] / [`Machine::attach_ahci_disk_port0`], this flag is
    /// cleared so subsequent [`Machine::reset`] calls and [`Machine::set_disk_image`] calls do not
    /// clobber the host-provided backend.
    ahci_port0_auto_attach_shared_disk: bool,
    /// Whether the machine should automatically keep its canonical [`SharedDisk`] attached to the
    /// canonical virtio-blk device.
    ///
    /// By default, [`Machine::set_disk_backend`] / [`Machine::set_disk_image`] rebuild the
    /// virtio-blk device to point at the shared disk so BIOS INT13, AHCI, and virtio-blk can
    /// observe consistent disk bytes.
    ///
    /// When a host/test explicitly attaches a different disk backend to virtio-blk via
    /// [`Machine::attach_virtio_blk_disk`], this flag is cleared so subsequent shared-disk updates
    /// do not clobber the host-provided backend.
    virtio_blk_auto_attach_shared_disk: bool,
    network_backend: Option<Box<dyn NetworkBackend>>,

    serial: Option<SharedSerial16550>,
    i8042: Option<SharedI8042Controller>,
    serial_log: Vec<u8>,
    debugcon_log: SharedDebugConLog,
    ps2_mouse_buttons: u8,
    // Tracks which backend delivered the most recent press for each Consumer Control usage so
    // releases are routed consistently even if virtio-input becomes ready mid-hold.
    //
    // Encoding:
    // - 0: not pressed / unknown
    // - 1: synthetic USB consumer-control device
    // - 2: virtio-input keyboard (media keys subset)
    consumer_usage_backend: [u8; 0x0400],
    // Host-side pressed key tracking for `Machine::inject_input_batch` keyboard backend switching.
    //
    // The input batch stream includes both:
    // - PS/2 Set-2 scancodes (`InputEventType::KeyScancode`), and
    // - USB HID keyboard usages (`InputEventType::KeyHidUsage`).
    //
    // `Machine::inject_input_batch` selects one keyboard backend (virtio/USB/PS2) based on driver
    // readiness and device configuration. Without tracking pressed state, that selection could
    // change between a press and release, leaving the previous backend "stuck" (no matching
    // release). Track pressed HID usages so we can keep the keyboard backend stable while any key
    // is held.
    input_batch_pressed_keyboard_usages: [u8; 256],
    input_batch_pressed_keyboard_usage_count: u16,
    // Cached keyboard backend selection used by `Machine::inject_input_batch`.
    //
    // Encoding:
    // - 0: PS/2 i8042 (KeyScancode)
    // - 1: synthetic USB HID keyboard (KeyHidUsage)
    // - 2: virtio-input keyboard (KeyHidUsage -> Linux KEY_*)
    input_batch_keyboard_backend: u8,
    // Current mouse button mask tracked from `InputEventType::MouseButtons` events in
    // `Machine::inject_input_batch`. Used to prevent backend switches while a button is held.
    input_batch_mouse_buttons_mask: u8,
    // Cached mouse backend selection used by `Machine::inject_input_batch`.
    //
    // Encoding:
    // - 0: PS/2 i8042 mouse
    // - 1: synthetic USB HID mouse
    // - 2: virtio-input mouse
    input_batch_mouse_backend: u8,

    next_snapshot_id: u64,
    last_snapshot_id: Option<u64>,
    /// Deterministic guest time accumulator used when converting CPU cycles (TSC ticks) into
    /// nanoseconds for platform device ticking.
    guest_time: GuestTime,

    /// Deferred snapshot restore error surfaced via `SnapshotTarget::post_restore`.
    ///
    /// `SnapshotTarget::restore_device_states` does not return a `Result`, so machine restore logic
    /// uses this field to record configuration mismatches that should abort restore (for example
    /// restoring xHCI state into a machine with xHCI disabled).
    restore_error: Option<snapshot::SnapshotError>,
}

/// Active scanout source selected by [`Machine::display_present`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanoutSource {
    /// Legacy VGA text mode.
    LegacyText,
    /// Legacy VGA graphics mode (currently mode 13h).
    LegacyVga,
    /// Legacy VBE linear framebuffer (VGA/VBE path).
    LegacyVbe,
    /// AeroGPU WDDM scanout (BAR0-programmed scanout registers).
    Wddm,
}

impl Machine {
    /// Return the machine's immutable configuration.
    ///
    /// This is primarily intended for host/coordinator code that needs to inspect the VM topology
    /// (e.g. how many CPUs were requested) after construction.
    pub fn config(&self) -> &MachineConfig {
        &self.cfg
    }

    // ---------------------------------------------------------------------
    // Stable snapshot disk ids (normative)
    // ---------------------------------------------------------------------
    //
    // These IDs are the contract between:
    // - the machine's canonical storage topology,
    // - the snapshot format's `DISKS` section (`aero_snapshot::DiskOverlayRefs`), and
    // - the host/coordinator that opens and re-attaches disk/ISO backends after restore.
    //
    // Canonical Windows 7 storage topology is documented in:
    // - `docs/05-storage-topology-win7.md`
    //
    // Note: this crate does *not* inline any disk bytes into snapshots; these ids only identify
    // which *external* overlays should be re-opened.

    /// `disk_id=0`: Primary HDD (AHCI `SATA_AHCI_ICH9` port 0).
    pub const DISK_ID_PRIMARY_HDD: u32 = 0;
    /// `disk_id=1`: Install media / CD-ROM (IDE `IDE_PIIX3` secondary channel master ATAPI).
    pub const DISK_ID_INSTALL_MEDIA: u32 = 1;
    /// `disk_id=2`: Optional IDE primary master ATA disk (if exposed as a separately managed disk).
    pub const DISK_ID_IDE_PRIMARY_MASTER: u32 = 2;

    // ---------------------------------------------------------------------
    // UHCI synthetic HID topology constants (normative)
    // ---------------------------------------------------------------------
    //
    // These constants mirror `web/src/usb/uhci_external_hub.ts` and `docs/08-input-devices.md` so
    // browser/WASM integrations can share a single guest-visible USB topology contract regardless
    // of whether the UHCI topology is managed by JS or auto-attached by `aero_machine::Machine`.

    /// UHCI root port index reserved for the external hub (synthetic HID + WebHID passthrough).
    pub const UHCI_EXTERNAL_HUB_ROOT_PORT: u8 = 0;
    /// UHCI root port index reserved for the guest-visible WebUSB passthrough device.
    pub const UHCI_WEBUSB_ROOT_PORT: u8 = 1;
    /// Default downstream port count for the external hub on [`Self::UHCI_EXTERNAL_HUB_ROOT_PORT`].
    pub const UHCI_EXTERNAL_HUB_PORT_COUNT: u8 = 16;
    /// External hub port number for the built-in USB HID keyboard.
    pub const UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT: u8 = 1;
    /// External hub port number for the built-in USB HID mouse.
    pub const UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT: u8 = 2;
    /// External hub port number for the built-in USB HID gamepad.
    pub const UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT: u8 = 3;
    /// External hub port number for the built-in USB HID consumer-control (media keys).
    pub const UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT: u8 = 4;
    /// Number of downstream hub ports reserved for built-in synthetic HID devices.
    pub const UHCI_SYNTHETIC_HID_HUB_PORT_COUNT: u8 = 4;
    /// First hub port number that is safe for dynamic passthrough allocation (e.g. WebHID) without
    /// colliding with built-in synthetic devices.
    pub const UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT: u8 =
        Self::UHCI_SYNTHETIC_HID_HUB_PORT_COUNT + 1;

    fn validate_cfg(cfg: &MachineConfig) -> Result<(), MachineError> {
        if cfg.cpu_count == 0 {
            return Err(MachineError::InvalidCpuCount(cfg.cpu_count));
        }
        if cfg.enable_e1000 && cfg.enable_virtio_net {
            return Err(MachineError::MultipleNicsEnabled);
        }
        if cfg.enable_aerogpu {
            if !cfg.enable_pc_platform {
                return Err(MachineError::AeroGpuRequiresPcPlatform);
            }
            if cfg.enable_vga {
                return Err(MachineError::AeroGpuConflictsWithVga);
            }
        }
        if cfg.enable_pc_platform && cfg.enable_vga && !cfg.enable_aerogpu {
            // The standalone legacy VGA/VBE LFB aperture is mapped directly inside the ACPI PCI
            // MMIO window by the full PCI MMIO router. Validate that the configured base can be
            // reached via that window.
            let bar_size = Self::legacy_vga_pci_bar_size_bytes_for_cfg(cfg);
            let (requested_base, aligned_base) = Self::legacy_vga_lfb_base_for_cfg(cfg, bar_size);
            let base_u64 = u64::from(aligned_base);
            let end_u64 = base_u64.saturating_add(u64::from(bar_size));
            let window_start = PCI_MMIO_BASE;
            let window_end = PCI_MMIO_BASE + PCI_MMIO_SIZE;
            if base_u64 < window_start || end_u64 > window_end {
                return Err(MachineError::VgaLfbOutsidePciMmioWindow {
                    requested_base,
                    aligned_base,
                    size: bar_size,
                });
            }
        }
        if (cfg.enable_ahci
            || cfg.enable_nvme
            || cfg.enable_ide
            || cfg.enable_virtio_blk
            || cfg.enable_virtio_input)
            && !cfg.enable_pc_platform
        {
            if cfg.enable_ahci {
                return Err(MachineError::AhciRequiresPcPlatform);
            }
            if cfg.enable_nvme {
                return Err(MachineError::NvmeRequiresPcPlatform);
            }
            if cfg.enable_ide {
                return Err(MachineError::IdeRequiresPcPlatform);
            }
            if cfg.enable_virtio_input {
                return Err(MachineError::VirtioInputRequiresPcPlatform);
            }
            return Err(MachineError::VirtioBlkRequiresPcPlatform);
        }
        if cfg.enable_synthetic_usb_hid && !cfg.enable_uhci {
            return Err(MachineError::SyntheticUsbHidRequiresUhci);
        }
        if cfg.enable_uhci && !cfg.enable_pc_platform {
            return Err(MachineError::UhciRequiresPcPlatform);
        }
        if cfg.enable_ehci && !cfg.enable_pc_platform {
            return Err(MachineError::EhciRequiresPcPlatform);
        }
        if cfg.enable_xhci && !cfg.enable_pc_platform {
            return Err(MachineError::XhciRequiresPcPlatform);
        }
        if cfg.enable_e1000 && !cfg.enable_pc_platform {
            return Err(MachineError::E1000RequiresPcPlatform);
        }
        if cfg.enable_virtio_net && !cfg.enable_pc_platform {
            return Err(MachineError::VirtioNetRequiresPcPlatform);
        }
        Ok(())
    }

    fn build(cfg: MachineConfig, chipset: ChipsetState, mem: SystemMemory) -> Self {
        let boot_drive = cfg.boot_drive;
        Self {
            cfg,
            chipset,
            reset_latch: ResetLatch::new(),
            cpu: CpuCore::new(CpuMode::Real),
            ap_cpus: Vec::new(),
            assist: AssistContext::default(),
            mmu: aero_mmu::Mmu::new(),
            mem,
            io: IoPortBus::new(),
            display_fb: Vec::new(),
            display_width: 0,
            display_height: 0,
            #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
            scanout_state: None,
            #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
            cursor_state: None,
            ahci_port0_overlay: None,
            ide_secondary_master_atapi_overlay: None,
            ide_primary_master_overlay: None,
            restored_disk_overlays: None,
            platform_clock: None,
            interrupts: None,
            pit: None,
            rtc: None,
            pci_cfg: None,
            pci_intx: None,
            acpi_pm: None,
            hpet: None,
            e1000: None,
            virtio_net: None,
            virtio_input_keyboard: None,
            virtio_input_mouse: None,
            vga: None,
            aerogpu: None,
            aerogpu_mmio: None,
            ahci: None,
            nvme: None,
            ide: None,
            virtio_blk: None,
            uhci: None,
            usb_hid_keyboard: None,
            usb_hid_mouse: None,
            usb_hid_gamepad: None,
            usb_hid_consumer_control: None,
            ehci: None,
            xhci: None,
            ide_irq14_line: None,
            ide_irq15_line: None,
            uhci_ns_remainder: 0,
            ehci_ns_remainder: 0,
            xhci_ns_remainder: 0,
            bios: Bios::new(BiosConfig {
                boot_drive,
                ..Default::default()
            }),
            disk: SharedDisk::from_bytes(Vec::new()).expect("empty disk is valid"),
            install_media: None,
            boot_drive,
            ahci_port0_auto_attach_shared_disk: true,
            virtio_blk_auto_attach_shared_disk: true,
            network_backend: None,
            serial: None,
            i8042: None,
            serial_log: Vec::new(),
            debugcon_log: Rc::new(RefCell::new(Vec::new())),
            ps2_mouse_buttons: 0,
            consumer_usage_backend: [0u8; 0x0400],
            input_batch_pressed_keyboard_usages: [0u8; 256],
            input_batch_pressed_keyboard_usage_count: 0,
            input_batch_keyboard_backend: 0,
            input_batch_mouse_buttons_mask: 0,
            input_batch_mouse_backend: 0,
            next_snapshot_id: 1,
            last_snapshot_id: None,
            guest_time: GuestTime::default(),
            restore_error: None,
        }
    }

    pub fn new(mut cfg: MachineConfig) -> Result<Self, MachineError> {
        // Normalize boot selection:
        //
        // - `boot_drive` is the canonical, backwards-compatible selector (used directly by BIOS).
        // - `boot_device` is a convenience/high-level selector.
        //
        // If callers set `boot_device=Cdrom` but leave the default HDD boot drive (`0x80`), treat
        // that as a request to boot from CD and upgrade the boot drive to the canonical CD0 drive
        // number (`0xE0`).
        if cfg.boot_device == BootDevice::Cdrom && cfg.boot_drive == 0x80 {
            cfg.boot_drive = 0xE0;
        }
        cfg.boot_device = if (0xE0..=0xEF).contains(&cfg.boot_drive) {
            BootDevice::Cdrom
        } else {
            BootDevice::Hdd
        };

        Self::validate_cfg(&cfg)?;

        let chipset = ChipsetState::new(false);
        let mem = SystemMemory::new(cfg.ram_size_bytes, chipset.a20())?;

        let mut machine = Self::build(cfg, chipset, mem);

        machine.reset();
        Ok(machine)
    }

    /// Construct a machine using an externally provided guest RAM backend.
    ///
    /// This is intended for `wasm32` builds where guest RAM can be backed by the host-provided
    /// WebAssembly linear memory region (avoiding large Rust heap allocations).
    ///
    /// `backing.size()` must match `cfg.ram_size_bytes`.
    pub fn new_with_guest_memory(
        mut cfg: MachineConfig,
        backing: Box<dyn memory::GuestMemory>,
    ) -> Result<Self, MachineError> {
        // Normalize boot selection like `Machine::new`.
        if cfg.boot_device == BootDevice::Cdrom && cfg.boot_drive == 0x80 {
            cfg.boot_drive = 0xE0;
        }
        cfg.boot_device = if (0xE0..=0xEF).contains(&cfg.boot_drive) {
            BootDevice::Cdrom
        } else {
            BootDevice::Hdd
        };
        Self::validate_cfg(&cfg)?;

        let actual = backing.size();
        let expected = cfg.ram_size_bytes;
        if actual != expected {
            return Err(MachineError::GuestMemorySizeMismatch { expected, actual });
        }

        let chipset = ChipsetState::new(false);
        let mem = SystemMemory::new_with_backing(backing, chipset.a20())?;

        let mut machine = Self::build(cfg, chipset, mem);
        machine.reset();
        Ok(machine)
    }

    /// Set the deterministic seed used to generate the SMBIOS Type 1 "System UUID".
    ///
    /// This only takes effect after the next [`Machine::reset`], when BIOS POST rebuilds SMBIOS
    /// tables.
    pub fn set_smbios_uuid_seed(&mut self, seed: u64) {
        self.cfg.smbios_uuid_seed = seed;
    }

    /// Convenience constructor for the canonical Windows 7 storage topology.
    ///
    /// This is equivalent to `Machine::new(MachineConfig::win7_storage_defaults(ram_size_bytes))`.
    ///
    /// See `docs/05-storage-topology-win7.md` for the normative BDFs and media attachment mapping.
    pub fn new_with_win7_storage(ram_size_bytes: u64) -> Result<Self, MachineError> {
        Self::new(MachineConfig::win7_storage_defaults(ram_size_bytes))
    }

    /// Convenience constructor for the canonical Windows 7 install flow (CD-first).
    ///
    /// This configures:
    /// - the canonical Windows 7 storage topology (AHCI + IDE at canonical BDFs),
    /// - the IDE secondary master as an ATAPI CD-ROM backed by `iso`, and
    /// - firmware boot policy to prefer the first CD-ROM (`DL=0xE0`) when install media is present
    ///   so El Torito install media boots without additional boilerplate.
    ///
    /// Notes:
    /// - The install ISO is stored separately from the canonical [`SharedDisk`] so callers can
    ///   still attach an HDD image (installed OS disk) via [`Machine::set_disk_backend`] /
    ///   [`Machine::set_disk_image`].
    /// - This helper resets the machine after attaching the ISO so firmware POST transfers control
    ///   to the ISO boot image immediately.
    pub fn new_with_win7_install(
        ram_size_bytes: u64,
        iso: Box<dyn aero_storage::VirtualDisk>,
    ) -> Result<Self, MachineError> {
        let mut m = Self::new(MachineConfig::win7_storage_defaults(ram_size_bytes))?;
        m.configure_win7_install_boot(iso)
            .map_err(|e| MachineError::DiskBackend(e.to_string()))?;
        Ok(m)
    }

    /// Configure the machine for the canonical Windows 7 install boot flow (CD-first).
    ///
    /// This is a builder-style helper for callers that construct a [`Machine`] first (for example
    /// to tweak other config fields) and then want to attach an install ISO and reboot into it.
    ///
    /// This method:
    /// - enables the firmware "CD-first when present" boot policy (boot from `DL=0xE0` when install
    ///   media is attached, otherwise fall back to the configured HDD boot drive),
    /// - attaches the ISO as an ATAPI CD-ROM on the IDE secondary master (if IDE is enabled),
    /// - then resets the machine.
    pub fn configure_win7_install_boot(
        &mut self,
        iso: Box<dyn aero_storage::VirtualDisk>,
    ) -> std::io::Result<()> {
        // Canonical Win7 install flow prefers the first CD-ROM when install media is present, but
        // keeps the configured HDD boot drive as a fallback (e.g. after ejecting the ISO).
        //
        // If the machine is currently configured to boot from a CD drive number (e.g. constructed
        // with `MachineConfig::win7_install_defaults`), override it to HDD0 so the CD-first policy
        // has a meaningful fallback when the ISO is absent or unbootable.
        if (0xE0..=0xEF).contains(&self.cfg.boot_drive) {
            self.set_boot_drive(0x80);
        }
        self.set_cd_boot_drive(0xE0);
        self.set_boot_from_cd_if_present(true);

        // Attach the ISO to the canonical install-media attachment point (IDE secondary master).
        self.attach_ide_secondary_master_iso(iso)?;

        // Re-run firmware POST so control transfers to the ISO boot image.
        self.reset();
        Ok(())
    }

    fn map_pc_platform_mmio_regions(&mut self) {
        if !self.cfg.enable_pc_platform {
            return;
        }

        let (Some(interrupts), Some(hpet), Some(pci_cfg)) =
            (&self.interrupts, &self.hpet, &self.pci_cfg)
        else {
            return;
        };

        let interrupts = interrupts.clone();
        let hpet = hpet.clone();
        let pci_cfg = pci_cfg.clone();

        // NOTE: The LAPIC MMIO window is per-vCPU even though it lives at a shared physical
        // address (0xFEE0_0000). Do not map it into the shared `SystemMemory` bus; CPU execution
        // wraps `SystemMemory` in a per-vCPU adapter that routes LAPIC accesses to the currently
        // running vCPU's LAPIC instance.
        self.mem
            .map_mmio_once(IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, || {
                Box::new(IoApicMmio::from_platform_interrupts(interrupts.clone()))
            });
        self.mem
            .map_mmio_once(hpet::HPET_MMIO_BASE, hpet::HPET_MMIO_SIZE, || {
                Box::new(HpetMmio {
                    hpet: hpet.clone(),
                    interrupts: interrupts.clone(),
                })
            });

        let ecam_cfg = PciEcamConfig {
            segment: firmware::bios::PCIE_ECAM_SEGMENT,
            start_bus: firmware::bios::PCIE_ECAM_START_BUS,
            end_bus: firmware::bios::PCIE_ECAM_END_BUS,
        };
        let ecam_len = ecam_cfg.window_size_bytes();
        self.mem
            .map_mmio_once(firmware::bios::PCIE_ECAM_BASE, ecam_len, || {
                Box::new(PciEcamMmio::new(pci_cfg, ecam_cfg))
            });
    }

    fn ensure_uhci_synthetic_usb_hid_topology(&mut self) {
        if !(self.cfg.enable_pc_platform
            && self.cfg.enable_uhci
            && self.cfg.enable_synthetic_usb_hid)
        {
            return;
        }
        let Some(uhci) = &self.uhci else {
            return;
        };

        let external_hub_root_port = usize::from(Self::UHCI_EXTERNAL_HUB_ROOT_PORT);

        let mut uhci = uhci.borrow_mut();
        let root_hub = uhci.controller_mut().hub_mut();

        // Best-effort: ensure the canonical "external hub + synthetic HID" USB topology is present
        // without overwriting any host-attached devices.
        //
        // This is intentionally conservative: if root port 0 is occupied by a non-hub device (or a
        // hub with insufficient ports), we do not detach/overwrite it. This matters for tests and
        // for host runtimes that temporarily attach other device models to root port 0.
        //
        // On snapshot restore we still want the synthetic devices to exist ahead of
        // `RootHub::load_snapshot_ports` so the loader can reuse the existing instances (handle
        // stability). In the canonical case, machines that enable this feature also boot with this
        // topology, so this helper is sufficient.
        if root_hub.port_device(external_hub_root_port).is_none() {
            root_hub.attach(
                external_hub_root_port,
                Box::new(UsbHubDevice::with_port_count(
                    Self::UHCI_EXTERNAL_HUB_PORT_COUNT,
                )),
            );
        }

        let Some(mut root_port0) = root_hub.port_device_mut(external_hub_root_port) else {
            return;
        };
        let Some(port_count) = root_port0.model().hub_port_count() else {
            // Root port 0 is occupied by a non-hub device; leave it alone.
            return;
        };

        let hub = root_port0.model_mut();

        if port_count >= Self::UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT
            && hub
                .hub_port_device_mut(Self::UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT)
                .is_err()
        {
            let keyboard = self
                .usb_hid_keyboard
                .get_or_insert_with(UsbHidKeyboardHandle::new)
                .clone();
            let _ = hub.hub_attach_device(
                Self::UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT,
                Box::new(keyboard),
            );
        }
        if port_count >= Self::UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT
            && hub
                .hub_port_device_mut(Self::UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT)
                .is_err()
        {
            let mouse = self
                .usb_hid_mouse
                .get_or_insert_with(UsbHidMouseHandle::new)
                .clone();
            let _ = hub.hub_attach_device(Self::UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT, Box::new(mouse));
        }
        if port_count >= Self::UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT
            && hub
                .hub_port_device_mut(Self::UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT)
                .is_err()
        {
            let gamepad = self
                .usb_hid_gamepad
                .get_or_insert_with(UsbHidGamepadHandle::new)
                .clone();
            let _ =
                hub.hub_attach_device(Self::UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT, Box::new(gamepad));
        }
        if port_count >= Self::UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT
            && hub
                .hub_port_device_mut(Self::UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT)
                .is_err()
        {
            let consumer = self
                .usb_hid_consumer_control
                .get_or_insert_with(UsbHidConsumerControlHandle::new)
                .clone();
            let _ = hub.hub_attach_device(
                Self::UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT,
                Box::new(consumer),
            );
        }
    }

    /// Returns the current CPU state.
    pub fn cpu(&self) -> &CpuState {
        &self.cpu.state
    }

    /// Debug/testing helper: returns the CPU state for vCPU `idx`.
    ///
    /// This exists to make SMP bring-up tests deterministic: the canonical `Machine` exposes
    /// multiple CPUs that share guest-physical address space, so tests need a stable way to inspect
    /// per-vCPU state without relying on guest execution.
    ///
    /// # Panics
    /// Panics if `idx` is out of range.
    pub fn cpu_by_index(&self, idx: usize) -> &CpuState {
        let cpu_count = self.cfg.cpu_count as usize;
        assert!(
            idx < cpu_count,
            "cpu index {idx} out of range (cpu_count={})",
            self.cfg.cpu_count
        );
        if idx == 0 {
            &self.cpu.state
        } else {
            &self.ap_cpus[idx - 1].state
        }
    }

    /// Mutable access to the current CPU state (debug/testing only).
    pub fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu.state
    }

    /// Debug/testing helper: returns mutable access to the CPU core for vCPU `idx`.
    ///
    /// # Panics
    /// Panics if `idx` is out of range.
    pub fn cpu_core_mut_by_index(&mut self, idx: usize) -> &mut CpuCore {
        let cpu_count = self.cfg.cpu_count as usize;
        assert!(
            idx < cpu_count,
            "cpu index {idx} out of range (cpu_count={})",
            self.cfg.cpu_count
        );
        if idx == 0 {
            &mut self.cpu
        } else {
            &mut self.ap_cpus[idx - 1]
        }
    }

    /// Guest physical address of the ACPI Root System Description Pointer (RSDP), if published.
    ///
    /// The firmware builds ACPI tables during POST/reset when ACPI is enabled in the BIOS
    /// configuration and writes the RSDP into guest memory. Higher-level runtimes can use this
    /// address to locate the rest of the ACPI table hierarchy without re-scanning the BIOS/EBDA
    /// regions.
    ///
    /// Returns `None` if ACPI table generation was disabled for the current firmware configuration
    /// (or if firmware POST has not run yet).
    pub fn acpi_rsdp_addr(&self) -> Option<u64> {
        self.bios.rsdp_addr()
    }

    /// Guest physical address of the SMBIOS Entry Point Structure (EPS), if published.
    ///
    /// The firmware builds SMBIOS tables during POST/reset and publishes an SMBIOS EPS in guest
    /// memory so guests can discover the DMI/SMBIOS structures. This method exposes the published
    /// EPS address directly.
    ///
    /// Returns `None` if SMBIOS table generation was disabled for the current firmware
    /// configuration (or if firmware POST has not run yet).
    pub fn smbios_eps_addr(&self) -> Option<u32> {
        self.bios.smbios_eps_addr()
    }

    /// Set the BIOS boot drive number exposed in `DL` when transferring control to the boot
    /// sector.
    ///
    /// Defaults to `0x80` (first hard disk).
    ///
    /// This controls which BIOS drive number the firmware treats as the active boot device (for
    /// example, `0x80` for the first HDD, or `0xE0` for a CD-ROM).
    ///
    /// The selection is stored in the BIOS configuration so it is captured/restored by snapshots
    /// and inherited by subsequent [`Machine::reset`] calls. Call [`Machine::reset`] to apply the
    /// new value to the next boot.
    pub fn set_boot_drive(&mut self, boot_drive: u8) {
        self.boot_drive = boot_drive;
        self.cfg.boot_drive = boot_drive;
        self.cfg.boot_device = if (0xE0..=0xEF).contains(&boot_drive) {
            BootDevice::Cdrom
        } else {
            BootDevice::Hdd
        };
        // Keep the current BIOS config in sync so snapshots capture the selected boot drive and so
        // `Machine::reset()` can persist it.
        self.bios.set_boot_drive(boot_drive);
    }

    /// Set the preferred BIOS boot device for the next [`Machine::reset`].
    ///
    /// This is a convenience wrapper around [`Machine::set_boot_drive`]:
    /// - [`BootDevice::Hdd`] maps to `boot_drive=0x80` (first hard disk).
    /// - [`BootDevice::Cdrom`] maps to `boot_drive=0xE0` (first CD-ROM).
    ///
    /// This does not affect the currently-running guest. Call [`Machine::reset`] to re-run BIOS
    /// POST and attempt boot from the newly-selected device.
    pub fn set_boot_device(&mut self, boot_device: BootDevice) {
        self.cfg.boot_device = boot_device;
        let boot_drive = match boot_device {
            BootDevice::Hdd => 0x80,
            BootDevice::Cdrom => 0xE0,
        };
        self.set_boot_drive(boot_drive);
    }

    /// Returns the configured boot device preference.
    pub fn boot_device(&self) -> BootDevice {
        self.cfg.boot_device
    }

    /// Returns the effective boot device used for the current boot session.
    pub fn active_boot_device(&self) -> BootDevice {
        if self.bios.booted_from_cdrom() {
            BootDevice::Cdrom
        } else {
            BootDevice::Hdd
        }
    }

    /// Returns the configured BIOS boot drive number (`DL`) used for the next firmware POST/boot.
    pub fn boot_drive(&self) -> u8 {
        self.boot_drive
    }

    /// Returns whether the firmware "CD-first when present" boot policy is enabled.
    pub fn boot_from_cd_if_present(&self) -> bool {
        self.bios.boot_from_cd_if_present()
    }

    /// Returns the BIOS drive number used for CD-ROM boot when the "CD-first when present" policy
    /// is enabled.
    pub fn cd_boot_drive(&self) -> u8 {
        self.bios.cd_boot_drive()
    }

    /// Returns whether the guest currently reports install media inserted in the canonical ATAPI
    /// CD-ROM slot (IDE secondary master).
    ///
    /// This reflects guest-visible tray/media state (`media_present`) rather than whether a host ISO
    /// backend is currently attached: snapshot restore intentionally drops host backends.
    pub fn install_media_is_inserted(&self) -> bool {
        self.ide
            .as_ref()
            .map(|ide| {
                ide.borrow()
                    .controller
                    .secondary_master_atapi_media_present()
            })
            .unwrap_or(false)
    }

    /// Enable/disable the firmware "CD-first when present" boot policy.
    ///
    /// When enabled, firmware POST will attempt to boot from the first CD-ROM drive when install
    /// media is attached, and fall back to the configured [`Machine::set_boot_drive`] selection
    /// (typically HDD0, `DL=0x80`) when no CD is present or the CD is not bootable.
    ///
    /// Call [`Machine::reset`] to apply the new policy to the next boot.
    pub fn set_boot_from_cd_if_present(&mut self, enabled: bool) {
        self.bios.set_boot_from_cd_if_present(enabled);
    }

    /// Set the BIOS drive number used when booting from CD-ROM under the
    /// "CD-first when present" policy.
    ///
    /// Conventional El Torito CD-ROM drive numbers are `0xE0..=0xEF`; the canonical machine uses
    /// `0xE0` for the first CD-ROM.
    ///
    /// Call [`Machine::reset`] to apply the new value to the next boot.
    pub fn set_cd_boot_drive(&mut self, cd_boot_drive: u8) {
        self.bios.set_cd_boot_drive(cd_boot_drive);
    }

    /// Returns the configured vCPU count.
    pub fn cpu_count(&self) -> usize {
        self.cfg.cpu_count as usize
    }

    /// Debug/testing helper: return a clone of a vCPU's architectural state.
    ///
    /// vCPU0 is the BSP (`Machine::cpu()`); vCPU1..N-1 are APs.
    pub fn vcpu_state(&self, cpu_index: usize) -> Option<CpuState> {
        if cpu_index == 0 {
            Some(self.cpu.state.clone())
        } else {
            self.ap_cpus.get(cpu_index - 1).map(|cpu| cpu.state.clone())
        }
    }

    /// Replace the attached disk image.
    pub fn set_disk_image(&mut self, bytes: Vec<u8>) -> Result<(), MachineError> {
        self.disk.set_bytes(bytes)?;
        // Keep storage controllers that are backed by the shared disk (e.g. AHCI) in sync with the
        // new disk geometry. In particular, the ATA IDENTIFY sector is derived from disk capacity,
        // so swapping in a new-sized image requires rebuilding the attached drive.
        self.attach_shared_disk_to_storage_controllers()?;
        Ok(())
    }

    /// Replace the machine's canonical disk backend (BIOS + storage controllers).
    ///
    /// This is the preferred API for non-`Vec<u8>` disk backends (OPFS, streaming, sparse formats,
    /// etc.). The same disk contents are visible to:
    /// - BIOS POST / INT 13h (`firmware::bios::BlockDevice`), and
    /// - storage controllers (AHCI/IDE) attached by this machine.
    ///
    /// Note: if the new backend has a different capacity, this call rebuilds any ATA drives that
    /// were derived from the shared disk so IDENTIFY geometry remains coherent.
    pub fn set_disk_backend(
        &mut self,
        backend: Box<dyn aero_storage::VirtualDisk>,
    ) -> Result<(), MachineError> {
        self.disk.set_backend(backend);
        self.attach_shared_disk_to_storage_controllers()?;
        Ok(())
    }

    /// Returns a cloneable handle to the machine's canonical disk backend.
    ///
    /// This is the same disk used by BIOS INT13 services, and (when enabled) is also attached as
    /// the backend for emulated storage controllers such as AHCI.
    ///
    /// Note: mutating the returned handle directly (e.g. via [`SharedDisk::set_backend`]) updates
    /// the underlying bytes for all users, but may *not* rebuild attached controller drive models
    /// that derive geometry from capacity (ATA IDENTIFY). Prefer [`Machine::set_disk_backend`] /
    /// [`Machine::set_disk_image`] when changing the disk contents.
    pub fn shared_disk(&self) -> SharedDisk {
        self.disk.clone()
    }

    fn attach_shared_disk_to_storage_controllers(&mut self) -> Result<(), MachineError> {
        // Canonical AHCI port 0.
        if let Some(ahci) = self.ahci.as_ref() {
            if self.ahci_port0_auto_attach_shared_disk {
                let drive = AtaDrive::new(Box::new(self.disk.clone()))
                    .map_err(|e| MachineError::DiskBackend(e.to_string()))?;
                ahci.borrow_mut().attach_drive(0, drive);
            }
        }

        // Canonical virtio-blk device.
        if self.virtio_blk_auto_attach_shared_disk {
            // virtio-blk capacity is derived from the backend size at device creation time, so when
            // the shared disk backend is replaced we rebuild the device to keep the reported
            // `VirtioBlkConfig::capacity` coherent.
            //
            // Preserve virtio-pci transport state so snapshot restore flows can reattach disk bytes
            // without losing queue configuration / negotiated features.
            self.swap_virtio_blk_backend_preserving_state(Box::new(self.disk.clone()))?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Snapshot disk overlay configuration (host-managed storage backends)
    // ---------------------------------------------------------------------

    /// Set the overlay reference for the canonical primary HDD (`disk_id=0`).
    pub fn set_ahci_port0_disk_overlay_ref(
        &mut self,
        base_image: impl Into<String>,
        overlay_image: impl Into<String>,
    ) {
        self.ahci_port0_overlay = Some(snapshot::DiskOverlayRef {
            disk_id: Self::DISK_ID_PRIMARY_HDD,
            base_image: base_image.into(),
            overlay_image: overlay_image.into(),
        });
    }

    /// Clear the overlay reference for the canonical primary HDD (`disk_id=0`).
    pub fn clear_ahci_port0_disk_overlay_ref(&mut self) {
        self.ahci_port0_overlay = None;
    }

    /// Set the overlay reference for the canonical install media / CD-ROM (`disk_id=1`).
    pub fn set_ide_secondary_master_atapi_overlay_ref(
        &mut self,
        base_image: impl Into<String>,
        overlay_image: impl Into<String>,
    ) {
        self.ide_secondary_master_atapi_overlay = Some(snapshot::DiskOverlayRef {
            disk_id: Self::DISK_ID_INSTALL_MEDIA,
            base_image: base_image.into(),
            overlay_image: overlay_image.into(),
        });
    }

    /// Clear the overlay reference for the canonical install media / CD-ROM (`disk_id=1`).
    pub fn clear_ide_secondary_master_atapi_overlay_ref(&mut self) {
        self.ide_secondary_master_atapi_overlay = None;
    }

    /// Set the overlay reference for an optional IDE primary master ATA disk (`disk_id=2`).
    pub fn set_ide_primary_master_ata_overlay_ref(
        &mut self,
        base_image: impl Into<String>,
        overlay_image: impl Into<String>,
    ) {
        self.ide_primary_master_overlay = Some(snapshot::DiskOverlayRef {
            disk_id: Self::DISK_ID_IDE_PRIMARY_MASTER,
            base_image: base_image.into(),
            overlay_image: overlay_image.into(),
        });
    }

    /// Clear the overlay reference for an optional IDE primary master ATA disk (`disk_id=2`).
    pub fn clear_ide_primary_master_ata_overlay_ref(&mut self) {
        self.ide_primary_master_overlay = None;
    }

    /// Return any disk overlay refs captured from the most recent snapshot restore.
    ///
    /// This is intended for host/coordinator code that needs to re-open and re-attach storage
    /// backends after restore.
    pub fn restored_disk_overlays(&self) -> Option<&snapshot::DiskOverlayRefs> {
        self.restored_disk_overlays.as_ref()
    }

    /// Take and clear the disk overlay refs captured from the most recent snapshot restore.
    pub fn take_restored_disk_overlays(&mut self) -> Option<snapshot::DiskOverlayRefs> {
        self.restored_disk_overlays.take()
    }

    /// Install/replace the host-side network backend used by any emulated NICs.
    ///
    /// Note: this backend is *external* state (e.g. a live tunnel connection) and is intentionally
    /// not included in snapshots. Callers should either:
    /// - re-attach after restoring a snapshot, or
    /// - call [`Machine::detach_network`] before snapshotting to make the lifecycle explicit.
    pub fn set_network_backend(&mut self, backend: Box<dyn NetworkBackend>) {
        if let Some(virtio) = &self.virtio_net {
            let mut virtio = virtio.borrow_mut();
            if let Some(net) = virtio.device_mut::<VirtioNet<VirtioNetBackendAdapter>>() {
                net.backend_mut().set_backend(Some(backend));
                return;
            }
        }
        self.network_backend = Some(backend);
    }

    /// Attach a ring-buffer-backed L2 tunnel network backend (NET_TX / NET_RX).
    pub fn attach_l2_tunnel_rings<TX: FrameRing + 'static, RX: FrameRing + 'static>(
        &mut self,
        tx: TX,
        rx: RX,
    ) {
        self.set_network_backend(Box::new(L2TunnelRingBackend::new(tx, rx)));
    }

    /// Convenience for native callers using [`aero_ipc::ring::RingBuffer`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn attach_l2_tunnel_rings_native(
        &mut self,
        tx: aero_ipc::ring::RingBuffer,
        rx: aero_ipc::ring::RingBuffer,
    ) {
        self.attach_l2_tunnel_rings(tx, rx);
    }

    /// Convenience for WASM/browser callers using [`aero_ipc::wasm::SharedRingBuffer`].
    #[cfg(target_arch = "wasm32")]
    pub fn attach_l2_tunnel_rings_wasm(
        &mut self,
        tx: aero_ipc::wasm::SharedRingBuffer,
        rx: aero_ipc::wasm::SharedRingBuffer,
    ) {
        self.attach_l2_tunnel_rings(tx, rx);
    }

    /// Detach (drop) any currently installed network backend.
    pub fn detach_network(&mut self) {
        self.network_backend = None;
        if let Some(virtio) = &self.virtio_net {
            let mut virtio = virtio.borrow_mut();
            if let Some(net) = virtio.device_mut::<VirtioNet<VirtioNetBackendAdapter>>() {
                net.backend_mut().set_backend(None);
            }
        }
    }

    /// Return best-effort stats for the attached `NET_TX`/`NET_RX` ring backend (if present).
    pub fn network_backend_l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        if let Some(virtio) = &self.virtio_net {
            // `VirtioNet` currently only exposes a `backend_mut()` accessor. We only need `&self`
            // for stats, but borrow mutably via `RefCell` so the same API can be used regardless of
            // which NIC model is enabled.
            let mut virtio = virtio.borrow_mut();
            if let Some(net) = virtio.device_mut::<VirtioNet<VirtioNetBackendAdapter>>() {
                return net.backend_mut().l2_ring_stats();
            }
        }
        self.network_backend
            .as_ref()
            .and_then(|b| b.l2_ring_stats())
    }

    /// Debug/testing helper: read a single guest physical byte.
    pub fn read_physical_u8(&mut self, paddr: u64) -> u8 {
        self.mem.read_u8(paddr)
    }

    /// Debug/testing helper: read a little-endian u16 from guest physical memory.
    pub fn read_physical_u16(&mut self, paddr: u64) -> u16 {
        self.mem.read_u16(paddr)
    }

    /// Debug/testing helper: read a little-endian u32 from guest physical memory.
    pub fn read_physical_u32(&mut self, paddr: u64) -> u32 {
        self.mem.read_u32(paddr)
    }

    /// Debug/testing helper: read a little-endian u64 from guest physical memory.
    pub fn read_physical_u64(&mut self, paddr: u64) -> u64 {
        self.mem.read_u64(paddr)
    }

    /// Debug/testing helper: read a range of guest physical memory into a new buffer.
    pub fn read_physical_bytes(&mut self, paddr: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.mem.read_physical(paddr, &mut out);
        out
    }

    /// Debug/testing helper: read a u32 from the LAPIC MMIO window as seen by `cpu_index`.
    ///
    /// This uses the same per-vCPU LAPIC routing path as CPU execution (a per-vCPU physical bus
    /// wrapper around the shared `SystemMemory`).
    pub fn read_lapic_u32(&mut self, cpu_index: usize, offset: u64) -> u32 {
        assert!(
            cpu_index < self.cfg.cpu_count as usize,
            "cpu_index {cpu_index} out of range (cpu_count={})",
            self.cfg.cpu_count
        );
        let apic_id = cpu_index as u8;
        let interrupts = self.interrupts.clone();
        let mut bus = PerCpuSystemMemoryBus::new(
            apic_id,
            interrupts,
            ApCpus::All(self.ap_cpus.as_mut_slice()),
            &mut self.mem,
        );
        aero_mmu::MemoryBus::read_u32(&mut bus, LAPIC_MMIO_BASE + offset)
    }

    /// Debug/testing helper: write a u32 to the LAPIC MMIO window as seen by `cpu_index`.
    ///
    /// This uses the same per-vCPU LAPIC routing path as CPU execution.
    pub fn write_lapic_u32(&mut self, cpu_index: usize, offset: u64, value: u32) {
        assert!(
            cpu_index < self.cfg.cpu_count as usize,
            "cpu_index {cpu_index} out of range (cpu_count={})",
            self.cfg.cpu_count
        );
        let apic_id = cpu_index as u8;
        let interrupts = self.interrupts.clone();
        let mut bus = PerCpuSystemMemoryBus::new(
            apic_id,
            interrupts,
            ApCpus::All(self.ap_cpus.as_mut_slice()),
            &mut self.mem,
        );
        aero_mmu::MemoryBus::write_u32(&mut bus, LAPIC_MMIO_BASE + offset, value);
    }

    /// Debug/testing helper: assert ("raise") a platform GSI (Global System Interrupt) input line.
    ///
    /// This is intended for `crates/aero-machine/tests/*` integration tests that need to drive
    /// platform interrupt input pins deterministically without reaching into internal device
    /// models.
    ///
    /// When the PC platform is enabled, this forwards to
    /// [`PlatformInterrupts::raise_irq`] with [`InterruptInput::Gsi`]. When the PC platform is not
    /// enabled, this is a no-op.
    pub fn raise_gsi(&mut self, gsi: u32) {
        let Some(interrupts) = &self.interrupts else {
            return;
        };
        interrupts.borrow_mut().raise_irq(InterruptInput::Gsi(gsi));
    }

    /// Debug/testing helper: deassert ("lower") a platform GSI (Global System Interrupt) input
    /// line.
    ///
    /// See [`Machine::raise_gsi`]. When the PC platform is disabled, this is a no-op.
    pub fn lower_gsi(&mut self, gsi: u32) {
        let Some(interrupts) = &self.interrupts else {
            return;
        };
        interrupts.borrow_mut().lower_irq(InterruptInput::Gsi(gsi));
    }

    /// Debug/testing helper: assert ("raise") an ISA IRQ input line (0-15).
    ///
    /// This forwards to [`PlatformInterrupts::raise_irq`] when the PC platform is enabled;
    /// otherwise it is a no-op.
    pub fn raise_isa_irq(&mut self, irq: u8) {
        let Some(interrupts) = &self.interrupts else {
            return;
        };
        interrupts
            .borrow_mut()
            .raise_irq(InterruptInput::IsaIrq(irq));
    }

    /// Debug/testing helper: deassert ("lower") an ISA IRQ input line (0-15).
    ///
    /// This forwards to [`PlatformInterrupts::lower_irq`] when the PC platform is enabled;
    /// otherwise it is a no-op.
    pub fn lower_isa_irq(&mut self, irq: u8) {
        let Some(interrupts) = &self.interrupts else {
            return;
        };
        interrupts
            .borrow_mut()
            .lower_irq(InterruptInput::IsaIrq(irq));
    }

    /// Debug/testing helper: write a single guest physical byte.
    pub fn write_physical_u8(&mut self, paddr: u64, value: u8) {
        self.mem.write_u8(paddr, value);
    }

    /// Debug/testing helper: write a little-endian u16 to guest physical memory.
    pub fn write_physical_u16(&mut self, paddr: u64, value: u16) {
        self.mem.write_u16(paddr, value);
    }

    /// Debug/testing helper: write a little-endian u32 to guest physical memory.
    pub fn write_physical_u32(&mut self, paddr: u64, value: u32) {
        self.mem.write_u32(paddr, value);
    }

    /// Debug/testing helper: write a little-endian u64 to guest physical memory.
    pub fn write_physical_u64(&mut self, paddr: u64, value: u64) {
        self.mem.write_u64(paddr, value);
    }

    /// Debug/testing helper: write a slice into guest physical memory.
    pub fn write_physical(&mut self, paddr: u64, data: &[u8]) {
        self.mem.write_physical(paddr, data);
    }

    /// Debug/testing helper: read from an I/O port.
    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        self.io.read(port, size)
    }

    /// Debug/testing helper: write to an I/O port.
    pub fn io_write(&mut self, port: u16, size: u8, value: u32) {
        self.io.write(port, size, value);
    }

    // ---------------------------------------------------------------------
    // Host-facing display API (VGA/VBE scanout)
    // ---------------------------------------------------------------------

    /// Returns the currently active scanout source according to the scanout handoff policy.
    ///
    /// Policy summary:
    /// - Before the guest claims the AeroGPU WDDM scanout, legacy VGA/VBE output is presented.
    /// - Once the guest claims WDDM scanout (writes a valid `SCANOUT0_*` config and enables it),
    ///   WDDM owns scanout until reset.
    ///   While WDDM owns scanout, legacy VGA/VBE is ignored by presentation even if legacy MMIO/PIO
    ///   continues to accept writes for compatibility.
    /// - Clearing `SCANOUT0_ENABLE` disables scanout (blanking), but does not release WDDM
    ///   ownership; the host presents a blank frame (0x0 resolution) rather than falling back to
    ///   legacy.
    /// - Device/VM reset reverts scanout ownership back to legacy.
    pub fn active_scanout_source(&self) -> ScanoutSource {
        if let Some(aerogpu_mmio) = &self.aerogpu_mmio {
            let state = aerogpu_mmio.borrow().scanout0_state();
            // Once the guest has claimed WDDM scanout, treat it as the authoritative scanout
            // source until VM reset. `SCANOUT0_ENABLE=0` is a visibility toggle (blanking) and does
            // not release ownership back to legacy VGA/VBE.
            if state.wddm_scanout_active {
                return ScanoutSource::Wddm;
            }
        }

        if let Some(vga) = &self.vga {
            let vga = vga.borrow();
            if (vga.vbe.enable & 0x0001) != 0 {
                return ScanoutSource::LegacyVbe;
            }

            // The VGA device model can be placed into mode 13h (320x200x256) via BIOS INT 10h or by
            // software directly programming VGA regs. The machine does not track the full VGA
            // state machine here; instead, use the BIOS-cached mode value which is updated when the
            // guest uses INT 10h AH=00h to set a classic VGA mode.
            if (self.bios.cached_video_mode() & 0x7F) == 0x13 {
                return ScanoutSource::LegacyVga;
            }

            ScanoutSource::LegacyText
        } else if self.cfg.enable_aerogpu {
            // When AeroGPU is enabled (and standalone VGA is disabled), legacy VBE state is tracked
            // in the BIOS HLE layer. Guests may also program the Bochs VBE_DISPI registers directly
            // (0x01CE/0x01CF), so also treat that as an active legacy VBE scanout.
            let bochs_vbe = self
                .aerogpu
                .as_ref()
                .is_some_and(|dev| dev.borrow().vbe_dispi_enabled());

            if self.bios.video.vbe.current_mode.is_some() || bochs_vbe {
                ScanoutSource::LegacyVbe
            } else if self.bios.cached_video_mode() == 0x13 {
                ScanoutSource::LegacyVga
            } else {
                ScanoutSource::LegacyText
            }
        } else {
            // No VGA attached; still report the legacy category for lack of a better option.
            ScanoutSource::LegacyText
        }
    }

    /// Re-render the emulated display into the machine's host-visible framebuffer cache.
    ///
    /// When the standalone VGA/VBE device model is disabled and `enable_aerogpu=true`, this
    /// presents (in priority order):
    ///
    /// - the guest-programmed AeroGPU scanout (WDDM scanout0), if claimed, or
    /// - the active VBE linear framebuffer (if a VBE mode is set), or
    /// - VGA mode 13h (320x200x256) if selected via BIOS INT 10h, or
    /// - BIOS/boot text mode output by rendering the legacy text buffer at `0xB8000` using BIOS
    ///   Data Area (BDA) state for active page selection and cursor overlay.
    ///
    /// Otherwise (no VGA, no AeroGPU fallback), this clears the cached framebuffer and returns
    /// `(0, 0)` resolution.
    pub fn display_present(&mut self) {
        if let Some(vga) = &self.vga {
            let mut vga = vga.borrow_mut();
            vga.present();
            let (w, h) = vga.get_resolution();
            let fb = vga.get_framebuffer();

            self.display_width = w;
            self.display_height = h;
            self.display_fb.resize(fb.len(), 0);
            self.display_fb.copy_from_slice(fb);
            return;
        }

        if self.cfg.enable_aerogpu {
            // Once the guest has claimed WDDM scanout, prefer presenting that framebuffer over the
            // VBE/text fallbacks. Disabling scanout (`ENABLE=0`) blanks output but does not release
            // WDDM ownership back to legacy VGA/VBE.
            if self.display_present_aerogpu_scanout() {
                return;
            }
            if self.display_present_aerogpu_vbe_lfb() {
                return;
            }
            if self.display_present_aerogpu_mode13h() {
                return;
            }
            self.display_present_aerogpu_text_mode();
            return;
        }

        self.display_fb.clear();
        self.display_width = 0;
        self.display_height = 0;
    }

    fn display_present_aerogpu_vbe_lfb(&mut self) -> bool {
        let (width, height, bpp, bytes_per_pixel, pitch, start_x, start_y) =
            if let Some(mode_id) = self.bios.video.vbe.current_mode {
                // BIOS-driven VBE mode.
                let Some(mode) = self.bios.video.vbe.find_mode(mode_id) else {
                    return false;
                };

                let width = u32::from(mode.width);
                let height = u32::from(mode.height);
                if width == 0 || height == 0 {
                    return false;
                }

                let bytes_per_pixel = usize::from(mode.bytes_per_pixel()).max(1);
                if bytes_per_pixel > 8 {
                    return false;
                }

                let pitch = u64::from(
                    self.bios
                        .video
                        .vbe
                        .bytes_per_scan_line
                        .max(mode.bytes_per_scan_line()),
                );
                if pitch == 0 {
                    return false;
                }

                let start_x = u64::from(self.bios.video.vbe.display_start_x);
                let start_y = u64::from(self.bios.video.vbe.display_start_y);

                (
                    width,
                    height,
                    u16::from(mode.bpp),
                    bytes_per_pixel,
                    pitch,
                    start_x,
                    start_y,
                )
            } else {
                // Guest-driven Bochs VBE_DISPI mode.
                let Some(aerogpu) = self.aerogpu.as_ref() else {
                    return false;
                };
                let dev = aerogpu.borrow();
                if !dev.vbe_dispi_enabled() {
                    return false;
                }

                let width = u32::from(dev.vbe_dispi_xres);
                let height = u32::from(dev.vbe_dispi_yres);
                if width == 0 || height == 0 {
                    return false;
                }

                let bpp = dev.vbe_dispi_bpp;
                let bytes_per_pixel = (bpp as usize).div_ceil(8).max(1);
                if bytes_per_pixel > 8 {
                    return false;
                }

                let pitch_pixels = if dev.vbe_dispi_virt_width != 0 {
                    dev.vbe_dispi_virt_width
                } else {
                    dev.vbe_dispi_xres
                };
                let pitch = u64::from(pitch_pixels).saturating_mul(bytes_per_pixel as u64);
                if pitch == 0 {
                    return false;
                }

                let start_x = u64::from(dev.vbe_dispi_x_offset);
                let start_y = u64::from(dev.vbe_dispi_y_offset);
                (width, height, bpp, bytes_per_pixel, pitch, start_x, start_y)
            };

        let base = u64::from(self.bios.video.vbe.lfb_base)
            .saturating_add(start_y.saturating_mul(pitch))
            .saturating_add(start_x.saturating_mul(bytes_per_pixel as u64));

        self.display_width = width;
        self.display_height = height;
        self.display_fb
            .resize(width.saturating_mul(height) as usize, 0);

        let width_usize = usize::try_from(width).unwrap_or(0);
        let height_usize = usize::try_from(height).unwrap_or(0);
        let pitch_usize = usize::try_from(pitch).unwrap_or(0);
        let row_bytes = match width_usize.checked_mul(bytes_per_pixel) {
            Some(n) if n != 0 => n,
            _ => return false,
        };

        // Fast-path: if the VBE LFB base falls within AeroGPU BAR1, read directly from the
        // device's `Vec<u8>` VRAM backing store rather than routing through the PCI MMIO router.
        //
        // This avoids millions of tiny MMIO read operations when presenting a large scanout.
        let vram_fast = if pitch_usize != 0 && pitch_usize >= row_bytes {
            match (self.aerogpu.clone(), self.aerogpu_bar1_base()) {
                (Some(aerogpu), Some(bar1_base)) => {
                    let bar1_end =
                        bar1_base.saturating_add(aero_devices::pci::profile::AEROGPU_VRAM_SIZE);
                    if base < bar1_base || base >= bar1_end {
                        None
                    } else {
                        let vram_off_u64 = base - bar1_base;
                        let Ok(vram_off) = usize::try_from(vram_off_u64) else {
                            return false;
                        };
                        let Some(buf_len) = pitch_usize.checked_mul(height_usize) else {
                            return false;
                        };
                        let Some(end) = vram_off.checked_add(buf_len) else {
                            return false;
                        };
                        let vram_len = aerogpu.borrow().vram.len();
                        if end <= vram_len {
                            Some((aerogpu, vram_off, pitch_usize))
                        } else {
                            None
                        }
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        match bpp {
            32 => {
                if let Some((aerogpu, vram_off, pitch_bytes)) = vram_fast.as_ref() {
                    let dev = aerogpu.borrow();
                    let vram = &dev.vram;
                    for y in 0..height_usize {
                        let src_row_off = vram_off.saturating_add(y.saturating_mul(*pitch_bytes));
                        let src_row = &vram[src_row_off..src_row_off.saturating_add(row_bytes)];
                        let dst_row = &mut self.display_fb[y * width_usize..(y + 1) * width_usize];
                        for (src_px, dst) in src_row.chunks_exact(4).zip(dst_row.iter_mut()) {
                            let b = src_px[0];
                            let g = src_px[1];
                            let r = src_px[2];
                            *dst = u32::from_le_bytes([r, g, b, 0xFF]);
                        }
                    }
                } else {
                    // Render row-by-row to avoid allocating large intermediate buffers (and to keep
                    // the MMIO read path incremental for BAR-backed apertures).
                    let mut row = vec![0u8; row_bytes];
                    for y in 0..height_usize {
                        let row_addr = base.saturating_add((y as u64).saturating_mul(pitch));
                        self.mem.read_physical(row_addr, &mut row);
                        let dst_row = &mut self.display_fb[y * width_usize..(y + 1) * width_usize];
                        for (src_px, dst) in row.chunks_exact(4).zip(dst_row.iter_mut()) {
                            let b = src_px[0];
                            let g = src_px[1];
                            let r = src_px[2];
                            *dst = u32::from_le_bytes([r, g, b, 0xFF]);
                        }
                    }
                }
                true
            }
            8 => {
                // In 8bpp VBE modes, the framebuffer is a stream of palette indices and guests
                // commonly program colors via the VGA DAC ports (`0x3C8/0x3C9`).
                //
                // Prefer the AeroGPU-emulated DAC palette so port writes affect visible output.
                // The BIOS VBE palette is mirrored into this DAC on reset and when the guest uses
                // INT 10h AX=4F09 "Set Palette Data" (see `handle_bios_interrupt`).
                let (pal, pel_mask) = self
                    .aerogpu
                    .as_ref()
                    .map(|dev| {
                        let dev = dev.borrow();
                        (dev.dac_palette, dev.pel_mask)
                    })
                    .unwrap_or(([[0u8; 3]; 256], 0xFF));
                let scale_6bit_to_8bit = |c: u8| -> u8 { (c << 2) | (c >> 4) };

                // Precompute a lookup table for each possible 8-bit pixel value (after applying
                // PEL mask).
                let mut lut = [0u32; 256];
                for (idx, out) in lut.iter_mut().enumerate() {
                    let pal_idx = (idx as u8 & pel_mask) as usize;
                    let [r6, g6, b6] = pal[pal_idx];
                    let b = scale_6bit_to_8bit(b6);
                    let g = scale_6bit_to_8bit(g6);
                    let r = scale_6bit_to_8bit(r6);
                    *out = 0xFF00_0000 | (u32::from(b) << 16) | (u32::from(g) << 8) | u32::from(r);
                }

                if let Some((aerogpu, vram_off, pitch_bytes)) = vram_fast.as_ref() {
                    let dev = aerogpu.borrow();
                    let vram = &dev.vram;
                    for y in 0..height_usize {
                        let src_row_off = vram_off.saturating_add(y.saturating_mul(*pitch_bytes));
                        let src_row = &vram[src_row_off..src_row_off.saturating_add(row_bytes)];
                        let dst_row = &mut self.display_fb[y * width_usize..(y + 1) * width_usize];
                        for (src, dst) in src_row.iter().zip(dst_row.iter_mut()) {
                            *dst = lut[*src as usize];
                        }
                    }
                } else {
                    // Render row-by-row to avoid allocating large intermediate buffers (and to keep
                    // the MMIO read path incremental for BAR-backed apertures).
                    let mut row = vec![0u8; row_bytes];
                    for y in 0..height_usize {
                        let row_addr = base.saturating_add((y as u64).saturating_mul(pitch));
                        self.mem.read_physical(row_addr, &mut row);
                        let dst_row = &mut self.display_fb[y * width_usize..(y + 1) * width_usize];
                        for (src, dst) in row.iter().zip(dst_row.iter_mut()) {
                            *dst = lut[*src as usize];
                        }
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn display_present_aerogpu_mode13h(&mut self) -> bool {
        // VGA mode 13h (320x200x256) is exposed via the HLE BIOS (INT 10h AH=00h, AL=0x13).
        //
        // When AeroGPU is enabled and the standalone VGA device is disabled, we still want to
        // present this legacy graphics mode for bootloaders / DOS-style guests that use it.
        if self.bios.video.vbe.current_mode.is_some() {
            return false;
        }

        // Many BIOSes treat bit 7 as a "no clear" flag, so mask it off when checking the mode.
        if (self.bios.cached_video_mode() & 0x7F) != 0x13 {
            return false;
        }

        let Some(aerogpu) = self.aerogpu.clone() else {
            return false;
        };

        const WIDTH: usize = 320;
        const HEIGHT: usize = 200;
        const PIXELS: usize = WIDTH * HEIGHT;
        const WINDOW_BYTES: usize = 64 * 1024;

        let (pal, pel_mask) = {
            let dev = aerogpu.borrow();
            (dev.dac_palette, dev.pel_mask)
        };

        let scale_6bit_to_8bit = |c: u8| -> u8 { (c << 2) | (c >> 4) };

        // Precompute a lookup table for each possible 8-bit pixel value (after applying PEL mask).
        let mut lut = [0u32; 256];
        for (idx, out) in lut.iter_mut().enumerate() {
            let pal_idx = (idx as u8 & pel_mask) as usize;
            let [r6, g6, b6] = pal[pal_idx];
            let b = scale_6bit_to_8bit(b6);
            let g = scale_6bit_to_8bit(g6);
            let r = scale_6bit_to_8bit(r6);
            *out = 0xFF00_0000 | (u32::from(b) << 16) | (u32::from(g) << 8) | u32::from(r);
        }

        self.display_width = WIDTH as u32;
        self.display_height = HEIGHT as u32;
        self.display_fb.resize(PIXELS, 0);

        // VGA mode 13h uses a simple linear 64KiB window at A0000. Under AeroGPU this is aliased
        // into the start of BAR1-backed VRAM (see `AeroGpuDevice::legacy_vga_read_u8`).
        let dev = aerogpu.borrow();
        if dev.vram.len() < WINDOW_BYTES {
            return false;
        }

        // Respect VGA CRTC start address + byte mode semantics so mode13h panning works.
        let start_word = (usize::from(dev.crtc_regs[0x0C]) << 8) | usize::from(dev.crtc_regs[0x0D]);
        let byte_mode = (dev.crtc_regs[0x17] & 0x40) != 0;
        let start = if byte_mode {
            start_word
        } else {
            start_word << 1
        } & 0xFFFF;

        for (linear, dst) in self.display_fb.iter_mut().enumerate() {
            let addr = start.wrapping_add(linear) & 0xFFFF;
            let idx = dev.vram[addr];
            *dst = lut[idx as usize];
        }

        true
    }

    fn display_present_aerogpu_scanout(&mut self) -> bool {
        let (Some(aerogpu), Some(pci_cfg)) = (&self.aerogpu_mmio, &self.pci_cfg) else {
            return false;
        };

        // Snapshot scanout/cursor state without holding the borrow across guest memory reads (the
        // scanout or cursor bitmaps may live in BAR1 VRAM, which re-enters the PCI MMIO router).
        let (state, cursor) = {
            let dev = aerogpu.borrow();
            (dev.scanout0_state(), dev.cursor_snapshot())
        };

        if !state.wddm_scanout_active {
            return false;
        }

        // Once WDDM scanout has been claimed, do not fall back to the BIOS VBE/text paths until
        // reset.
        //
        // The Win7 AeroGPU driver uses `SCANOUT0_ENABLE` as a visibility toggle
        // (`DxgkDdiSetVidPnSourceVisibility`). When disabled, render a blank frame but keep the
        // WDDM ownership latch held so legacy VGA/VBE cannot steal scanout back.
        if !state.enable {
            self.display_fb.clear();
            self.display_width = 0;
            self.display_height = 0;
            return true;
        }
        // Gate device-initiated scanout reads on PCI COMMAND.BME.
        let command = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(aero_devices::pci::profile::AEROGPU.bdf)
                .map(|cfg| cfg.command())
                .unwrap_or(0)
        };
        if (command & (1 << 2)) == 0 {
            self.display_fb.clear();
            self.display_width = 0;
            self.display_height = 0;
            return true;
        }

        // -----------------------------------------------------------------
        // BAR1 VRAM readback fast-path
        // -----------------------------------------------------------------
        //
        // If the guest points scanout/cursor buffers into the AeroGPU BAR1 VRAM aperture, routing
        // those reads through the physical `MemoryBus` causes the PCI MMIO router to bounce back
        // into the AeroGPU BAR1 handler for every row read.
        //
        // Detect BAR1-backed surfaces and read directly from `AeroGpuDevice.vram` instead.
        let bar1_base = self.aerogpu_bar1_base();
        let bar1_end = bar1_base
            .and_then(|base| base.checked_add(aero_devices::pci::profile::AEROGPU_VRAM_SIZE));

        let scanout_row_bytes = || -> Option<u64> {
            let bytes_per_pixel = u64::try_from(
                aero_devices_gpu::AeroGpuFormat::from_u32(state.format).bytes_per_pixel()?,
            )
            .ok()?;
            u64::from(state.width).checked_mul(bytes_per_pixel)
        };

        let scanout_is_bar1_backed = || -> bool {
            let (Some(bar1_base), Some(bar1_end)) = (bar1_base, bar1_end) else {
                return false;
            };
            let Some(row_bytes) = scanout_row_bytes() else {
                return false;
            };
            if state.fb_gpa < bar1_base || state.fb_gpa >= bar1_end {
                return false;
            }
            let pitch = u64::from(state.pitch_bytes);
            if pitch == 0 {
                return false;
            }
            if pitch < row_bytes {
                return false;
            }
            let height = u64::from(state.height);
            if height == 0 {
                return false;
            }
            let Some(last_row) = height.checked_sub(1).and_then(|h| h.checked_mul(pitch)) else {
                return false;
            };
            let Some(last_row_gpa) = state.fb_gpa.checked_add(last_row) else {
                return false;
            };
            let Some(end_gpa) = last_row_gpa.checked_add(row_bytes) else {
                return false;
            };
            end_gpa <= bar1_end
        };

        let cursor_is_bar1_backed = || -> bool {
            if !cursor.enable {
                return false;
            }
            let (Some(bar1_base), Some(bar1_end)) = (bar1_base, bar1_end) else {
                return false;
            };
            if cursor.fb_gpa < bar1_base || cursor.fb_gpa >= bar1_end {
                return false;
            }
            let width = u64::from(cursor.width);
            let height = u64::from(cursor.height);
            if width == 0 || height == 0 {
                return false;
            }

            if aero_devices_gpu::AeroGpuFormat::from_u32(cursor.format).bytes_per_pixel() != Some(4)
            {
                return false;
            }

            let Some(row_bytes) = width.checked_mul(4) else {
                return false;
            };
            let pitch = u64::from(cursor.pitch_bytes);
            if pitch == 0 {
                return false;
            }
            if pitch < row_bytes {
                return false;
            }
            let Some(last_row) = height.checked_sub(1).and_then(|h| h.checked_mul(pitch)) else {
                return false;
            };
            let Some(last_row_gpa) = cursor.fb_gpa.checked_add(last_row) else {
                return false;
            };
            let Some(end_gpa) = last_row_gpa.checked_add(row_bytes) else {
                return false;
            };
            end_gpa <= bar1_end
        };

        let scanout_in_vram = scanout_is_bar1_backed() && self.aerogpu.is_some();
        let cursor_in_vram = cursor_is_bar1_backed() && self.aerogpu.is_some();

        let Some(mut fb) = (if scanout_in_vram {
            // Avoid borrowing `self.mem` while holding the AeroGPU VRAM borrow.
            let aerogpu = self.aerogpu.as_ref().expect("checked above");
            let dev = aerogpu.borrow();
            let mut vram_bus = AeroGpuBar1VramReadbackBus::new(&dev.vram, bar1_base.unwrap_or(0));
            state.read_rgba8888(&mut vram_bus)
        } else {
            state.read_rgba8888(&mut self.mem)
        }) else {
            self.display_fb.clear();
            self.display_width = 0;
            self.display_height = 0;
            return true;
        };

        if cursor.enable {
            let cursor_fb = if cursor_in_vram {
                let aerogpu = self.aerogpu.as_ref().expect("checked above");
                let dev = aerogpu.borrow();
                let mut vram_bus =
                    AeroGpuBar1VramReadbackBus::new(&dev.vram, bar1_base.unwrap_or(0));
                cursor.read_rgba8888(&mut vram_bus)
            } else {
                cursor.read_rgba8888(&mut self.mem)
            };

            if let Some(cursor_fb) = cursor_fb {
                aerogpu::composite_cursor_rgba8888_over_scanout(
                    &mut fb,
                    state.width,
                    state.height,
                    &cursor,
                    &cursor_fb,
                );
            }
        }

        self.display_width = state.width;
        self.display_height = state.height;
        self.display_fb = fb;
        true
    }

    fn display_present_aerogpu_text_mode(&mut self) {
        // Avoid holding a `RefCell` borrow of the AeroGPU device while reading from guest memory:
        // legacy VRAM is MMIO-routed back into the same device and may borrow it again.
        let (pel_mask, dac_palette, attr_regs) = if let Some(aerogpu) = &self.aerogpu {
            let dev = aerogpu.borrow();
            (dev.pel_mask, dev.dac_palette, dev.attr_regs)
        } else {
            (
                0xFF,
                AeroGpuDevice::default_dac_palette(),
                AeroGpuDevice::default_attr_regs(),
            )
        };

        let (w, h) = aerogpu_legacy_text::render_into(
            &mut self.display_fb,
            &mut self.mem,
            &dac_palette,
            pel_mask,
            &attr_regs,
        );
        self.display_width = w;
        self.display_height = h;
    }

    /// Return the last framebuffer produced by [`Machine::display_present`].
    pub fn display_framebuffer(&self) -> &[u32] {
        &self.display_fb
    }

    /// Return the last resolution produced by [`Machine::display_present`].
    pub fn display_resolution(&self) -> (u32, u32) {
        (self.display_width, self.display_height)
    }

    /// Return the physical base address of the VBE linear framebuffer (LFB) as reported by the
    /// machine's firmware (VBE mode info `PhysBasePtr`).
    ///
    /// This is the canonical address guests are expected to use when mapping the VBE framebuffer.
    ///
    /// Note: When VGA is disabled, the firmware keeps the LFB inside conventional RAM so BIOS-only
    /// helpers do not scribble over the canonical PCI MMIO window.
    pub fn vbe_lfb_base(&self) -> u64 {
        u64::from(self.bios.video.vbe.lfb_base)
    }

    /// Return the BIOS-reported VBE linear framebuffer (LFB) base address as a raw `u32`.
    ///
    /// This is the value reported via `INT 10h AX=4F01h` (`VBE ModeInfoBlock.PhysBasePtr`).
    pub fn vbe_lfb_base_u32(&self) -> u32 {
        self.bios.video.vbe.lfb_base
    }
    /// Install an external scanout descriptor that should receive legacy VGA/VBE mode updates.
    ///
    /// When present, BIOS INT 10h mode transitions publish updates to this descriptor so an
    /// external presentation layer (e.g. browser canvas) can follow VGA text vs VBE LFB scanout.
    ///
    /// On single-threaded wasm builds (no atomic support), scanout state publishing is unavailable
    /// and this method is not compiled.
    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    pub fn set_scanout_state(&mut self, state: Option<Arc<ScanoutState>>) {
        self.scanout_state = state.map(SharedStateHandle::Arc);
    }

    /// Install an external scanout descriptor backed by a `'static` reference.
    ///
    /// This exists for the threaded wasm build, where the scanout state header is embedded inside
    /// the shared wasm linear memory.
    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    pub fn set_scanout_state_static(&mut self, state: Option<&'static ScanoutState>) {
        self.scanout_state = state.map(SharedStateHandle::Static);
    }

    /// Install an external hardware cursor descriptor that should receive AeroGPU cursor updates.
    ///
    /// When present, AeroGPU BAR0 cursor register updates publish updates to this descriptor so an
    /// external presentation layer (e.g. browser canvas) can render the hardware cursor without
    /// legacy postMessage plumbing.
    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    pub fn set_cursor_state(&mut self, state: Option<Arc<CursorState>>) {
        self.cursor_state = state.map(SharedStateHandle::Arc);
    }

    /// Install an external hardware cursor descriptor backed by a `'static` reference.
    ///
    /// This exists for the threaded wasm build, where the cursor state header is embedded inside
    /// the shared wasm linear memory.
    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    pub fn set_cursor_state_static(&mut self, state: Option<&'static CursorState>) {
        self.cursor_state = state.map(SharedStateHandle::Static);
    }
    /// Debug/testing helper: returns the number of MMIO reads routed through AeroGPU BAR1 (VRAM),
    /// if AeroGPU is enabled.
    ///
    /// This is primarily intended for performance regression tests to ensure scanout presentation
    /// uses a direct VRAM read fast-path rather than dispatching millions of small MMIO reads
    /// through the PCI router.
    pub fn aerogpu_bar1_mmio_read_count(&self) -> Option<u64> {
        Some(self.aerogpu.as_ref()?.borrow().vram_mmio_read_count())
    }

    /// Returns the shared manual clock backing platform timer devices, if the PC platform is
    /// enabled.
    pub fn platform_clock(&self) -> Option<ManualClock> {
        self.platform_clock.clone()
    }

    /// Returns the platform interrupt controller complex (PIC + IOAPIC + LAPIC), if present.
    pub fn platform_interrupts(&self) -> Option<Rc<RefCell<PlatformInterrupts>>> {
        self.interrupts.clone()
    }

    /// Returns the PCI config mechanism #1 ports device, if present.
    pub fn pci_config_ports(&self) -> Option<SharedPciConfigPorts> {
        self.pci_cfg.clone()
    }

    /// Returns the base address assigned to a device's PCI BAR.
    ///
    /// This consults the device's PCI config space (as exposed via [`Machine::pci_config_ports`]).
    /// It returns `None` if the PC platform is disabled, the device function is missing, or the
    /// BAR index is not implemented by that device.
    pub fn pci_bar_base(&self, bdf: PciBdf, bar: u8) -> Option<u64> {
        let pci_cfg = self.pci_config_ports()?;
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg.bus_mut().device_config(bdf)?;
        Some(cfg.bar_range(bar)?.base)
    }

    /// Returns the canonical AeroGPU PCI function BDF if the device is present.
    ///
    /// The canonical AeroGPU identity contract reserves `00:07.0` for
    /// `VID:DID = A3A0:0001` (see `docs/abi/aerogpu-pci-identity.md`). This helper allows
    /// integration tests and wasm glue code to detect whether an AeroGPU device model is attached
    /// without reaching into machine internals.
    pub fn aerogpu(&self) -> Option<PciBdf> {
        let pci_cfg = self.pci_config_ports()?;
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        let bdf = aero_devices::pci::profile::AEROGPU.bdf;
        let vendor = bus.read_config(bdf, 0x00, 2) as u16;
        if vendor == 0xFFFF {
            return None;
        }
        let device = bus.read_config(bdf, 0x02, 2) as u16;
        (vendor == aero_devices::pci::profile::PCI_VENDOR_ID_AERO
            && device == aero_devices::pci::profile::PCI_DEVICE_ID_AERO_AEROGPU)
            .then_some(bdf)
    }

    /// Install/replace the host-side AeroGPU command backend.
    ///
    /// This backend receives submissions from the BAR0 ring (doorbells) and is responsible for
    /// reporting fence completions back to the device model.
    ///
    /// Behavior:
    /// - The selected backend is preserved across [`Machine::reset`] calls.
    /// - Swapping the backend drops any in-flight fence tracking inside the device model; callers
    ///   should install the backend before the guest submits work (typically immediately after
    ///   [`Machine::new`] or after a reset).
    pub fn aerogpu_set_backend(
        &mut self,
        backend: Box<dyn AeroGpuCommandBackend>,
    ) -> Result<(), MachineError> {
        let Some(dev) = &self.aerogpu_mmio else {
            return Err(MachineError::AeroGpuNotEnabled);
        };
        dev.borrow_mut().set_backend(backend);
        Ok(())
    }

    /// Install the "immediate" AeroGPU backend (headless-friendly).
    ///
    /// The immediate backend completes fences synchronously and performs no rendering. This is
    /// safe to call even when AeroGPU is not enabled/present; it will no-op.
    pub fn aerogpu_set_backend_immediate(&mut self) {
        let Some(mmio) = &self.aerogpu_mmio else {
            return;
        };
        mmio.borrow_mut().set_backend(Box::new(
            aero_devices_gpu::backend::ImmediateAeroGpuBackend::new(),
        ));
    }

    /// Install the "null" AeroGPU backend (drops all submissions).
    ///
    /// The null backend never completes fences (guests will observe stuck fences). This is safe to
    /// call even when AeroGPU is not enabled/present; it will no-op.
    pub fn aerogpu_set_backend_null(&mut self) {
        let Some(mmio) = &self.aerogpu_mmio else {
            return;
        };
        mmio.borrow_mut().set_backend(Box::new(
            aero_devices_gpu::backend::NullAeroGpuBackend::new(),
        ));
    }

    /// Install the native wgpu-based AeroGPU backend (headless) if available.
    ///
    /// This is feature-gated because the native backend is heavier (wgpu + renderer) than the
    /// lightweight default `aero-machine` build.
    ///
    /// Safe to call even when AeroGPU is not enabled/present; it will no-op.
    #[cfg(all(feature = "aerogpu-wgpu-backend", not(target_arch = "wasm32")))]
    pub fn aerogpu_set_backend_wgpu(&mut self) -> Result<(), String> {
        let Some(mmio) = &self.aerogpu_mmio else {
            return Ok(());
        };
        let backend = aero_devices_gpu::backend::NativeAeroGpuBackend::new_headless()
            .map_err(|err| err.to_string())?;
        mmio.borrow_mut().set_backend(Box::new(backend));
        Ok(())
    }

    /// Returns the PCI INTx router, if present.
    pub fn pci_intx_router(&self) -> Option<Rc<RefCell<PciIntxRouter>>> {
        self.pci_intx.clone()
    }

    /// Returns the PIT 8254 device, if present.
    pub fn pit(&self) -> Option<SharedPit8254> {
        self.pit.clone()
    }

    /// Returns the RTC CMOS device, if present.
    pub fn rtc(&self) -> Option<SharedRtcCmos<ManualClock, PlatformIrqLine>> {
        self.rtc.clone()
    }

    /// Returns the ACPI PM I/O device, if present.
    pub fn acpi_pm(&self) -> Option<SharedAcpiPmIo<ManualClock>> {
        self.acpi_pm.clone()
    }

    /// Host-facing helper for resuming a guest from an ACPI sleep state.
    ///
    /// This sets `PM1_STS.WAK_STS` and triggers a wake source (power button) so the guest receives
    /// an SCI when it has armed the corresponding PM1 event.
    pub fn acpi_wake(&mut self) {
        let Some(acpi_pm) = &self.acpi_pm else {
            return;
        };
        let mut pm = acpi_pm.borrow_mut();
        pm.set_wake_status();
        pm.trigger_power_button();
    }

    /// Returns the HPET device, if present.
    pub fn hpet(&self) -> Option<Rc<RefCell<hpet::Hpet<ManualClock>>>> {
        self.hpet.clone()
    }

    /// Returns the AHCI controller, if present.
    pub fn ahci(&self) -> Option<Rc<RefCell<AhciPciDevice>>> {
        self.ahci.clone()
    }

    /// Returns the NVMe controller, if present.
    pub fn nvme(&self) -> Option<Rc<RefCell<NvmePciDevice>>> {
        self.nvme.clone()
    }

    /// Returns the E1000 NIC device, if present.
    pub fn e1000(&self) -> Option<Rc<RefCell<E1000Device>>> {
        self.e1000.clone()
    }

    /// Returns the virtio-net (virtio-pci) device, if present.
    pub fn virtio_net(&self) -> Option<Rc<RefCell<VirtioPciDevice>>> {
        self.virtio_net.clone()
    }

    /// Returns the virtio-input keyboard (virtio-pci) device, if present.
    pub fn virtio_input_keyboard(&self) -> Option<Rc<RefCell<VirtioPciDevice>>> {
        self.virtio_input_keyboard.clone()
    }

    /// Returns the virtio-input mouse (virtio-pci) device, if present.
    pub fn virtio_input_mouse(&self) -> Option<Rc<RefCell<VirtioPciDevice>>> {
        self.virtio_input_mouse.clone()
    }
    /// Returns the VGA/SVGA device, if present.
    pub fn vga(&self) -> Option<Rc<RefCell<VgaDevice>>> {
        self.vga.clone()
    }

    /// Returns the AeroGPU BAR0 MMIO device model (register block), if present.
    pub fn aerogpu_mmio(&self) -> Option<Rc<RefCell<AeroGpuMmioDevice>>> {
        self.aerogpu_mmio.clone()
    }

    /// Returns the AeroGPU BAR0 MMIO base (register block), if present.
    ///
    /// This consults the machine's canonical PCI config space (the same one exposed to the guest)
    /// and therefore reflects BIOS POST / resource allocation.
    pub fn aerogpu_bar0_base(&self) -> Option<u64> {
        let bdf = self.aerogpu()?;
        let base = self.pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR0_INDEX)?;
        (base != 0).then_some(base)
    }

    /// Returns the AeroGPU VRAM BAR base (BAR1) if present in the active PCI profile.
    ///
    /// Note: BAR1 is not currently used by the `aero-machine` AeroGPU stub, but exposing it allows
    /// integration tests and runtimes to discover the configured VRAM aperture once the profile is
    /// extended.
    pub fn aerogpu_vram_bar_base(&self) -> Option<u64> {
        let bdf = self.aerogpu()?;
        let base = self.pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)?;
        (base != 0).then_some(base)
    }

    /// Returns the PIIX3-compatible IDE controller, if present.
    pub fn ide(&self) -> Option<Rc<RefCell<Piix3IdePciDevice>>> {
        self.ide.clone()
    }
    /// Returns the virtio-blk controller, if present.
    pub fn virtio_blk(&self) -> Option<Rc<RefCell<VirtioPciDevice>>> {
        self.virtio_blk.clone()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn swap_virtio_blk_backend_preserving_state(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> Result<(), MachineError> {
        // `Box<dyn VirtualDisk + Send>` can be used anywhere `Box<dyn VirtualDisk>` is expected
        // (dropping the `Send` auto-trait), which lets us share the implementation between native
        // and wasm32 builds.
        self.swap_virtio_blk_backend_preserving_state_impl(disk)
    }

    #[cfg(target_arch = "wasm32")]
    fn swap_virtio_blk_backend_preserving_state(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> Result<(), MachineError> {
        self.swap_virtio_blk_backend_preserving_state_impl(disk)
    }

    fn swap_virtio_blk_backend_preserving_state_impl(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> Result<(), MachineError> {
        let Some(virtio_blk) = &self.virtio_blk else {
            return Ok(());
        };

        if !disk
            .capacity_bytes()
            .is_multiple_of(aero_storage::SECTOR_SIZE as u64)
        {
            return Err(MachineError::DiskBackend(format!(
                "virtio-blk disk capacity must be a multiple of {} bytes",
                aero_storage::SECTOR_SIZE
            )));
        }

        let (command, bar0_base) = if let Some(pci_cfg) = &self.pci_cfg {
            let bdf = aero_devices::pci::profile::VIRTIO_BLK.bdf;
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar0_base = cfg.and_then(|cfg| cfg.bar_range(0)).map(|range| range.base);
            (command, bar0_base)
        } else {
            (0, None)
        };

        // Ensure the device model's internal PCI config view is coherent with the canonical PCI
        // config space before capturing state to preserve.
        {
            let mut dev = virtio_blk.borrow_mut();
            dev.set_pci_command(command);
            if let Some(bar0_base) = bar0_base {
                dev.config_mut().set_bar_base(0, bar0_base);
            }
        }

        // Preserve the device model's in-flight state while swapping the disk backend.
        let state = virtio_blk.borrow().save_state();
        let interrupt_sink: Box<dyn VirtioInterruptSink> = match &self.interrupts {
            Some(ints) => Box::new(VirtioMsixInterruptSink::new(ints.clone())),
            None => Box::new(NoopVirtioInterruptSink),
        };
        let mut new_dev = VirtioPciDevice::new(Box::new(VirtioBlk::new(disk)), interrupt_sink);
        new_dev.load_state(&state).map_err(|e| {
            MachineError::DiskBackend(format!(
                "failed to restore virtio-blk state after attaching disk backend: {e}"
            ))
        })?;
        *virtio_blk.borrow_mut() = new_dev;

        // Keep the device model's internal PCI config view coherent with the canonical PCI config
        // space. This ensures `save_state()` sees consistent BAR programming without requiring an
        // immediate `reset()` cycle.
        {
            let mut dev = virtio_blk.borrow_mut();
            dev.set_pci_command(command);
            if let Some(bar0_base) = bar0_base {
                dev.config_mut().set_bar_base(0, bar0_base);
            }
        }

        Ok(())
    }

    /// Attach a disk backend to the virtio-blk controller, if present.
    ///
    /// Virtio-blk accepts an [`aero_storage::VirtualDisk`] backend. On `wasm32`, this intentionally
    /// does **not** require `Send` because some browser-backed disks (OPFS, etc.) may contain JS
    /// values and thus cannot be sent across threads.
    ///
    /// This method preserves the virtio-blk controller's guest-visible state (PCI config + virtio
    /// transport registers/queues) by snapshotting the current device model state and re-loading
    /// it after swapping the host disk backend. This is intended for host-managed snapshot restore
    /// flows where the disk contents live outside the snapshot blob.
    pub fn attach_virtio_blk_disk(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> Result<(), MachineError> {
        if self.virtio_blk.is_none() {
            return Ok(());
        }
        let result = self.swap_virtio_blk_backend_preserving_state(disk);
        if result.is_ok() {
            // Host explicitly attached a virtio-blk disk backend. Do not overwrite this device with
            // the machine's shared disk when `set_disk_backend`/`set_disk_image` are called.
            self.virtio_blk_auto_attach_shared_disk = false;
        }
        result
    }

    /// Returns the PIIX3-compatible UHCI (USB 1.1) controller, if present.
    pub fn uhci(&self) -> Option<Rc<RefCell<UhciPciDevice>>> {
        self.uhci.clone()
    }

    /// Returns the EHCI (USB 2.0) controller, if present.
    pub fn ehci(&self) -> Option<Rc<RefCell<EhciPciDevice>>> {
        self.ehci.clone()
    }

    /// Returns the xHCI (USB 3.x) controller, if present.
    pub fn xhci(&self) -> Option<Rc<RefCell<XhciPciDevice>>> {
        self.xhci.clone()
    }

    /// Attach a USB device model at a topology path on the xHCI root hub.
    ///
    /// Path semantics match [`aero_usb::xhci::XhciController::attach_at_path`]:
    ///
    /// - `path[0]` is the **root port index** (0-based).
    /// - `path[1..]` are **hub port numbers** (1-based) for any nested hubs.
    ///
    /// If xHCI is not enabled on this machine, this call is a no-op and returns `Ok(())`.
    pub fn usb_xhci_attach_at_path(
        &mut self,
        path: &[u8],
        dev: Box<dyn aero_usb::UsbDeviceModel>,
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        let Some(xhci) = &self.xhci else {
            return Ok(());
        };
        xhci.borrow_mut().controller_mut().attach_at_path(path, dev)
    }

    /// Detach any USB device model at a topology path on the xHCI root hub.
    ///
    /// Path semantics match [`aero_usb::xhci::XhciController::detach_at_path`]. If xHCI is not
    /// enabled on this machine, this call is a no-op and returns `Ok(())`.
    pub fn usb_xhci_detach_at_path(
        &mut self,
        path: &[u8],
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        let Some(xhci) = &self.xhci else {
            return Ok(());
        };
        xhci.borrow_mut().controller_mut().detach_at_path(path)
    }

    /// Attach a USB device model directly to an xHCI root hub port.
    ///
    /// `port` is 0-based.
    ///
    /// If xHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_xhci_attach_root(
        &mut self,
        port: u8,
        model: Box<dyn aero_usb::UsbDeviceModel>,
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        let path = [port];
        self.usb_xhci_attach_at_path(&path, model)
    }

    /// Detach any USB device model from an xHCI root hub port.
    ///
    /// `port` is 0-based.
    ///
    /// If xHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_xhci_detach_root(&mut self, port: u8) -> Result<(), aero_usb::UsbHubAttachError> {
        let path = [port];
        self.usb_xhci_detach_at_path(&path)
    }

    /// Attach a USB device model to a UHCI root hub port.
    ///
    /// `port` is 0-based (UHCI exposes two root ports: `0` and `1`).
    ///
    /// If UHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_attach_root(
        &mut self,
        port: u8,
        model: Box<dyn aero_usb::UsbDeviceModel>,
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        let path = [port];
        self.usb_attach_at_path(&path, model)
    }

    /// Detach any USB device model from a UHCI root hub port.
    ///
    /// `port` is 0-based (UHCI exposes two root ports: `0` and `1`).
    ///
    /// If UHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_detach_root(&mut self, port: u8) -> Result<(), aero_usb::UsbHubAttachError> {
        let path = [port];
        self.usb_detach_at_path(&path)
    }

    /// Attach a USB device model at a topology path rooted at the UHCI root hub.
    ///
    /// `path` is a list of ports starting at the root hub:
    /// - `path[0]`: UHCI root port index (0-based).
    /// - `path[1..]`: downstream hub port numbers (1-based, per the USB hub spec).
    ///
    /// If UHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_attach_path(
        &mut self,
        path: &[u8],
        model: Box<dyn aero_usb::UsbDeviceModel>,
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        self.usb_attach_at_path(path, model)
    }

    /// Detach any USB device model at a topology path rooted at the UHCI root hub.
    ///
    /// `path` is a list of ports starting at the root hub:
    /// - `path[0]`: UHCI root port index (0-based).
    /// - `path[1..]`: downstream hub port numbers (1-based, per the USB hub spec).
    ///
    /// If UHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_detach_path(&mut self, path: &[u8]) -> Result<(), aero_usb::UsbHubAttachError> {
        self.usb_detach_at_path(path)
    }

    /// Attach a USB device model at a topology path on the UHCI root hub.
    ///
    /// Path semantics match [`aero_usb::hub::RootHub::attach_at_path`]:
    ///
    /// - `path[0]` is the **root port index** (0-based).
    /// - `path[1..]` are **hub port numbers** (1-based) for any nested hubs.
    ///
    /// If UHCI is not enabled on this machine, this call is a no-op.
    pub fn usb_attach_at_path(
        &mut self,
        path: &[u8],
        dev: Box<dyn aero_usb::UsbDeviceModel>,
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        let Some(uhci) = &self.uhci else {
            return Ok(());
        };

        let Some((&root_port, _)) = path.split_first() else {
            return Err(aero_usb::UsbHubAttachError::InvalidPort);
        };
        // UHCI root hub has 2 ports (PORTSC1/PORTSC2). Validate early so hosts don't accidentally
        // attach to non-existent root ports and then rely on undefined behaviour.
        if root_port as usize >= 2 {
            return Err(aero_usb::UsbHubAttachError::InvalidPort);
        }

        uhci.borrow_mut()
            .controller_mut()
            .hub_mut()
            .attach_at_path(path, dev)
    }

    /// Detach any USB device model at a topology path on the UHCI root hub.
    ///
    /// Path semantics match [`aero_usb::hub::RootHub::detach_at_path`]. If UHCI is not enabled on
    /// this machine, this call is a no-op.
    pub fn usb_detach_at_path(&mut self, path: &[u8]) -> Result<(), aero_usb::UsbHubAttachError> {
        let Some(uhci) = &self.uhci else {
            return Ok(());
        };

        let Some((&root_port, _)) = path.split_first() else {
            return Err(aero_usb::UsbHubAttachError::InvalidPort);
        };
        if root_port as usize >= 2 {
            return Err(aero_usb::UsbHubAttachError::InvalidPort);
        }

        uhci.borrow_mut()
            .controller_mut()
            .hub_mut()
            .detach_at_path(path)
    }

    /// Returns the synthetic USB HID keyboard handle, if present.
    pub fn usb_hid_keyboard_handle(&self) -> Option<aero_usb::hid::UsbHidKeyboardHandle> {
        self.usb_hid_keyboard.clone()
    }

    /// Whether the synthetic USB HID keyboard is present *and configured* (`SET_CONFIGURATION != 0`).
    pub fn usb_hid_keyboard_configured(&self) -> bool {
        self.usb_hid_keyboard
            .as_ref()
            .is_some_and(|kbd| kbd.configured())
    }

    /// Returns the current HID boot keyboard LED bitmask (NumLock/CapsLock/ScrollLock/Compose/Kana)
    /// as last set by the guest OS, or 0 if the synthetic USB HID keyboard is not present.
    pub fn usb_hid_keyboard_leds(&self) -> u8 {
        self.usb_hid_keyboard
            .as_ref()
            .map(|kbd| kbd.leds())
            .unwrap_or(0)
    }

    /// Returns the synthetic USB HID mouse handle, if present.
    pub fn usb_hid_mouse_handle(&self) -> Option<aero_usb::hid::UsbHidMouseHandle> {
        self.usb_hid_mouse.clone()
    }

    /// Whether the synthetic USB HID mouse is present *and configured* (`SET_CONFIGURATION != 0`).
    pub fn usb_hid_mouse_configured(&self) -> bool {
        self.usb_hid_mouse
            .as_ref()
            .is_some_and(|mouse| mouse.configured())
    }

    /// Returns the synthetic USB HID gamepad handle, if present.
    pub fn usb_hid_gamepad_handle(&self) -> Option<aero_usb::hid::UsbHidGamepadHandle> {
        self.usb_hid_gamepad.clone()
    }

    /// Whether the synthetic USB HID gamepad is present *and configured* (`SET_CONFIGURATION != 0`).
    pub fn usb_hid_gamepad_configured(&self) -> bool {
        self.usb_hid_gamepad
            .as_ref()
            .is_some_and(|gamepad| gamepad.configured())
    }

    /// Returns the synthetic USB HID consumer-control handle, if present.
    pub fn usb_hid_consumer_control_handle(
        &self,
    ) -> Option<aero_usb::hid::UsbHidConsumerControlHandle> {
        self.usb_hid_consumer_control.clone()
    }

    /// Whether the synthetic USB HID consumer-control device is present *and configured*
    /// (`SET_CONFIGURATION != 0`).
    pub fn usb_hid_consumer_control_configured(&self) -> bool {
        self.usb_hid_consumer_control
            .as_ref()
            .is_some_and(|consumer| consumer.configured())
    }

    /// Attach a USB device model to an EHCI root hub port.
    ///
    /// `port` is 0-based (the canonical ICH9-style EHCI model exposes 6 root ports: `0..=5`).
    ///
    /// If EHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_ehci_attach_root(
        &mut self,
        port: u8,
        model: Box<dyn aero_usb::UsbDeviceModel>,
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        let path = [port];
        self.usb_ehci_attach_at_path(&path, model)
    }

    /// Detach any USB device model from an EHCI root hub port.
    ///
    /// `port` is 0-based (the canonical ICH9-style EHCI model exposes 6 root ports: `0..=5`).
    ///
    /// If EHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_ehci_detach_root(&mut self, port: u8) -> Result<(), aero_usb::UsbHubAttachError> {
        let path = [port];
        self.usb_ehci_detach_at_path(&path)
    }

    /// Attach a USB device model at a topology path rooted at the EHCI root hub.
    ///
    /// `path` is a list of ports starting at the root hub:
    /// - `path[0]`: EHCI root port index (0-based).
    /// - `path[1..]`: downstream hub port numbers (1-based, per the USB hub spec).
    ///
    /// If EHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_ehci_attach_path(
        &mut self,
        path: &[u8],
        model: Box<dyn aero_usb::UsbDeviceModel>,
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        self.usb_ehci_attach_at_path(path, model)
    }

    /// Detach any USB device model at a topology path rooted at the EHCI root hub.
    ///
    /// `path` is a list of ports starting at the root hub:
    /// - `path[0]`: EHCI root port index (0-based).
    /// - `path[1..]`: downstream hub port numbers (1-based, per the USB hub spec).
    ///
    /// If EHCI is not enabled on this machine, this is a no-op and returns `Ok(())`.
    pub fn usb_ehci_detach_path(&mut self, path: &[u8]) -> Result<(), aero_usb::UsbHubAttachError> {
        self.usb_ehci_detach_at_path(path)
    }

    /// Attach a USB device model at a topology path on the EHCI root hub.
    ///
    /// Path semantics match [`aero_usb::ehci::RootHub::attach_at_path`]:
    /// - `path[0]` is the **root port index** (0-based).
    /// - `path[1..]` are **hub port numbers** (1-based) for any nested hubs.
    ///
    /// If EHCI is not enabled on this machine, this call is a no-op.
    pub fn usb_ehci_attach_at_path(
        &mut self,
        path: &[u8],
        dev: Box<dyn aero_usb::UsbDeviceModel>,
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        let Some(ehci) = &self.ehci else {
            return Ok(());
        };

        let Some((&root_port, _)) = path.split_first() else {
            return Err(aero_usb::UsbHubAttachError::InvalidPort);
        };

        let port_count = {
            let ehci = ehci.borrow();
            ehci.controller().hub().num_ports()
        };
        if root_port as usize >= port_count {
            return Err(aero_usb::UsbHubAttachError::InvalidPort);
        }

        ehci.borrow_mut()
            .controller_mut()
            .hub_mut()
            .attach_at_path(path, dev)
    }

    /// Detach any USB device model at a topology path on the EHCI root hub.
    ///
    /// Path semantics match [`aero_usb::ehci::RootHub::detach_at_path`]. If EHCI is not enabled on
    /// this machine, this call is a no-op.
    pub fn usb_ehci_detach_at_path(
        &mut self,
        path: &[u8],
    ) -> Result<(), aero_usb::UsbHubAttachError> {
        let Some(ehci) = &self.ehci else {
            return Ok(());
        };

        let Some((&root_port, _)) = path.split_first() else {
            return Err(aero_usb::UsbHubAttachError::InvalidPort);
        };

        let port_count = {
            let ehci = ehci.borrow();
            ehci.controller().hub().num_ports()
        };
        if root_port as usize >= port_count {
            return Err(aero_usb::UsbHubAttachError::InvalidPort);
        }

        ehci.borrow_mut()
            .controller_mut()
            .hub_mut()
            .detach_at_path(path)
    }
    /// Attach an ATA drive to the canonical AHCI port 0, if the AHCI controller is enabled.
    pub fn attach_ahci_drive_port0(&mut self, drive: AtaDrive) {
        self.attach_ahci_drive(0, drive);
    }

    /// Attach an ATA drive to an AHCI port, if present.
    pub fn attach_ahci_drive(&mut self, port: usize, drive: AtaDrive) {
        let Some(ahci) = &self.ahci else {
            return;
        };
        if port == 0 {
            // The host is explicitly attaching a drive backend. Do not auto-attach/overwrite this
            // slot with the machine's `SharedDisk` on future resets or `set_disk_image()` calls.
            self.ahci_port0_auto_attach_shared_disk = false;
        }
        ahci.borrow_mut().attach_drive(port, drive);
    }

    /// Attach a disk image to the canonical AHCI port 0, if the AHCI controller is enabled.
    pub fn attach_ahci_disk_port0(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> std::io::Result<()> {
        self.attach_ahci_drive_port0(AtaDrive::new(disk)?);
        Ok(())
    }

    /// Attach a disk backend to the NVMe controller, if present.
    ///
    /// On non-`wasm32` targets, the NVMe device model requires a `Send` disk backend for thread
    /// safety.
    ///
    /// On `wasm32`, disk backends do not need to be `Send` (browser disk handles are often
    /// `!Send`).
    ///
    /// This method preserves the NVMe controller's guest-visible state (PCI config + controller
    /// registers/queues) by snapshotting the current device model state and re-loading it after
    /// swapping the host disk backend. This is intended for host-managed snapshot restore flows
    /// where the disk contents live outside the snapshot blob.
    pub fn attach_nvme_disk(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> Result<(), MachineError> {
        self.attach_nvme_disk_impl(disk)
    }

    fn attach_nvme_disk_impl(&mut self, disk: NvmeDisk) -> Result<(), MachineError> {
        let Some(nvme) = &self.nvme else {
            return Ok(());
        };

        if !disk
            .capacity_bytes()
            .is_multiple_of(aero_storage::SECTOR_SIZE as u64)
        {
            return Err(MachineError::DiskBackend(format!(
                "nvme disk capacity must be a multiple of {} bytes",
                aero_storage::SECTOR_SIZE
            )));
        }

        let (command, bar0_base) = if let Some(pci_cfg) = &self.pci_cfg {
            let bdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar0_base = cfg.and_then(|cfg| cfg.bar_range(0)).map(|range| range.base);
            (command, bar0_base)
        } else {
            (0, None)
        };

        // Ensure the device model's internal PCI config view is coherent with the canonical PCI
        // config space before capturing state to preserve.
        {
            let mut dev = nvme.borrow_mut();
            dev.config_mut().set_command(command);
            if let Some(bar0_base) = bar0_base {
                dev.config_mut().set_bar_base(0, bar0_base);
            }
        }

        // Preserve the device model's in-flight state (queues, pending interrupts, PCI config
        // snapshot bytes, etc.) while swapping the disk backend.
        let state = nvme.borrow().save_state();

        let mut new_dev = NvmePciDevice::try_new_from_virtual_disk(disk)
            .map_err(|e| MachineError::DiskBackend(format!("nvme disk backend error: {e:?}")))?;
        new_dev.load_state(&state).map_err(|e| {
            MachineError::DiskBackend(format!(
                "failed to restore nvme state after attaching disk backend: {e}"
            ))
        })?;

        // Replace the device model while keeping the `Rc` identity stable for persistent MMIO
        // mappings.
        *nvme.borrow_mut() = new_dev;

        // Keep internal PCI config coherent with the canonical `PciConfigPorts` state.
        {
            let mut dev = nvme.borrow_mut();
            dev.config_mut().set_command(command);
            if let Some(bar0_base) = bar0_base {
                dev.config_mut().set_bar_base(0, bar0_base);
            }
        }

        Ok(())
    }
    /// Attach the machine's canonical [`SharedDisk`] to AHCI port 0 (if AHCI is enabled).
    ///
    /// This makes firmware INT13 disk reads and AHCI DMA observe the same underlying bytes.
    ///
    /// If the AHCI controller is not present, this is a no-op and returns `Ok(())`.
    ///
    /// This method is idempotent: once the shared disk is attached/configured for port 0, further
    /// calls will not replace the existing drive.
    pub fn attach_shared_disk_to_ahci_port0(&mut self) -> std::io::Result<()> {
        let Some(ahci) = &self.ahci else {
            return Ok(());
        };
        if self.ahci_port0_auto_attach_shared_disk {
            // Auto-attach is already enabled; treat this as an idempotent "ensure attached" helper.
            //
            // Snapshot restore clears transient host-side backends (AHCI `AtaDrive`), but this flag
            // is host configuration state and remains true. In that case, port 0 may currently have
            // no drive and we must re-attach.
            if ahci.borrow().drive_attached(0) {
                return Ok(());
            }
        }

        let drive = AtaDrive::new(Box::new(self.disk.clone()))?;
        ahci.borrow_mut().attach_drive(0, drive);
        self.ahci_port0_auto_attach_shared_disk = true;
        Ok(())
    }

    /// Attach the machine's canonical [`SharedDisk`] to virtio-blk (if enabled).
    ///
    /// This makes BIOS INT13 disk reads and virtio-blk DMA observe the same underlying bytes.
    ///
    /// If the virtio-blk controller is not present, this is a no-op and returns `Ok(())`.
    ///
    /// This method is idempotent: once the shared disk is attached/configured for virtio-blk,
    /// further calls will not replace the existing backend.
    pub fn attach_shared_disk_to_virtio_blk(&mut self) -> Result<(), MachineError> {
        if self.virtio_blk.is_none() {
            return Ok(());
        }
        if self.virtio_blk_auto_attach_shared_disk {
            return Ok(());
        }

        self.swap_virtio_blk_backend_preserving_state(Box::new(self.disk.clone()))?;
        self.virtio_blk_auto_attach_shared_disk = true;
        Ok(())
    }

    /// Detach any drive currently attached to the canonical AHCI port 0.
    pub fn detach_ahci_drive_port0(&mut self) {
        let Some(ahci) = self.ahci.as_ref() else {
            return;
        };
        // If the canonical shared disk was attached, detaching it should also disable the
        // auto-attach behaviour so subsequent calls like `set_disk_image` do not silently
        // re-populate the port.
        self.ahci_port0_auto_attach_shared_disk = false;
        ahci.borrow_mut().detach_drive(0);
    }

    /// Attach an ATA drive as the primary master on the IDE controller, if present.
    pub fn attach_ide_primary_master_drive(&mut self, drive: AtaDrive) {
        let Some(ide) = &self.ide else {
            return;
        };
        ide.borrow_mut().controller.attach_primary_master_ata(drive);
    }

    /// Attach a disk image as an ATA drive on the IDE primary master, if present.
    pub fn attach_ide_primary_master_disk(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> std::io::Result<()> {
        self.attach_ide_primary_master_drive(AtaDrive::new(disk)?);
        Ok(())
    }

    /// Attach an ATAPI CD-ROM device as the secondary master on the IDE controller, if present.
    pub fn attach_ide_secondary_master_atapi(&mut self, dev: AtapiCdrom) {
        let Some(ide) = &self.ide else {
            return;
        };
        ide.borrow_mut()
            .controller
            .attach_secondary_master_atapi(dev);
    }
    /// Attach an ISO backend as the machine's canonical install media / ATAPI CD-ROM (`disk_id=1`).
    ///
    /// This models the media as inserted (updates guest-visible tray/media state) and also updates
    /// the BIOS' `install_media` backend so El Torito boot + INT13 reads can be routed to the ISO
    /// bytes when `boot_drive` selects a CD-ROM (`0xE0..=0xEF`).
    ///
    /// This should be used when initially attaching install media before booting the machine. For
    /// snapshot restore flows (where the guest-visible tray/media state is restored from the IDE
    /// snapshot and only the host backend needs re-attaching), use
    /// [`Machine::attach_install_media_iso_for_restore`].
    ///
    pub fn attach_install_media_iso(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> io::Result<()> {
        self.attach_ide_secondary_master_iso(disk)
    }

    /// Attach an ISO backend as the machine's canonical install media / ATAPI CD-ROM (`disk_id=1`)
    /// without changing guest-visible tray/media state.
    ///
    /// This is intended for snapshot restore flows: snapshot restore keeps the ATAPI device's
    /// internal tray/media state but drops the host-side ISO backend.
    pub fn attach_install_media_iso_for_restore(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> io::Result<()> {
        self.attach_ide_secondary_master_iso_for_restore(disk)
    }
    /// Attach an ISO image (provided as raw bytes) as the machine's canonical install media /
    /// ATAPI CD-ROM (`disk_id=1`).
    ///
    /// This is a convenience wrapper for browser/native hosts that already have the ISO contents
    /// in memory. For large ISOs, prefer a streaming/file-backed disk and call
    /// [`Machine::attach_install_media_iso_and_set_overlay_ref`].
    pub fn attach_install_media_iso_bytes(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        let disk = RawDisk::open(MemBackend::from_vec(bytes))
            .map_err(|e| io::Error::other(e.to_string()))?;
        // Raw/in-memory ISOs do not have a stable "base_image" identifier, so do not update the
        // snapshot overlay ref. Clear any existing ref so callers do not accidentally snapshot with
        // stale disk reopen metadata.
        self.clear_ide_secondary_master_atapi_overlay_ref();
        self.attach_ide_secondary_master_iso(Box::new(disk))
    }

    /// Attach a disk image as an ATAPI CD-ROM ISO on the IDE secondary master, if present.
    ///
    /// On `wasm32`, this intentionally does **not** require `Send` because some browser-backed
    /// disks (OPFS, etc.) may contain JS values and therefore cannot be sent across threads.
    pub fn attach_ide_secondary_master_iso(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> std::io::Result<()> {
        let shared = SharedIsoDisk::new(disk)?;
        if self.ide.is_some() {
            self.install_media = Some(InstallMedia::Weak(shared.downgrade()));
            self.attach_ide_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(shared))));
        } else {
            // Without IDE enabled, BIOS is the only consumer of install media. Keep a strong
            // reference so firmware boot + INT13 CD reads can access the ISO.
            self.install_media = Some(InstallMedia::Strong(shared));
        }
        Ok(())
    }

    /// Detach/eject any ISO currently attached to the IDE secondary master ATAPI device.
    pub fn detach_ide_secondary_master_iso(&mut self) {
        self.install_media = None;
        let Some(ide) = &self.ide else {
            return;
        };

        // Model a media eject but keep the ATAPI CD-ROM device present.
        let mut dev = AtapiCdrom::new(None);
        dev.eject_media();
        ide.borrow_mut()
            .controller
            .attach_secondary_master_atapi(dev);
    }

    /// Re-attach a disk image as an ISO backend to the IDE secondary master ATAPI device without
    /// changing guest-visible media state.
    ///
    /// This is intended for snapshot restore flows: snapshot restore keeps the ATAPI device's
    /// internal tray/media state but drops the host-side ISO backend.
    ///
    /// If the IDE controller is not present, this is a no-op and returns `Ok(())`.
    pub fn attach_ide_secondary_master_iso_for_restore(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
    ) -> std::io::Result<()> {
        if self.ide.is_none() {
            return Ok(());
        }
        let shared = SharedIsoDisk::new(disk)?;
        self.install_media = Some(InstallMedia::Weak(shared.downgrade()));
        let backend: Box<dyn IsoBackend> = Box::new(shared);
        self.attach_ide_secondary_master_atapi_backend_for_restore(backend);
        Ok(())
    }

    /// Convenience: attach the canonical install media ISO (`disk_id=1`) and record its snapshot
    /// overlay ref.
    ///
    /// # Overlay ref policy
    ///
    /// Install media is treated as a read-only ISO, so this records:
    /// - `base_image = base_image` (typically the OPFS path, e.g. `/state/win7.iso`)
    /// - `overlay_image = ""` (empty string indicates "no writable overlay")
    pub fn attach_install_media_iso_and_set_overlay_ref(
        &mut self,
        disk: Box<dyn aero_storage::VirtualDisk>,
        base_image: impl Into<String>,
    ) -> std::io::Result<()> {
        self.attach_install_media_iso(disk)?;
        self.set_ide_secondary_master_atapi_overlay_ref(base_image, "");
        Ok(())
    }

    /// Eject the canonical install media (IDE secondary master ATAPI) and clear its snapshot
    /// overlay ref (`disk_id=1`).
    pub fn eject_install_media(&mut self) {
        // Dropping the install media handle is important for browser runtimes using OPFS
        // `SyncAccessHandle`s: those are exclusive per file, so keeping the handle alive after
        // "eject" would prevent re-attaching the same ISO path later.
        self.install_media = None;
        if let Some(ide) = &self.ide {
            ide.borrow_mut()
                .controller
                .eject_secondary_master_atapi_media();
        }
        self.clear_ide_secondary_master_atapi_overlay_ref();
    }

    /// Re-attach a host ISO backend to the IDE secondary master ATAPI device without changing
    /// guest-visible media state.
    ///
    /// This is intended for snapshot restore flows: the IDE controller snapshot restores the
    /// ATAPI device's internal state (tray/sense/media_changed) but drops the host backend.
    pub fn attach_ide_secondary_master_atapi_backend_for_restore(
        &mut self,
        backend: Box<dyn IsoBackend>,
    ) {
        let Some(ide) = &self.ide else {
            return;
        };
        ide.borrow_mut()
            .controller
            .attach_secondary_master_atapi_backend_for_restore(backend);
    }

    /// Poll all known PCI INTx sources (and legacy ISA IRQ sources like IDE) and drive their
    /// current levels into the platform interrupt controller.
    ///
    /// This does *not* acknowledge any interrupts; it only updates level-triggered lines.
    pub fn poll_pci_intx_lines(&mut self) {
        self.sync_pci_intx_sources_to_interrupts();
    }
    /// Advance deterministic machine/platform time and poll any timer devices.
    ///
    /// This is used by [`Machine::run_slice`] to keep PIT/RTC/HPET/LAPIC timers progressing
    /// deterministically (based on executed CPU cycles, including while the CPU is halted), and
    /// is also exposed for tests and debugging.
    pub fn tick_platform(&mut self, delta_ns: u64) {
        if delta_ns == 0 {
            // Allow callers to "sync" canonical PCI config space state into USB device models
            // without advancing time.
            //
            // This is useful in tests and host integrations that program MSI/MSI-X enable bits in
            // the canonical PCI config space and then need the runtime device model to observe the
            // updated interrupt configuration.
            //
            // Keep this branch *side-effect free* with respect to guest RAM: do not advance BIOS
            // BDA time, PIT/HPET/LAPIC timers, or other devices that would write to guest memory,
            // so calling `tick_platform(0)` does not spuriously dirty pages.
            if let Some(uhci) = self.uhci.as_ref() {
                let bdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
                let (command, bar4_base) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let bar4_base = cfg
                            .and_then(|cfg| cfg.bar_range(UhciPciDevice::IO_BAR_INDEX))
                            .map(|range| range.base);
                        (command, bar4_base)
                    })
                    .unwrap_or((0, None));
                let mut uhci = uhci.borrow_mut();
                uhci.config_mut().set_command(command);
                if let Some(bar4_base) = bar4_base {
                    uhci.config_mut()
                        .set_bar_base(UhciPciDevice::IO_BAR_INDEX, bar4_base);
                }
            }

            if let Some(ehci) = self.ehci.as_ref() {
                let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
                let (command, bar0_base) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let bar0_base = cfg
                            .and_then(|cfg| cfg.bar_range(EhciPciDevice::MMIO_BAR_INDEX))
                            .map(|range| range.base);
                        (command, bar0_base)
                    })
                    .unwrap_or((0, None));
                let mut ehci = ehci.borrow_mut();
                ehci.config_mut().set_command(command);
                if let Some(bar0_base) = bar0_base {
                    ehci.config_mut()
                        .set_bar_base(EhciPciDevice::MMIO_BAR_INDEX, bar0_base);
                }
            }

            if let Some(xhci) = self.xhci.as_ref() {
                let bdf = aero_devices::pci::profile::USB_XHCI_QEMU.bdf;
                let (command, msi_state, msix_state) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let msi_state =
                            cfg.and_then(|cfg| cfg.capability::<MsiCapability>())
                                .map(|msi| {
                                    (
                                        msi.enabled(),
                                        msi.message_address(),
                                        msi.message_data(),
                                        msi.mask_bits(),
                                    )
                                });
                        let msix_state = cfg
                            .and_then(|cfg| cfg.capability::<MsixCapability>())
                            .map(|msix| (msix.enabled(), msix.function_masked()));
                        (command, msi_state, msix_state)
                    })
                    .unwrap_or((0, None, None));

                let mut xhci = xhci.borrow_mut();
                let cfg = xhci.config_mut();
                cfg.set_command(command);
                if let Some((enabled, addr, data, mask)) = msi_state {
                    sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                }
                if let Some((enabled, function_masked)) = msix_state {
                    sync_msix_capability_into_config(cfg, enabled, function_masked);
                }
            }

            return;
        }

        // Keep the core's A20 view coherent with the chipset latch even when advancing time
        // without executing instructions, and advance BIOS time-of-day / BDA tick count from the
        // canonical tick loop.
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        self.bios
            .advance_time(&mut self.mem, Duration::from_nanos(delta_ns));

        if let Some(vga) = &self.vga {
            vga.borrow_mut().tick(delta_ns);
        }

        if let Some(clock) = &self.platform_clock {
            clock.advance_ns(delta_ns);
        }

        if let Some(acpi_pm) = &self.acpi_pm {
            acpi_pm.borrow_mut().tick(delta_ns);
        }

        if let Some(pit) = &self.pit {
            pit.borrow_mut().advance_ns(delta_ns);
        }

        if let Some(rtc) = &self.rtc {
            rtc.borrow_mut().tick();
        }

        if let Some(interrupts) = &self.interrupts {
            interrupts.borrow().tick(delta_ns);
        }

        if let (Some(hpet), Some(interrupts)) = (&self.hpet, &self.interrupts) {
            let mut hpet = hpet.borrow_mut();
            let mut interrupts = interrupts.borrow_mut();
            hpet.poll(&mut *interrupts);
        }

        if let Some(aerogpu_mmio) = self.aerogpu_mmio.as_ref() {
            aerogpu_mmio.borrow_mut().tick(delta_ns, &mut self.mem);
        }

        if let Some(uhci) = self.uhci.as_ref() {
            const NS_PER_MS: u64 = 1_000_000;

            let bdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
            let (command, bar4_base) = self
                .pci_cfg
                .as_ref()
                .map(|pci_cfg| {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar4_base = cfg
                        .and_then(|cfg| cfg.bar_range(UhciPciDevice::IO_BAR_INDEX))
                        .map(|range| range.base);
                    (command, bar4_base)
                })
                .unwrap_or((0, None));

            // Keep the UHCI model's view of PCI config state in sync so it can apply bus mastering
            // gating when used via `tick_1ms`.
            let mut uhci = uhci.borrow_mut();
            uhci.config_mut().set_command(command);
            if let Some(bar4_base) = bar4_base {
                uhci.config_mut()
                    .set_bar_base(UhciPciDevice::IO_BAR_INDEX, bar4_base);
            }

            self.uhci_ns_remainder = self.uhci_ns_remainder.saturating_add(delta_ns);
            let mut ticks = self.uhci_ns_remainder / NS_PER_MS;
            self.uhci_ns_remainder %= NS_PER_MS;

            while ticks != 0 {
                uhci.tick_1ms(&mut self.mem.bus);
                ticks -= 1;
            }
        }

        if let Some(ehci) = self.ehci.as_ref() {
            const NS_PER_MS: u64 = 1_000_000;

            let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
            let (command, bar0_base) = self
                .pci_cfg
                .as_ref()
                .map(|pci_cfg| {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg
                        .and_then(|cfg| cfg.bar_range(EhciPciDevice::MMIO_BAR_INDEX))
                        .map(|range| range.base);
                    (command, bar0_base)
                })
                .unwrap_or((0, None));

            // Keep the EHCI model's view of PCI config state in sync so it can apply bus mastering
            // gating when used via `tick_1ms`.
            let mut ehci = ehci.borrow_mut();
            ehci.config_mut().set_command(command);
            if let Some(bar0_base) = bar0_base {
                ehci.config_mut()
                    .set_bar_base(EhciPciDevice::MMIO_BAR_INDEX, bar0_base);
            }

            self.ehci_ns_remainder = self.ehci_ns_remainder.saturating_add(delta_ns);
            let mut ticks = self.ehci_ns_remainder / NS_PER_MS;
            self.ehci_ns_remainder %= NS_PER_MS;

            while ticks != 0 {
                ehci.tick_1ms(&mut self.mem.bus);
                ticks -= 1;
            }
        }

        if let Some(xhci) = self.xhci.as_ref() {
            const NS_PER_MS: u64 = 1_000_000;

            let bdf = aero_devices::pci::profile::USB_XHCI_QEMU.bdf;
            let (command, msi_state, msix_state) = self
                .pci_cfg
                .as_ref()
                .map(|pci_cfg| {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let msi_state =
                        cfg.and_then(|cfg| cfg.capability::<MsiCapability>())
                            .map(|msi| {
                                (
                                    msi.enabled(),
                                    msi.message_address(),
                                    msi.message_data(),
                                    msi.mask_bits(),
                                )
                            });
                    let msix_state = cfg
                        .and_then(|cfg| cfg.capability::<MsixCapability>())
                        .map(|msix| (msix.enabled(), msix.function_masked()));
                    (command, msi_state, msix_state)
                })
                .unwrap_or((0, None, None));

            // Keep the xHCI model's view of PCI config state in sync (including MSI capability
            // state) so it can apply bus mastering and deliver MSI through `tick_1ms`.
            let mut xhci = xhci.borrow_mut();
            {
                // Note: MSI pending bits are device-managed and must not be overwritten from the
                // canonical PCI config space (which cannot observe them).
                let cfg = xhci.config_mut();
                cfg.set_command(command);
                if let Some((enabled, addr, data, mask)) = msi_state {
                    sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                }
                if let Some((enabled, function_masked)) = msix_state {
                    sync_msix_capability_into_config(cfg, enabled, function_masked);
                }
            }

            self.xhci_ns_remainder = self.xhci_ns_remainder.saturating_add(delta_ns);
            let mut ticks = self.xhci_ns_remainder / NS_PER_MS;
            self.xhci_ns_remainder %= NS_PER_MS;

            while ticks != 0 {
                xhci.tick_1ms(&mut self.mem.bus);
                ticks -= 1;
            }
        }
    }

    /// Backward-compatible alias for [`Machine::tick_platform`].
    pub fn tick(&mut self, delta_ns: u64) {
        self.tick_platform(delta_ns);
    }

    fn tick_platform_from_cycles(&mut self, cycles: u64) {
        if cycles == 0 {
            return;
        }

        let tsc_hz = self.cpu.time.tsc_hz();
        if tsc_hz == 0 {
            return;
        }

        if self.guest_time.cpu_hz() != tsc_hz {
            // If the caller changes the deterministic TSC frequency, preserve continuity by
            // resynchronizing the fractional remainder from the pre-batch TSC value.
            let tsc_before = self.cpu.state.msr.tsc.wrapping_sub(cycles);
            self.guest_time = GuestTime::new(tsc_hz);
            self.guest_time.resync_from_tsc(tsc_before);
        }

        let delta_ns = self.guest_time.advance_guest_time_for_instructions(cycles);
        if delta_ns != 0 {
            self.tick_platform(delta_ns);
        }

        // Keep AP TSC state synchronized to the BSP/global time. This models the fact that the TSC
        // continues ticking even while APs are waiting-for-SIPI and not being scheduled/executed.
        self.sync_ap_tsc_to_bsp();
    }

    fn sync_ap_tsc_to_bsp(&mut self) {
        if self.cfg.cpu_count <= 1 {
            return;
        }

        let tsc = self.cpu.state.msr.tsc;
        for cpu in self.ap_cpus.iter_mut() {
            cpu.time.set_tsc(tsc);
            cpu.state.msr.tsc = tsc;
        }
    }

    fn idle_tick_platform_1ms(&mut self) {
        // Only tick while halted when *some* vCPU could observe maskable interrupts (or is still
        // running).
        //
        // In a uniprocessor configuration, `cli; hlt` is effectively terminal (until NMI/SMI/reset,
        // which we do not model yet), so ticking would just burn host time.
        //
        // In SMP bring-up tests, the BSP may intentionally park itself in `cli; hlt` while another
        // vCPU waits in `sti; hlt` for a timer interrupt. Allow time to advance as long as at least
        // one vCPU can accept a maskable interrupt (IF=1) or is currently executing, so devices
        // like PIT/HPET/LAPIC timers can make progress and wake the system.
        let should_tick = (self.cpu.state.rflags() & aero_cpu_core::state::RFLAGS_IF) != 0
            || self.ap_cpus.iter().any(|cpu| {
                !cpu.state.halted || (cpu.state.rflags() & aero_cpu_core::state::RFLAGS_IF) != 0
            });
        if !should_tick {
            return;
        }

        let tsc_hz = self.cpu.time.tsc_hz();
        if tsc_hz == 0 {
            return;
        }

        // Advance 1ms worth of CPU cycles while halted so timer devices can wake the CPU.
        let cycles = (tsc_hz / 1000).max(1);
        self.cpu.time.advance_cycles(cycles);
        self.cpu.state.msr.tsc = self.cpu.time.read_tsc();
        self.tick_platform_from_cycles(cycles);
    }

    fn resync_guest_time_from_tsc(&mut self) {
        let tsc_hz = self.cpu.time.tsc_hz();
        if self.guest_time.cpu_hz() != tsc_hz {
            self.guest_time = GuestTime::new(tsc_hz);
        }
        self.guest_time.resync_from_tsc(self.cpu.state.msr.tsc);
    }

    fn sync_pci_intx_sources_to_interrupts(&mut self) {
        let Some(interrupts) = &self.interrupts else {
            return;
        };

        if let Some(pci_intx) = &self.pci_intx {
            let mut pci_intx = pci_intx.borrow_mut();
            let mut interrupts = interrupts.borrow_mut();

            // E1000 legacy INTx (level-triggered).
            if let Some(e1000) = &self.e1000 {
                let bdf: PciBdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
                let pin = PciInterruptPin::IntA;

                // Keep the device model's internal PCI command register in sync with the platform
                // PCI bus so `E1000Device::irq_level` can respect COMMAND.INTX_DISABLE (bit 10).
                let command = self
                    .pci_cfg
                    .as_ref()
                    .and_then(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        pci_cfg
                            .bus_mut()
                            .device_config(bdf)
                            .map(|cfg| cfg.command())
                    })
                    .unwrap_or(0);
                {
                    let mut dev = e1000.borrow_mut();
                    dev.pci_config_write(0x04, 2, u32::from(command));
                }

                let mut level = e1000.borrow().irq_level();

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // ICH9 AHCI legacy INTx (level-triggered).
            if let Some(ahci) = &self.ahci {
                let bdf: PciBdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
                let pin = PciInterruptPin::IntA;

                // Keep the device model's internal PCI command state coherent so
                // `AhciPciDevice::intx_level()` can apply COMMAND.INTX_DISABLE gating.
                let (command, bar5_base, msi_state) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config_mut(bdf);
                        let command = cfg.as_ref().map(|cfg| cfg.command()).unwrap_or(0);
                        let bar5_base = cfg
                            .as_ref()
                            .and_then(|cfg| {
                                cfg.bar_range(aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX)
                            })
                            .map(|range| range.base);
                        let msi_state = cfg
                            .as_ref()
                            .and_then(|cfg| cfg.capability::<MsiCapability>())
                            .map(|msi| {
                                (
                                    msi.enabled(),
                                    msi.message_address(),
                                    msi.message_data(),
                                    msi.mask_bits(),
                                )
                            });
                        (command, bar5_base, msi_state)
                    })
                    .unwrap_or((0, None, None));

                let mut ahci_dev = ahci.borrow_mut();
                {
                    let cfg = ahci_dev.config_mut();
                    cfg.set_command(command);
                    if let Some(bar5_base) = bar5_base {
                        cfg.set_bar_base(
                            aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX,
                            bar5_base,
                        );
                    }
                    if let Some((enabled, addr, data, mask)) = msi_state {
                        // Note: MSI pending bits are device-managed and must not be overwritten from the
                        // canonical PCI config space (which cannot observe them).
                        sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                    }
                }

                let mut level = ahci_dev.intx_level();

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // NVMe legacy INTx (level-triggered).
            if let Some(nvme) = &self.nvme {
                let bdf: PciBdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
                let pin = PciInterruptPin::IntA;

                let (command, msi_state, msix_state) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let msi_state =
                            cfg.and_then(|cfg| cfg.capability::<MsiCapability>())
                                .map(|msi| {
                                    (
                                        msi.enabled(),
                                        msi.message_address(),
                                        msi.message_data(),
                                        msi.mask_bits(),
                                    )
                                });
                        let msix_state = cfg
                            .and_then(|cfg| cfg.capability::<MsixCapability>())
                            .map(|msix| (msix.enabled(), msix.function_masked()));
                        (command, msi_state, msix_state)
                    })
                    .unwrap_or((0, None, None));

                let mut nvme_dev = nvme.borrow_mut();
                {
                    // Keep device-side gating consistent even though the machine owns the canonical
                    // PCI config space.
                    //
                    // Note: MSI pending bits are device-managed and must not be overwritten from the
                    // canonical PCI config space (which cannot observe them).
                    let cfg = nvme_dev.config_mut();
                    cfg.set_command(command);
                    if let Some((enabled, addr, data, mask)) = msi_state {
                        sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                    }
                    if let Some((enabled, function_masked)) = msix_state {
                        sync_msix_capability_into_config(cfg, enabled, function_masked);
                    }
                }

                let mut level = nvme_dev.irq_level();

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // Virtio-net legacy INTx (level-triggered).
            if let Some(virtio) = &self.virtio_net {
                let bdf: PciBdf = aero_devices::pci::profile::VIRTIO_NET.bdf;
                let pin = PciInterruptPin::IntA;

                let (command, msix_enabled, msix_masked) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        match pci_cfg.bus_mut().device_config(bdf) {
                            Some(cfg) => {
                                let msix = cfg.capability::<MsixCapability>();
                                (
                                    cfg.command(),
                                    msix.is_some_and(|msix| msix.enabled()),
                                    msix.is_some_and(|msix| msix.function_masked()),
                                )
                            }
                            None => (0, false, false),
                        }
                    })
                    .unwrap_or((0, false, false));

                // Keep the virtio transport's internal PCI command register in sync so `irq_level`
                // can respect COMMAND.INTX_DISABLE (bit 10) while the machine owns the canonical PCI
                // config space.
                let mut level = {
                    let mut virtio_dev = virtio.borrow_mut();
                    // Keep the runtime virtio transport's MSI-X enable/mask bits coherent with the
                    // canonical PCI config space so INTx suppression reflects the guest-visible
                    // MSI-X state even when only polling INTx lines.
                    sync_msix_capability_into_config(
                        virtio_dev.config_mut(),
                        msix_enabled,
                        msix_masked,
                    );
                    virtio_dev.set_pci_command(command);
                    virtio_dev.irq_level()
                };

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // virtio-input keyboard legacy INTx (level-triggered).
            if let Some(virtio_input_keyboard) = &self.virtio_input_keyboard {
                let bdf: PciBdf = aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD.bdf;
                let pin = PciInterruptPin::IntA;

                let (command, msix_enabled, msix_masked) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        match pci_cfg.bus_mut().device_config(bdf) {
                            Some(cfg) => {
                                let msix = cfg.capability::<MsixCapability>();
                                (
                                    cfg.command(),
                                    msix.is_some_and(|msix| msix.enabled()),
                                    msix.is_some_and(|msix| msix.function_masked()),
                                )
                            }
                            None => (0, false, false),
                        }
                    })
                    .unwrap_or((0, false, false));

                let mut level = {
                    let mut dev = virtio_input_keyboard.borrow_mut();
                    sync_msix_capability_into_config(dev.config_mut(), msix_enabled, msix_masked);
                    dev.set_pci_command(command);
                    dev.irq_level()
                };
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // virtio-input mouse legacy INTx (level-triggered).
            if let Some(virtio_input_mouse) = &self.virtio_input_mouse {
                let bdf: PciBdf = aero_devices::pci::profile::VIRTIO_INPUT_MOUSE.bdf;
                let pin = PciInterruptPin::IntA;

                let (command, msix_enabled, msix_masked) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        match pci_cfg.bus_mut().device_config(bdf) {
                            Some(cfg) => {
                                let msix = cfg.capability::<MsixCapability>();
                                (
                                    cfg.command(),
                                    msix.is_some_and(|msix| msix.enabled()),
                                    msix.is_some_and(|msix| msix.function_masked()),
                                )
                            }
                            None => (0, false, false),
                        }
                    })
                    .unwrap_or((0, false, false));

                let mut level = {
                    let mut dev = virtio_input_mouse.borrow_mut();
                    sync_msix_capability_into_config(dev.config_mut(), msix_enabled, msix_masked);
                    dev.set_pci_command(command);
                    dev.irq_level()
                };
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // virtio-blk legacy INTx (level-triggered).
            if let Some(virtio_blk) = &self.virtio_blk {
                let bdf: PciBdf = aero_devices::pci::profile::VIRTIO_BLK.bdf;
                let pin = PciInterruptPin::IntA;

                let (command, msix_enabled, msix_masked) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        match pci_cfg.bus_mut().device_config(bdf) {
                            Some(cfg) => {
                                let msix = cfg.capability::<MsixCapability>();
                                (
                                    cfg.command(),
                                    msix.is_some_and(|msix| msix.enabled()),
                                    msix.is_some_and(|msix| msix.function_masked()),
                                )
                            }
                            None => (0, false, false),
                        }
                    })
                    .unwrap_or((0, false, false));

                // Keep the virtio transport's internal PCI command register in sync so it can
                // respect COMMAND.INTX_DISABLE gating.
                {
                    let mut dev = virtio_blk.borrow_mut();
                    sync_msix_capability_into_config(dev.config_mut(), msix_enabled, msix_masked);
                    dev.set_pci_command(command);
                }

                let mut level = virtio_blk.borrow().irq_level();

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // AeroGPU legacy INTx (level-triggered).
            if let Some(aerogpu) = &self.aerogpu_mmio {
                let bdf: PciBdf = aero_devices::pci::profile::AEROGPU.bdf;
                let pin = PciInterruptPin::IntA;

                // Keep the AeroGPU model's internal PCI command/BAR state coherent so `irq_level()`
                // can apply COMMAND.INTX_DISABLE gating even though the machine owns the canonical
                // PCI config space.
                let (command, bar0_base, bar1_base) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let bar0_base = cfg
                            .and_then(|cfg| {
                                cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR0_INDEX)
                            })
                            .map(|range| range.base)
                            .unwrap_or(0);
                        let bar1_base = cfg
                            .and_then(|cfg| {
                                cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
                            })
                            .map(|range| range.base)
                            .unwrap_or(0);
                        (command, bar0_base, bar1_base)
                    })
                    .unwrap_or((0, 0, 0));
                let mut level = {
                    let mut dev = aerogpu.borrow_mut();
                    // Keep the AeroGPU model's internal PCI config image coherent with the
                    // canonical PCI config space owned by the machine.
                    dev.config_mut().set_command(command);
                    dev.config_mut()
                        .set_bar_base(aero_devices::pci::profile::AEROGPU_BAR0_INDEX, bar0_base);
                    dev.config_mut().set_bar_base(
                        aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX,
                        bar1_base,
                    );
                    dev.irq_level()
                };

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // PIIX3 UHCI legacy INTx (level-triggered).
            if let Some(uhci) = &self.uhci {
                let bdf: PciBdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
                let pin = PciInterruptPin::IntA;

                // Keep the device model's internal PCI command state coherent so
                // `UhciPciDevice::irq_level()` and `UhciPciDevice::tick_1ms` can apply
                // COMMAND.INTX_DISABLE and COMMAND.BME gating.
                let (command, bar4_base) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let bar4_base = cfg
                            .and_then(|cfg| cfg.bar_range(UhciPciDevice::IO_BAR_INDEX))
                            .map(|range| range.base);
                        (command, bar4_base)
                    })
                    .unwrap_or((0, None));

                let mut uhci_dev = uhci.borrow_mut();
                uhci_dev.config_mut().set_command(command);
                if let Some(bar4_base) = bar4_base {
                    uhci_dev
                        .config_mut()
                        .set_bar_base(UhciPciDevice::IO_BAR_INDEX, bar4_base);
                }

                let mut level = uhci_dev.irq_level();

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // EHCI legacy INTx (level-triggered).
            if let Some(ehci) = &self.ehci {
                let bdf: PciBdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
                let pin = PciInterruptPin::IntA;

                // Keep the device model's internal PCI command state coherent so
                // `EhciPciDevice::irq_level()` and `EhciPciDevice::tick_1ms` can apply
                // COMMAND.INTX_DISABLE and COMMAND.BME gating.
                let (command, bar0_base) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let bar0_base = cfg
                            .and_then(|cfg| cfg.bar_range(EhciPciDevice::MMIO_BAR_INDEX))
                            .map(|range| range.base);
                        (command, bar0_base)
                    })
                    .unwrap_or((0, None));

                let mut ehci_dev = ehci.borrow_mut();
                ehci_dev.config_mut().set_command(command);
                if let Some(bar0_base) = bar0_base {
                    ehci_dev
                        .config_mut()
                        .set_bar_base(EhciPciDevice::MMIO_BAR_INDEX, bar0_base);
                }

                let mut level = ehci_dev.irq_level();

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }

            // xHCI legacy INTx (level-triggered).
            if let Some(xhci) = &self.xhci {
                let bdf: PciBdf = aero_devices::pci::profile::USB_XHCI_QEMU.bdf;
                let pin = PciInterruptPin::IntA;

                // Keep the xHCI model's internal PCI config state coherent so `irq_level()` can
                // suppress legacy INTx when MSI is enabled.
                let (command, msi_state, msix_state) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let msi_state =
                            cfg.and_then(|cfg| cfg.capability::<MsiCapability>())
                                .map(|msi| {
                                    (
                                        msi.enabled(),
                                        msi.message_address(),
                                        msi.message_data(),
                                        msi.mask_bits(),
                                    )
                                });
                        let msix_state = cfg
                            .and_then(|cfg| cfg.capability::<MsixCapability>())
                            .map(|msix| (msix.enabled(), msix.function_masked()));
                        (command, msi_state, msix_state)
                    })
                    .unwrap_or((0, None, None));

                let mut xhci_dev = xhci.borrow_mut();
                {
                    // Note: MSI pending bits are device-managed and must not be overwritten from the
                    // canonical PCI config space (which cannot observe them).
                    let cfg = xhci_dev.config_mut();
                    cfg.set_command(command);
                    if let Some((enabled, addr, data, mask)) = msi_state {
                        sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                    }
                    if let Some((enabled, function_masked)) = msix_state {
                        sync_msix_capability_into_config(cfg, enabled, function_masked);
                    }
                }

                let mut level = xhci_dev.irq_level();

                // Redundantly gate on the canonical PCI command register as well (defensive).
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }
        }

        // IDE legacy compatibility mode uses ISA IRQ14/IRQ15 rather than PCI INTx.
        if let Some(ide) = &self.ide {
            let (irq14, irq15) = {
                let ide = ide.borrow();
                (
                    ide.controller.primary_irq_pending(),
                    ide.controller.secondary_irq_pending(),
                )
            };
            if let (Some(irq14_line), Some(irq15_line)) =
                (self.ide_irq14_line.as_ref(), self.ide_irq15_line.as_ref())
            {
                irq14_line.set_level(irq14);
                irq15_line.set_level(irq15);
            } else {
                // Fall back to direct interrupt router manipulation for non-standard machine
                // wiring. Prefer `PlatformIrqLine` when possible so repeated polling does not
                // over/under-count assertions in `PlatformInterrupts`.
                let mut interrupts = interrupts.borrow_mut();
                if irq14 {
                    interrupts.raise_irq(InterruptInput::IsaIrq(14));
                } else {
                    interrupts.lower_irq(InterruptInput::IsaIrq(14));
                }
                if irq15 {
                    interrupts.raise_irq(InterruptInput::IsaIrq(15));
                } else {
                    interrupts.lower_irq(InterruptInput::IsaIrq(15));
                }
            }
        }

        // COM1 serial (16550) uses ISA IRQ4 on PC-compatible platforms. The UART's interrupt output
        // is level-based (asserted while an interrupt condition is pending); the platform interrupt
        // router converts this into an edge for the legacy PIC, and into a level for IOAPIC mode.
        if let Some(serial) = &self.serial {
            let level = serial.borrow().irq_level();
            let mut interrupts = interrupts.borrow_mut();
            if level {
                interrupts.raise_irq(InterruptInput::IsaIrq(4));
            } else {
                interrupts.lower_irq(InterruptInput::IsaIrq(4));
            }
        }
    }

    /// Take (drain) all serial output accumulated so far.
    pub fn take_serial_output(&mut self) -> Vec<u8> {
        self.flush_serial();
        std::mem::take(&mut self.serial_log)
    }

    /// Return a copy of the serial output accumulated so far without draining it.
    ///
    /// This is intentionally a cloning API: callers that only need a byte count should prefer
    /// [`Machine::serial_output_len`].
    pub fn serial_output_bytes(&mut self) -> Vec<u8> {
        self.flush_serial();
        self.serial_log.clone()
    }

    /// Return the number of bytes currently buffered in the serial output log.
    ///
    /// This is a cheap alternative to [`Machine::take_serial_output`] for callers that only need a
    /// byte count (e.g. UI progress indicators) and want to avoid copying large buffers.
    pub fn serial_output_len(&mut self) -> u64 {
        self.flush_serial();
        u64::try_from(self.serial_log.len()).unwrap_or(u64::MAX)
    }

    /// Returns the BIOS "TTY output" buffer accumulated so far.
    ///
    /// The legacy HLE BIOS records:
    /// - bytes written via INT 10h teletype output (AH=0Eh), and
    /// - certain early boot/panic strings (for example when the boot sector is missing/invalid).
    ///
    /// This is primarily intended for **early-boot debugging and tests**. Output is best-effort:
    /// it is not a stable, user-facing console API and may change as the firmware implementation
    /// evolves.
    pub fn bios_tty_output(&self) -> &[u8] {
        self.bios.tty_output()
    }

    /// Return a copy of the BIOS "TTY output" buffer accumulated so far.
    ///
    /// This is a cloning convenience API for callers (for example, wasm bindings) that prefer an
    /// owned buffer rather than borrowing a slice from the machine.
    pub fn bios_tty_output_bytes(&self) -> Vec<u8> {
        self.bios.tty_output().to_vec()
    }

    /// Clear the BIOS "TTY output" buffer.
    pub fn clear_bios_tty_output(&mut self) {
        self.bios.clear_tty_output();
    }

    /// Take (drain) all DebugCon output accumulated so far.
    ///
    /// This captures bytes written by the guest to I/O port `0xE9` (Bochs/QEMU "debugcon").
    pub fn take_debugcon_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.debugcon_log.borrow_mut())
    }

    /// Return a copy of the DebugCon output accumulated so far without draining it.
    pub fn debugcon_output_bytes(&mut self) -> Vec<u8> {
        self.debugcon_log.borrow().clone()
    }

    /// Return the number of bytes currently buffered in the DebugCon output log.
    pub fn debugcon_output_len(&mut self) -> u64 {
        u64::try_from(self.debugcon_log.borrow().len()).unwrap_or(u64::MAX)
    }

    /// Returns the current PS/2 (i8042) keyboard LED bitmask as last set by the guest OS, or 0 if
    /// the i8042 controller is not present.
    ///
    /// The returned bitmask matches the HID/virtio-input LED layout used by
    /// [`Machine::usb_hid_keyboard_leds`] and [`Machine::virtio_input_keyboard_leds`]:
    /// - bit0: Num Lock
    /// - bit1: Caps Lock
    /// - bit2: Scroll Lock
    /// - bit3: Compose
    /// - bit4: Kana
    ///
    /// Note: the underlying PS/2 `Set LEDs` command uses a different bit ordering. This helper
    /// normalizes it to the shared HID-style layout for convenience.
    pub fn ps2_keyboard_leds(&self) -> u8 {
        let Some(ctrl) = &self.i8042 else {
            return 0;
        };
        // PS/2 bit layout (Set LEDs payload): bit0=Scroll, bit1=Num, bit2=Caps.
        let raw = ctrl.borrow().keyboard().leds() & 0x07;
        let scroll = raw & 0x01;
        let num = (raw >> 1) & 0x01;
        let caps = (raw >> 2) & 0x01;
        (num) | (caps << 1) | (scroll << 2)
    }

    /// Inject a browser-style keyboard code into the i8042 controller, if present.
    pub fn inject_browser_key(&mut self, code: &str, pressed: bool) {
        // `Machine::inject_browser_key` is primarily a PS/2 injection API (i8042), but browsers
        // expose several "media keys" (volume/playback/navigation) as `KeyboardEvent.code` values
        // that do not have stable PS/2 Set-2 scancode assignments. Those live on the HID Consumer
        // usage page (0x0C), and are modeled by Aero as a separate synthetic USB HID device.
        //
        // To keep the injection surface ergonomic for callers that only have DOM `code` strings,
        // fall back to the synthetic consumer-control device when the code is not representable
        // as a PS/2 scancode.
        if aero_devices_input::scancode::browser_code_to_set2(code).is_some() {
            if let Some(ctrl) = &self.i8042 {
                ctrl.borrow_mut().inject_browser_key(code, pressed);
            }
            return;
        }

        let Some(usage) = aero_usb::hid::usage::keyboard_code_to_consumer_usage(code) else {
            return;
        };

        // Route the consumer usage via the best available backend, ensuring the press+release pair
        // is delivered to the *same* backend. Virtio-input readiness can change asynchronously
        // (when the guest sets `DRIVER_OK`), so without this tracking we'd risk leaving the USB
        // consumer-control device stuck in a pressed state if the press was routed to USB and the
        // release was routed to virtio.
        let idx = usage as usize;
        if idx < self.consumer_usage_backend.len() {
            const BACKEND_USB: u8 = 1;
            const BACKEND_VIRTIO: u8 = 2;
            let prev = self.consumer_usage_backend[idx];

            // Map the consumer usage to a virtio-input `KEY_*` code when supported by the Win7
            // virtio-input driver (media keys subset).
            //
            // Note: this mapping is independent of whether virtio-input is currently `DRIVER_OK`;
            // we use it for routing both press and release events (to keep pairs consistent).
            let virtio_key = {
                use aero_virtio::devices::input::*;
                match usage {
                    0x00E2 => Some(KEY_MUTE),
                    0x00EA => Some(KEY_VOLUMEDOWN),
                    0x00E9 => Some(KEY_VOLUMEUP),
                    0x00CD => Some(KEY_PLAYPAUSE),
                    0x00B5 => Some(KEY_NEXTSONG),
                    0x00B6 => Some(KEY_PREVIOUSSONG),
                    0x00B7 => Some(KEY_STOPCD),
                    _ => None,
                }
            };

            if pressed {
                if prev != 0 {
                    // Duplicate keydown; ignore.
                    return;
                }
                if self.virtio_input_keyboard_driver_ok() {
                    if let Some(key) = virtio_key {
                        self.inject_virtio_key(key, true);
                        self.consumer_usage_backend[idx] = BACKEND_VIRTIO;
                        return;
                    }
                }
                if self.usb_hid_consumer_control.is_some() {
                    self.inject_usb_hid_consumer_usage(u32::from(usage), true);
                    self.consumer_usage_backend[idx] = BACKEND_USB;
                }
                return;
            }

            // Release: prefer the backend that handled the press.
            match prev {
                BACKEND_VIRTIO => {
                    if let Some(key) = virtio_key {
                        self.inject_virtio_key(key, false);
                    }
                }
                BACKEND_USB => {
                    self.inject_usb_hid_consumer_usage(u32::from(usage), false);
                }
                _ => {
                    // Unknown backend (e.g. snapshot restored into a new machine which does not
                    // have the press->release pairing state). Best-effort release both backends so
                    // we don't leave the guest stuck in a pressed state.
                    if let Some(key) = virtio_key {
                        self.inject_virtio_key(key, false);
                    }
                    self.inject_usb_hid_consumer_usage(u32::from(usage), false);
                }
            }
            self.consumer_usage_backend[idx] = 0;
            return;
        }

        // Out-of-range consumer usage: best-effort fall back to the USB consumer-control device.
        self.inject_usb_hid_consumer_usage(u32::from(usage), pressed);
    }

    /// Inject raw PS/2 Set-2 keyboard scancode bytes into the i8042 controller, if present.
    ///
    /// This is intended for callers that already have Set-2 byte sequences (including `0xE0` and
    /// `0xF0` prefixes), such as the browser runtime input capture pipeline.
    pub fn inject_key_scancode_bytes(&mut self, bytes: &[u8]) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_key_scancode_bytes(bytes);
        }
    }

    /// Inject up to 4 raw PS/2 Set-2 scancode bytes into the i8042 controller, if present.
    ///
    /// This matches the packed format used by the browser input batch pipeline
    /// (`web/src/input/event_queue.ts`):
    /// - `packed`: little-endian packed bytes (b0 in bits 0..7)
    /// - `len`: number of valid bytes in `packed` (1..=4)
    pub fn inject_key_scancode_packed(&mut self, packed: u32, len: u8) {
        let len = len.min(4) as usize;
        if len == 0 {
            return;
        }

        let bytes = packed.to_le_bytes();
        self.inject_key_scancode_bytes(&bytes[..len]);
    }

    /// Inject an arbitrary-length raw PS/2 Set-2 scancode byte sequence into the guest i8042
    /// keyboard device.
    ///
    /// This is an alias for [`Machine::inject_key_scancode_bytes`], provided for API parity with
    /// `crates/aero-wasm::Machine`.
    pub fn inject_keyboard_bytes(&mut self, bytes: &[u8]) {
        self.inject_key_scancode_bytes(bytes);
    }

    /// Inject a classic BIOS keyboard word into the firmware INT 16h queue.
    ///
    /// `key` uses the historical BIOS encoding: `(scan_code << 8) | ascii`.
    ///
    /// This is intentionally separate from [`Machine::inject_browser_key`] /
    /// [`Machine::inject_key_scancode_bytes`], which target the PS/2 i8042 controller path used by
    /// modern OSes.
    pub fn inject_bios_key(&mut self, key: u16) {
        self.bios.push_key(key);

        // Keep the BIOS Data Area keyboard ring buffer mirror coherent for guests that probe it
        // directly (e.g. DOS bootloaders/UI code that checks the head/tail pointers without
        // invoking INT 16h).
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios.sync_keyboard_bda(bus);
    }

    /// Inject relative mouse motion into the i8042 controller, if present.
    ///
    /// `dx` is positive to the right and `dy` is positive down (browser-style). The underlying PS/2
    /// mouse model converts this into PS/2 packet coordinates (+Y is up).
    pub fn inject_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_mouse_motion(dx, dy, wheel);
        }
    }

    /// Inject a PS/2 mouse button transition into the i8042 controller, if present.
    pub fn inject_mouse_button(&mut self, button: Ps2MouseButton, pressed: bool) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_mouse_button(button, pressed);
        }

        // Keep the absolute-mask helper (`inject_ps2_mouse_buttons`) coherent even if callers mix
        // the per-button APIs and the absolute-mask API.
        let bit = match button {
            Ps2MouseButton::Left => 0x01,
            Ps2MouseButton::Right => 0x02,
            Ps2MouseButton::Middle => 0x04,
            Ps2MouseButton::Side => 0x08,
            Ps2MouseButton::Extra => 0x10,
        };
        if pressed {
            self.ps2_mouse_buttons |= bit;
        } else {
            self.ps2_mouse_buttons &= !bit;
        }
    }

    pub fn inject_mouse_left(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Left, pressed);
    }

    pub fn inject_mouse_right(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Right, pressed);
    }

    pub fn inject_mouse_middle(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Middle, pressed);
    }

    pub fn inject_mouse_back(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Side, pressed);
    }

    pub fn inject_mouse_forward(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Extra, pressed);
    }

    /// Inject a PS/2 mouse button transition using DOM `MouseEvent.button` mapping:
    /// - `0`: left
    /// - `1`: middle
    /// - `2`: right
    /// - `3`: back
    /// - `4`: forward
    ///
    /// Other values are ignored.
    pub fn inject_mouse_button_dom(&mut self, button: u8, pressed: bool) {
        match button {
            0 => self.inject_mouse_left(pressed),
            1 => self.inject_mouse_middle(pressed),
            2 => self.inject_mouse_right(pressed),
            3 => self.inject_mouse_back(pressed),
            4 => self.inject_mouse_forward(pressed),
            _ => {}
        }
    }

    /// Inject a PS/2 mouse motion event into the i8042 controller, if present.
    ///
    /// Coordinate conventions:
    /// - `dy > 0` means cursor moved up.
    /// - `wheel > 0` means wheel moved up.
    pub fn inject_ps2_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        // `aero_devices_input::Ps2Mouse` expects browser-style +Y=down internally.
        // Host input values are untrusted; avoid overflow when negating `i32::MIN`.
        self.inject_mouse_motion(dx, 0i32.saturating_sub(dy), wheel);
    }

    /// Inject a PS/2 mouse button state into the i8042 controller, if present.
    ///
    /// `buttons` is a bitmask:
    /// - bit 0: left
    /// - bit 1: right
    /// - bit 2: middle
    /// - bit 3: back/side (only emitted if the guest enabled the IntelliMouse Explorer extension)
    /// - bit 4: forward/extra (same note as bit 3)
    pub fn inject_mouse_buttons_mask(&mut self, buttons: u8) {
        self.inject_ps2_mouse_buttons(buttons);
    }

    /// Inject a PS/2 mouse button state into the i8042 controller, if present.
    ///
    /// `buttons` is a bitmask:
    /// - bit 0: left
    /// - bit 1: right
    /// - bit 2: middle
    /// - bit 3: back/side (only emitted if the guest enabled the IntelliMouse Explorer extension)
    /// - bit 4: forward/extra (same note as bit 3)
    pub fn inject_ps2_mouse_buttons(&mut self, buttons: u8) {
        let buttons = buttons & 0x1f;

        // `ps2_mouse_buttons` is a host-side cache used to compute transitions from an absolute
        // button mask. Prefer the authoritative guest device state when the i8042 controller is
        // present: the guest can reset/reconfigure the mouse independently, making the cached value
        // stale.
        let prev = if let Some(ctrl) = &self.i8042 {
            ctrl.borrow().mouse_buttons_mask() & 0x1f
        } else {
            self.ps2_mouse_buttons & 0x1f
        };
        let changed = prev ^ buttons;
        if changed == 0 {
            // Keep the cache coherent (and clear any invalid marker, e.g. post snapshot restore).
            self.ps2_mouse_buttons = buttons;
            return;
        }

        if (changed & 0x01) != 0 {
            self.inject_mouse_button(Ps2MouseButton::Left, (buttons & 0x01) != 0);
        }
        if (changed & 0x02) != 0 {
            self.inject_mouse_button(Ps2MouseButton::Right, (buttons & 0x02) != 0);
        }
        if (changed & 0x04) != 0 {
            self.inject_mouse_button(Ps2MouseButton::Middle, (buttons & 0x04) != 0);
        }
        if (changed & 0x08) != 0 {
            self.inject_mouse_button(Ps2MouseButton::Side, (buttons & 0x08) != 0);
        }
        if (changed & 0x10) != 0 {
            self.inject_mouse_button(Ps2MouseButton::Extra, (buttons & 0x10) != 0);
        }

        self.ps2_mouse_buttons = buttons;
    }

    // -------------------------------------------------------------------------
    // virtio-input (paravirtualized keyboard/mouse)
    // -------------------------------------------------------------------------

    /// Whether the guest driver for the virtio-input keyboard has reached `DRIVER_OK`.
    ///
    /// Returns `false` if virtio-input is disabled or the keyboard function is absent.
    pub fn virtio_input_keyboard_driver_ok(&self) -> bool {
        self.virtio_input_keyboard
            .as_ref()
            .is_some_and(|dev| dev.borrow().driver_ok())
    }

    /// Returns the virtio-input keyboard LED bitmask (NumLock/CapsLock/ScrollLock/Compose/Kana)
    /// as last set by the guest OS via the virtio-input `statusq`, or 0 if the virtio-input
    /// keyboard function is absent.
    pub fn virtio_input_keyboard_leds(&self) -> u8 {
        let Some(kbd) = &self.virtio_input_keyboard else {
            return 0;
        };
        let dev = kbd.borrow();
        let Some(input) = dev.device::<VirtioInput>() else {
            return 0;
        };
        input.leds_mask()
    }

    /// Whether the guest driver for the virtio-input mouse has reached `DRIVER_OK`.
    ///
    /// Returns `false` if virtio-input is disabled or the mouse function is absent.
    pub fn virtio_input_mouse_driver_ok(&self) -> bool {
        self.virtio_input_mouse
            .as_ref()
            .is_some_and(|dev| dev.borrow().driver_ok())
    }

    /// Inject a Linux input key event (`EV_KEY` + `KEY_*`) into the virtio-input keyboard device.
    ///
    /// This is a no-op when virtio-input is disabled.
    pub fn inject_virtio_key(&mut self, linux_key: u16, pressed: bool) {
        let Some(kbd) = &self.virtio_input_keyboard else {
            return;
        };
        {
            let mut dev = kbd.borrow_mut();
            let Some(input) = dev.device_mut::<VirtioInput>() else {
                return;
            };
            input.inject_key(linux_key, pressed);
        }
        self.process_virtio_input();
        // Ensure legacy INTx routing reflects the device's updated IRQ latch immediately, without
        // requiring a subsequent `run_slice` call.
        self.sync_pci_intx_sources_to_interrupts();
    }

    /// Inject a Linux input relative motion event (`EV_REL` + `REL_X/REL_Y`) into the virtio-input
    /// mouse device.
    ///
    /// This is a no-op when virtio-input is disabled.
    pub fn inject_virtio_rel(&mut self, dx: i32, dy: i32) {
        let Some(mouse) = &self.virtio_input_mouse else {
            return;
        };
        {
            let mut dev = mouse.borrow_mut();
            let Some(input) = dev.device_mut::<VirtioInput>() else {
                return;
            };
            input.inject_rel_move(dx, dy);
        }
        self.process_virtio_input();
        self.sync_pci_intx_sources_to_interrupts();
    }

    /// Inject a Linux input button event (`EV_KEY` + `BTN_*`) into the virtio-input mouse device.
    ///
    /// This is a no-op when virtio-input is disabled.
    pub fn inject_virtio_button(&mut self, btn: u16, pressed: bool) {
        let Some(mouse) = &self.virtio_input_mouse else {
            return;
        };
        {
            let mut dev = mouse.borrow_mut();
            let Some(input) = dev.device_mut::<VirtioInput>() else {
                return;
            };
            input.inject_button(btn, pressed);
        }
        self.process_virtio_input();
        self.sync_pci_intx_sources_to_interrupts();
    }

    /// Inject a Linux mouse wheel event (`EV_REL` + `REL_WHEEL`) into the virtio-input mouse device.
    ///
    /// This is a no-op when virtio-input is disabled.
    pub fn inject_virtio_wheel(&mut self, delta: i32) {
        let Some(mouse) = &self.virtio_input_mouse else {
            return;
        };
        {
            let mut dev = mouse.borrow_mut();
            let Some(input) = dev.device_mut::<VirtioInput>() else {
                return;
            };
            input.inject_wheel(delta);
        }
        self.process_virtio_input();
        self.sync_pci_intx_sources_to_interrupts();
    }

    // Explicit aliases for parity with the wasm-facing API.
    pub fn inject_virtio_mouse_rel(&mut self, dx: i32, dy: i32) {
        self.inject_virtio_rel(dx, dy);
    }

    pub fn inject_virtio_mouse_button(&mut self, btn: u32, pressed: bool) {
        let Ok(btn) = u16::try_from(btn) else {
            return;
        };
        self.inject_virtio_button(btn, pressed);
    }

    pub fn inject_virtio_mouse_wheel(&mut self, delta: i32) {
        self.inject_virtio_wheel(delta);
    }
    /// Inject a Linux mouse horizontal wheel event (`EV_REL` + `REL_HWHEEL`) into the virtio-input
    /// mouse device.
    ///
    /// This is a no-op when virtio-input is disabled.
    pub fn inject_virtio_hwheel(&mut self, delta: i32) {
        let Some(mouse) = &self.virtio_input_mouse else {
            return;
        };
        {
            let mut dev = mouse.borrow_mut();
            let Some(input) = dev.device_mut::<VirtioInput>() else {
                return;
            };
            input.inject_hwheel(delta);
        }
        self.process_virtio_input();
        self.sync_pci_intx_sources_to_interrupts();
    }

    /// Inject a Linux mouse vertical + horizontal wheel update into the virtio-input mouse device.
    ///
    /// This uses a single `SYN_REPORT`, matching how physical devices may report both axes within
    /// one frame.
    ///
    /// This is a no-op when virtio-input is disabled.
    pub fn inject_virtio_wheel2(&mut self, wheel: i32, hwheel: i32) {
        let Some(mouse) = &self.virtio_input_mouse else {
            return;
        };
        {
            let mut dev = mouse.borrow_mut();
            let Some(input) = dev.device_mut::<VirtioInput>() else {
                return;
            };
            input.inject_wheel2(wheel, hwheel);
        }
        self.process_virtio_input();
        self.sync_pci_intx_sources_to_interrupts();
    }

    // ---------------------------------------------------------------------
    // Synthetic USB HID devices (UHCI external hub)
    // ---------------------------------------------------------------------

    /// Inject a USB HID keyboard usage into the machine's synthetic USB keyboard (if enabled).
    ///
    /// `usage` is the USB HID usage ID (Keyboard page).
    pub fn inject_usb_hid_keyboard_usage(&mut self, usage: u8, pressed: bool) {
        if let Some(kbd) = &self.usb_hid_keyboard {
            kbd.key_event(usage, pressed);
        }
    }

    /// Inject relative mouse motion into the machine's synthetic USB HID mouse (if enabled).
    ///
    /// Coordinate conventions match USB HID (and browser events): `dx > 0` is right and `dy > 0` is
    /// down.
    pub fn inject_usb_hid_mouse_move(&mut self, dx: i32, dy: i32) {
        if let Some(mouse) = &self.usb_hid_mouse {
            mouse.movement(dx, dy);
        }
    }

    /// Inject a USB HID mouse button mask into the machine's synthetic USB HID mouse (if enabled).
    ///
    /// `mask` matches the DOM `MouseEvent.buttons` bitmask:
    /// - bit 0: left
    /// - bit 1: right
    /// - bit 2: middle
    /// - bit 3: back
    /// - bit 4: forward
    pub fn inject_usb_hid_mouse_buttons(&mut self, mask: u8) {
        let buttons = mask & 0x1f;
        if let Some(mouse) = &self.usb_hid_mouse {
            // `UsbHidMouseHandle` operates on per-bit transitions; drive it with a two-step
            // "release then press" update so the final state matches the requested absolute mask.
            let clear = (!buttons) & 0x1f;
            if clear != 0 {
                mouse.button_event(clear, false);
            }
            if buttons != 0 {
                mouse.button_event(buttons, true);
            }
        }
    }

    /// Inject a USB HID mouse wheel delta into the machine's synthetic USB HID mouse (if enabled).
    ///
    /// Positive values represent wheel-up (matching PS/2 wheel conventions used by the browser
    /// runtime input batching).
    pub fn inject_usb_hid_mouse_wheel(&mut self, delta: i32) {
        if let Some(mouse) = &self.usb_hid_mouse {
            mouse.wheel(delta);
        }
    }

    /// Inject a USB HID mouse horizontal wheel (AC Pan) delta into the machine's synthetic USB HID
    /// mouse (if enabled).
    ///
    /// Positive values represent wheel-right.
    pub fn inject_usb_hid_mouse_hwheel(&mut self, delta: i32) {
        if let Some(mouse) = &self.usb_hid_mouse {
            mouse.hwheel(delta);
        }
    }

    /// Inject both vertical and horizontal mouse wheel deltas into the machine's synthetic USB HID
    /// mouse (if enabled).
    ///
    /// This emits a single report containing both axes, matching how physical devices may report
    /// diagonal scrolling.
    ///
    /// Conventions:
    /// - `wheel > 0` means wheel up.
    /// - `hwheel > 0` means wheel right / AC Pan.
    pub fn inject_usb_hid_mouse_wheel2(&mut self, wheel: i32, hwheel: i32) {
        if let Some(mouse) = &self.usb_hid_mouse {
            mouse.wheel2(wheel, hwheel);
        }
    }

    /// Inject an 8-byte USB HID gamepad report into the machine's synthetic USB HID gamepad (if
    /// enabled).
    ///
    /// The report bytes are packed into two little-endian `u32` words:
    /// - `packed0_3_le`: bytes 0..=3
    /// - `packed4_7_le`: bytes 4..=7
    pub fn inject_usb_hid_gamepad_report(&mut self, packed0_3_le: u32, packed4_7_le: u32) {
        let Some(gamepad) = &self.usb_hid_gamepad else {
            return;
        };

        let a = packed0_3_le.to_le_bytes();
        let b = packed4_7_le.to_le_bytes();
        let buttons = u16::from_le_bytes([a[0], a[1]]);
        let hat = a[2];
        let x = a[3] as i8;
        let y = b[0] as i8;
        let rx = b[1] as i8;
        let ry = b[2] as i8;

        gamepad.set_report(GamepadReport {
            buttons,
            hat,
            x,
            y,
            rx,
            ry,
        });
    }

    /// Inject a USB HID Consumer Control usage event into the machine's synthetic consumer-control
    /// device (if enabled).
    ///
    /// `usage` is a HID usage ID from Usage Page 0x0C ("Consumer"). The synthetic consumer-control
    /// device currently supports usages in the range `1..=0x03FF` (see
    /// `crates/aero-usb/src/hid/consumer_control.rs`).
    pub fn inject_usb_hid_consumer_usage(&mut self, usage: u32, pressed: bool) {
        let Some(consumer) = &self.usb_hid_consumer_control else {
            return;
        };
        let Ok(usage) = u16::try_from(usage) else {
            return;
        };
        if !(1..=0x03ff).contains(&usage) {
            return;
        }
        consumer.consumer_event(usage, pressed);
    }

    // ---------------------------------------------------------------------
    // Input batching (InputEventQueue wire format)
    // ---------------------------------------------------------------------

    /// Inject a batch of input events encoded in the `InputEventQueue` wire format used by the
    /// web runtime.
    ///
    /// ## Wire format
    ///
    /// The input batch is a little-endian `u32` word stream:
    ///
    /// - Header (2 words): `[count, batch_timestamp_us]`
    /// - Events: `count` entries, each 4 words: `[ty, event_timestamp_us, a, b]`
    ///
    /// Event type values are defined by `web/src/input/event_queue.ts::InputEventType`.
    ///
    /// ## Defensive parsing
    ///
    /// - Malformed/truncated buffers are ignored without panicking.
    /// - Unknown event types are ignored.
    /// - Per-call work is capped to keep worst-case runtime bounded.
    pub fn inject_input_batch(&mut self, words: &[u32]) {
        // Keep these values in sync with `web/src/input/event_queue.ts`.
        const TYPE_KEY_SCANCODE: u32 = 1;
        const TYPE_MOUSE_MOVE: u32 = 2;
        const TYPE_MOUSE_BUTTONS: u32 = 3;
        const TYPE_MOUSE_WHEEL: u32 = 4;
        const TYPE_GAMEPAD_REPORT: u32 = 5;
        const TYPE_KEY_HID_USAGE: u32 = 6;
        const TYPE_HID_USAGE16: u32 = 7;

        const HEADER_WORDS: usize = 2;
        const WORDS_PER_EVENT: usize = 4;

        // Cap the amount of work performed per batch, even if the caller passes a huge buffer.
        // This bounds CPU time for hostile/malformed input.
        const MAX_EVENTS_PER_BATCH: usize = 4096;

        if words.len() < HEADER_WORDS {
            return;
        }

        // Word 0 is written as an `i32` by the JS runtime but represents a count. Treat it as a
        // wrapping `u32` and clamp it to the buffer bounds below.
        let declared_count = words[0] as usize;
        if declared_count == 0 {
            return;
        }

        let available_events = (words.len().saturating_sub(HEADER_WORDS)) / WORDS_PER_EVENT;
        let count = declared_count
            .min(available_events)
            .min(MAX_EVENTS_PER_BATCH);

        // Routing policy mirrors the browser worker runtime at a high level:
        // - Keyboard: virtio-input (DRIVER_OK) → synthetic USB HID keyboard (once configured) → PS/2 i8042.
        // - Mouse: virtio-input (DRIVER_OK) → PS/2 until the synthetic USB mouse is configured → USB HID.
        // - Gamepad: synthetic USB HID gamepad (no PS/2 fallback).
        let ps2_available = self.i8042.is_some();

        let virtio_keyboard_driver_ok = self.virtio_input_keyboard_driver_ok();
        let virtio_mouse_driver_ok = self.virtio_input_mouse_driver_ok();

        let usb_keyboard_present = self.usb_hid_keyboard.is_some();
        let usb_keyboard_ready = self
            .usb_hid_keyboard
            .as_ref()
            .is_some_and(|kbd| kbd.configured());
        let usb_mouse_present = self.usb_hid_mouse.is_some();
        let usb_mouse_ready = self
            .usb_hid_mouse
            .as_ref()
            .is_some_and(|mouse| mouse.configured());

        // Keyboard backend selection: keep the backend stable while any key is held down to avoid
        // press/release pairs being delivered to different devices ("stuck keys").
        //
        // This mirrors the browser worker selection logic in `web/src/input/input_backend_selection.ts`.
        let usb_keyboard_ok = if ps2_available {
            usb_keyboard_ready
        } else {
            usb_keyboard_present
        };
        let keyboard_keys_held = self.input_batch_pressed_keyboard_usage_count != 0
            || self.consumer_usage_backend.iter().any(|&b| b != 0);
        if !keyboard_keys_held {
            self.input_batch_keyboard_backend = if virtio_keyboard_driver_ok {
                2
            } else if usb_keyboard_ok {
                1
            } else {
                0
            };
        }
        let use_virtio_keyboard_hid =
            self.input_batch_keyboard_backend == 2 && virtio_keyboard_driver_ok;
        let use_usb_keyboard = self.input_batch_keyboard_backend == 1 && usb_keyboard_present;
        let use_ps2_keyboard = self.input_batch_keyboard_backend == 0 && ps2_available;

        // Mouse backend selection: keep the backend stable while any button is held down to avoid
        // leaving the previous backend in a latched state (stuck drag).
        let usb_mouse_ok = if ps2_available {
            usb_mouse_ready
        } else {
            usb_mouse_present
        };
        let mouse_buttons_held = self.input_batch_mouse_buttons_mask != 0;
        if !mouse_buttons_held {
            self.input_batch_mouse_backend = if virtio_mouse_driver_ok {
                2
            } else if usb_mouse_ok {
                1
            } else {
                0
            };
        }
        let use_virtio_mouse = self.input_batch_mouse_backend == 2 && virtio_mouse_driver_ok;
        let use_usb_mouse = self.input_batch_mouse_backend == 1 && usb_mouse_present;
        let use_ps2_mouse = self.input_batch_mouse_backend == 0 && ps2_available;
        let mut virtio_input_dirty = false;

        fn hid_usage_to_linux_key(usage: u8) -> Option<u16> {
            use aero_virtio::devices::input::*;
            Some(match usage {
                // Letters.
                0x04 => KEY_A,
                0x05 => KEY_B,
                0x06 => KEY_C,
                0x07 => KEY_D,
                0x08 => KEY_E,
                0x09 => KEY_F,
                0x0A => KEY_G,
                0x0B => KEY_H,
                0x0C => KEY_I,
                0x0D => KEY_J,
                0x0E => KEY_K,
                0x0F => KEY_L,
                0x10 => KEY_M,
                0x11 => KEY_N,
                0x12 => KEY_O,
                0x13 => KEY_P,
                0x14 => KEY_Q,
                0x15 => KEY_R,
                0x16 => KEY_S,
                0x17 => KEY_T,
                0x18 => KEY_U,
                0x19 => KEY_V,
                0x1A => KEY_W,
                0x1B => KEY_X,
                0x1C => KEY_Y,
                0x1D => KEY_Z,

                // Digits.
                0x1E => KEY_1,
                0x1F => KEY_2,
                0x20 => KEY_3,
                0x21 => KEY_4,
                0x22 => KEY_5,
                0x23 => KEY_6,
                0x24 => KEY_7,
                0x25 => KEY_8,
                0x26 => KEY_9,
                0x27 => KEY_0,

                // Basic.
                0x28 => KEY_ENTER,
                0x29 => KEY_ESC,
                0x2A => KEY_BACKSPACE,
                0x2B => KEY_TAB,
                0x2C => KEY_SPACE,
                0x2D => KEY_MINUS,
                0x2E => KEY_EQUAL,
                0x2F => KEY_LEFTBRACE,
                0x30 => KEY_RIGHTBRACE,
                0x31 => KEY_BACKSLASH,
                0x32 => KEY_BACKSLASH, // IntlHash alias
                0x33 => KEY_SEMICOLON,
                0x34 => KEY_APOSTROPHE,
                0x35 => KEY_GRAVE,
                0x36 => KEY_COMMA,
                0x37 => KEY_DOT,
                0x38 => KEY_SLASH,

                // Modifiers.
                0xE0 => KEY_LEFTCTRL,
                0xE1 => KEY_LEFTSHIFT,
                0xE2 => KEY_LEFTALT,
                0xE3 => KEY_LEFTMETA,
                0xE4 => KEY_RIGHTCTRL,
                0xE5 => KEY_RIGHTSHIFT,
                0xE6 => KEY_RIGHTALT,
                0xE7 => KEY_RIGHTMETA,

                0x39 => KEY_CAPSLOCK,

                // Function keys.
                0x3A => KEY_F1,
                0x3B => KEY_F2,
                0x3C => KEY_F3,
                0x3D => KEY_F4,
                0x3E => KEY_F5,
                0x3F => KEY_F6,
                0x40 => KEY_F7,
                0x41 => KEY_F8,
                0x42 => KEY_F9,
                0x43 => KEY_F10,
                0x44 => KEY_F11,
                0x45 => KEY_F12,
                0x46 => KEY_SYSRQ,

                // Locks.
                0x47 => KEY_SCROLLLOCK,
                0x48 => KEY_PAUSE,

                // Navigation.
                0x49 => KEY_INSERT,
                0x4A => KEY_HOME,
                0x4B => KEY_PAGEUP,
                0x4C => KEY_DELETE,
                0x4D => KEY_END,
                0x4E => KEY_PAGEDOWN,
                0x4F => KEY_RIGHT,
                0x50 => KEY_LEFT,
                0x51 => KEY_DOWN,
                0x52 => KEY_UP,

                // Keypad.
                0x53 => KEY_NUMLOCK,
                0x54 => KEY_KPSLASH,
                0x55 => KEY_KPASTERISK,
                0x56 => KEY_KPMINUS,
                0x57 => KEY_KPPLUS,
                0x58 => KEY_KPENTER,
                0x59 => KEY_KP1,
                0x5A => KEY_KP2,
                0x5B => KEY_KP3,
                0x5C => KEY_KP4,
                0x5D => KEY_KP5,
                0x5E => KEY_KP6,
                0x5F => KEY_KP7,
                0x60 => KEY_KP8,
                0x61 => KEY_KP9,
                0x62 => KEY_KP0,
                0x63 => KEY_KPDOT,
                0x64 => KEY_102ND,
                0x65 => KEY_MENU,
                0x67 => KEY_KPEQUAL,
                0x85 => KEY_KPCOMMA,
                0x87 => KEY_RO,
                0x89 => KEY_YEN,

                _ => return None,
            })
        }

        fn hid_consumer_usage_to_linux_key(usage: u16) -> Option<u16> {
            use aero_virtio::devices::input::*;
            // Keep this mapping aligned with the JS helper `hidConsumerUsageToLinuxKeyCode`
            // (`web/src/io/devices/virtio_input.ts`).
            Some(match usage {
                0x00e2 => KEY_MUTE,
                0x00ea => KEY_VOLUMEDOWN,
                0x00e9 => KEY_VOLUMEUP,
                0x00cd => KEY_PLAYPAUSE,
                0x00b5 => KEY_NEXTSONG,
                0x00b6 => KEY_PREVIOUSSONG,
                0x00b7 => KEY_STOPCD,
                _ => return None,
            })
        }

        for i in 0..count {
            let off = HEADER_WORDS + i * WORDS_PER_EVENT;
            let ty = words[off];
            // `event_timestamp_us` is currently unused by the machine; keep parsing in case future
            // runtimes want to drive guest-time heuristics from it.
            let _event_timestamp_us = words[off + 1];
            let a = words[off + 2];
            let b = words[off + 3];

            match ty {
                TYPE_KEY_SCANCODE => {
                    if !use_ps2_keyboard {
                        continue;
                    }
                    // Payload:
                    //   a = packed bytes (little-endian, b0 in bits 0..7)
                    //   b = byte length (1..4)
                    let len = b as usize;
                    if len == 0 || len > 4 {
                        continue;
                    }
                    let mut bytes = [0u8; 4];
                    for (j, slot) in bytes.iter_mut().enumerate().take(len) {
                        *slot = ((a >> (j * 8)) & 0xff) as u8;
                    }
                    self.inject_key_scancode_bytes(&bytes[..len]);
                }
                TYPE_KEY_HID_USAGE => {
                    // Payload:
                    //   a = (usage & 0xFF) | ((pressed ? 1 : 0) << 8)
                    //   b = unused
                    let usage = (a & 0xff) as u8;
                    let pressed = ((a >> 8) & 1) != 0;

                    // Track pressed keyboard usages regardless of the current backend selection so
                    // backend switching can be gated on "any key is held".
                    let idx = usage as usize;
                    let prev_pressed = self.input_batch_pressed_keyboard_usages[idx] != 0;
                    if pressed {
                        if !prev_pressed {
                            self.input_batch_pressed_keyboard_usages[idx] = 1;
                            self.input_batch_pressed_keyboard_usage_count = self
                                .input_batch_pressed_keyboard_usage_count
                                .saturating_add(1);
                        }
                    } else if prev_pressed {
                        self.input_batch_pressed_keyboard_usages[idx] = 0;
                        self.input_batch_pressed_keyboard_usage_count = self
                            .input_batch_pressed_keyboard_usage_count
                            .saturating_sub(1);
                    }

                    if !pressed && !prev_pressed {
                        // Unknown key-up: best-effort clear both backends. This can happen after a
                        // snapshot restore (host-side pressed-key tracking is not part of the
                        // snapshot format), where the guest still has a key held down but the host
                        // only sends the release event after restore.
                        if usb_keyboard_present {
                            self.inject_usb_hid_keyboard_usage(usage, false);
                        }
                        if virtio_keyboard_driver_ok {
                            if let Some(code) = hid_usage_to_linux_key(usage) {
                                if let Some(kbd) = &self.virtio_input_keyboard {
                                    let mut dev = kbd.borrow_mut();
                                    if let Some(input) = dev.device_mut::<VirtioInput>() {
                                        input.inject_key(code, false);
                                        virtio_input_dirty = true;
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    if use_virtio_keyboard_hid {
                        let Some(code) = hid_usage_to_linux_key(usage) else {
                            continue;
                        };
                        let Some(kbd) = &self.virtio_input_keyboard else {
                            continue;
                        };
                        let mut dev = kbd.borrow_mut();
                        let Some(input) = dev.device_mut::<VirtioInput>() else {
                            continue;
                        };
                        input.inject_key(code, pressed);
                        virtio_input_dirty = true;
                    } else if use_usb_keyboard {
                        self.inject_usb_hid_keyboard_usage(usage, pressed);
                    }
                }
                TYPE_HID_USAGE16 => {
                    // Payload:
                    //   a = (usagePage & 0xFFFF) | ((pressed ? 1 : 0) << 16)
                    //   b = usageId & 0xFFFF
                    let usage_page = (a & 0xffff) as u16;
                    let pressed = ((a >> 16) & 1) != 0;
                    let usage_id = b & 0xffff;

                    // Consumer Control (0x0C) can be delivered either via:
                    // - virtio-input keyboard (media keys subset, represented as Linux `KEY_*` codes), or
                    // - a dedicated synthetic USB HID consumer-control device (supports full usage IDs).
                    if usage_page == 0x000c {
                        // Usage 0 means "no control"; ignore it. The synthetic consumer-control
                        // device only supports `1..=0x03FF`.
                        if usage_id == 0 || usage_id > 0x03ff {
                            continue;
                        }
                        // Route the usage via the best available backend, ensuring the press+release
                        // pair is delivered to the same backend even if virtio-input becomes ready
                        // between calls.
                        let idx = usage_id as usize;
                        if idx < self.consumer_usage_backend.len() {
                            const BACKEND_USB: u8 = 1;
                            const BACKEND_VIRTIO: u8 = 2;
                            let prev = self.consumer_usage_backend[idx];

                            if pressed {
                                if prev != 0 {
                                    // Duplicate keydown; ignore.
                                    continue;
                                }

                                // Prefer virtio-input when the virtio keyboard driver is active and the
                                // usage is representable as a Linux `KEY_*` code (media keys subset).
                                if virtio_keyboard_driver_ok {
                                    let usage16 = usage_id as u16;
                                    if let Some(code) = hid_consumer_usage_to_linux_key(usage16) {
                                        let Some(kbd) = &self.virtio_input_keyboard else {
                                            continue;
                                        };
                                        let mut dev = kbd.borrow_mut();
                                        let Some(input) = dev.device_mut::<VirtioInput>() else {
                                            continue;
                                        };
                                        input.inject_key(code, true);
                                        virtio_input_dirty = true;
                                        self.consumer_usage_backend[idx] = BACKEND_VIRTIO;
                                        continue;
                                    }
                                }

                                // Otherwise fall back to the synthetic USB consumer-control device (when
                                // available). This handles browser navigation keys (AC Back/Forward/etc.)
                                // which are not currently modeled by the virtio-input keyboard.
                                if self.usb_hid_consumer_control.is_some() {
                                    self.inject_usb_hid_consumer_usage(usage_id, true);
                                    self.consumer_usage_backend[idx] = BACKEND_USB;
                                }
                                continue;
                            }

                            // Release: prefer the backend that handled the press.
                            match prev {
                                BACKEND_VIRTIO => {
                                    let usage16 = usage_id as u16;
                                    if let Some(code) = hid_consumer_usage_to_linux_key(usage16) {
                                        let Some(kbd) = &self.virtio_input_keyboard else {
                                            continue;
                                        };
                                        let mut dev = kbd.borrow_mut();
                                        let Some(input) = dev.device_mut::<VirtioInput>() else {
                                            continue;
                                        };
                                        input.inject_key(code, false);
                                        virtio_input_dirty = true;
                                    }
                                }
                                BACKEND_USB => {
                                    self.inject_usb_hid_consumer_usage(usage_id, false);
                                }
                                _ => {
                                    // Unknown backend (e.g. snapshot restored into a new machine).
                                    // Best-effort release both backends so we don't leave the guest
                                    // stuck in a pressed state.
                                    let usage16 = usage_id as u16;
                                    if let Some(code) = hid_consumer_usage_to_linux_key(usage16) {
                                        if let Some(kbd) = &self.virtio_input_keyboard {
                                            let mut dev = kbd.borrow_mut();
                                            if let Some(input) = dev.device_mut::<VirtioInput>() {
                                                input.inject_key(code, false);
                                                virtio_input_dirty = true;
                                            }
                                        }
                                    }
                                    self.inject_usb_hid_consumer_usage(usage_id, false);
                                }
                            }
                            self.consumer_usage_backend[idx] = 0;
                        } else {
                            // Out-of-range consumer usage: best-effort fall back to USB.
                            self.inject_usb_hid_consumer_usage(usage_id, pressed);
                        }
                    }
                }
                TYPE_MOUSE_MOVE => {
                    let dx = a as i32;
                    let dy_ps2 = b as i32;
                    let dy_down = 0i32.saturating_sub(dy_ps2);
                    if use_ps2_mouse {
                        // Payload:
                        //   a = dx (signed 32-bit)
                        //   b = dy (signed 32-bit), positive = up (PS/2 convention)
                        self.inject_ps2_mouse_motion(dx, dy_ps2, 0);
                    } else if use_virtio_mouse {
                        // virtio-input uses Linux REL_Y where positive = down.
                        let Some(mouse) = &self.virtio_input_mouse else {
                            continue;
                        };
                        let mut dev = mouse.borrow_mut();
                        let Some(input) = dev.device_mut::<VirtioInput>() else {
                            continue;
                        };
                        input.inject_rel_move(dx, dy_down);
                        virtio_input_dirty = true;
                    } else if use_usb_mouse {
                        // USB HID follows DOM/browser convention where +Y is down.
                        self.inject_usb_hid_mouse_move(dx, dy_down);
                    }
                }
                TYPE_MOUSE_BUTTONS => {
                    // Payload:
                    //   a = buttons bitmask (low 8 bits represent up to 8 buttons).
                    //
                    // DOM `MouseEvent.buttons` typically only uses bits 0..4, but the
                    // InputEventQueue wire format supports up to 8 buttons for completeness.
                    let next = (a & 0xff) as u8;
                    let prev_batch_mask = self.input_batch_mouse_buttons_mask;
                    self.input_batch_mouse_buttons_mask = next;

                    if prev_batch_mask == 0 && next == 0 {
                        // Unknown "all released" snapshot: best-effort clear all mouse backends.
                        // Like keyboards, host-side held-state tracking is not part of the snapshot
                        // format, so after restore we may only observe the release event.
                        if self.i8042.is_some() {
                            self.inject_ps2_mouse_buttons(0);
                        }
                        if self.usb_hid_mouse.is_some() {
                            self.inject_usb_hid_mouse_buttons(0);
                        }
                        if virtio_mouse_driver_ok {
                            if let Some(mouse) = &self.virtio_input_mouse {
                                let mut dev = mouse.borrow_mut();
                                if let Some(input) = dev.device_mut::<VirtioInput>() {
                                    use aero_virtio::devices::input::*;
                                    input.inject_button(BTN_LEFT, false);
                                    input.inject_button(BTN_RIGHT, false);
                                    input.inject_button(BTN_MIDDLE, false);
                                    input.inject_button(BTN_SIDE, false);
                                    input.inject_button(BTN_EXTRA, false);
                                    input.inject_button(BTN_FORWARD, false);
                                    input.inject_button(BTN_BACK, false);
                                    input.inject_button(BTN_TASK, false);
                                    virtio_input_dirty = true;
                                }
                            }
                        }
                        // Clear any invalid marker used by other mouse injection helpers.
                        self.ps2_mouse_buttons = 0;
                        continue;
                    }
                    if use_ps2_mouse {
                        // Payload:
                        //   a = buttons bitmask (low 5 bits match DOM `MouseEvent.buttons`)
                        self.inject_ps2_mouse_buttons(next & 0x1f);
                    } else if use_virtio_mouse {
                        let prev = self.ps2_mouse_buttons;
                        let changed = prev ^ next;
                        if changed == 0 {
                            // Clear any invalid marker (e.g. post snapshot restore).
                            self.ps2_mouse_buttons = next;
                            continue;
                        }

                        let Some(mouse) = &self.virtio_input_mouse else {
                            continue;
                        };
                        let mut dev = mouse.borrow_mut();
                        let Some(input) = dev.device_mut::<VirtioInput>() else {
                            continue;
                        };
                        // Match DOM `MouseEvent.buttons` bit mapping.
                        if (changed & 0x01) != 0 {
                            input.inject_button(
                                aero_virtio::devices::input::BTN_LEFT,
                                next & 0x01 != 0,
                            );
                        }
                        if (changed & 0x02) != 0 {
                            input.inject_button(
                                aero_virtio::devices::input::BTN_RIGHT,
                                next & 0x02 != 0,
                            );
                        }
                        if (changed & 0x04) != 0 {
                            input.inject_button(
                                aero_virtio::devices::input::BTN_MIDDLE,
                                next & 0x04 != 0,
                            );
                        }
                        if (changed & 0x08) != 0 {
                            input.inject_button(
                                aero_virtio::devices::input::BTN_SIDE,
                                next & 0x08 != 0,
                            );
                        }
                        if (changed & 0x10) != 0 {
                            input.inject_button(
                                aero_virtio::devices::input::BTN_EXTRA,
                                next & 0x10 != 0,
                            );
                        }
                        // Additional mouse buttons (6..8). These are not typically surfaced via
                        // DOM `MouseEvent.buttons`, but are supported by the input batch wire
                        // format and advertised by Aero's virtio-input mouse device model.
                        if (changed & 0x20) != 0 {
                            input.inject_button(
                                aero_virtio::devices::input::BTN_FORWARD,
                                next & 0x20 != 0,
                            );
                        }
                        if (changed & 0x40) != 0 {
                            input.inject_button(
                                aero_virtio::devices::input::BTN_BACK,
                                next & 0x40 != 0,
                            );
                        }
                        if (changed & 0x80) != 0 {
                            input.inject_button(
                                aero_virtio::devices::input::BTN_TASK,
                                next & 0x80 != 0,
                            );
                        }
                        self.ps2_mouse_buttons = next;
                        virtio_input_dirty = true;
                    } else if use_usb_mouse {
                        self.inject_usb_hid_mouse_buttons(next & 0x1f);
                        self.ps2_mouse_buttons = next;
                    }
                }
                TYPE_MOUSE_WHEEL => {
                    let dz = a as i32;
                    let dx = b as i32;
                    if use_ps2_mouse {
                        // Payload:
                        //   a = dz (signed 32-bit), positive = wheel up
                        let _ = dx;
                        self.inject_ps2_mouse_motion(0, 0, dz);
                    } else if use_virtio_mouse {
                        let Some(mouse) = &self.virtio_input_mouse else {
                            continue;
                        };
                        let mut dev = mouse.borrow_mut();
                        let Some(input) = dev.device_mut::<VirtioInput>() else {
                            continue;
                        };
                        input.inject_wheel2(dz, dx);
                        virtio_input_dirty = true;
                    } else if use_usb_mouse {
                        self.inject_usb_hid_mouse_wheel2(dz, dx);
                    }
                }
                TYPE_GAMEPAD_REPORT => {
                    self.inject_usb_hid_gamepad_report(a, b);
                }
                _ => {
                    // Unknown event type; ignore.
                }
            }
        }

        // Re-evaluate keyboard backend selection after processing the batch: key-up events can make
        // it safe to switch away from PS/2 or USB injection.
        let keyboard_keys_held_after = self.input_batch_pressed_keyboard_usage_count != 0
            || self.consumer_usage_backend.iter().any(|&b| b != 0);
        if !keyboard_keys_held_after {
            self.input_batch_keyboard_backend = if virtio_keyboard_driver_ok {
                2
            } else if usb_keyboard_ok {
                1
            } else {
                0
            };
        }

        // Re-evaluate mouse backend selection after processing the batch: button-up events can make
        // it safe to switch away from PS/2 or USB injection.
        if self.input_batch_mouse_buttons_mask == 0 {
            self.input_batch_mouse_backend = if virtio_mouse_driver_ok {
                2
            } else if usb_mouse_ok {
                1
            } else {
                0
            };
        }

        if virtio_input_dirty {
            // Poll once to forward any newly enqueued input events into guest virtqueues.
            self.process_virtio_input();
            // Ensure legacy INTx routing reflects any newly asserted virtio IRQ latch immediately,
            // without requiring a subsequent `run_slice` call.
            self.sync_pci_intx_sources_to_interrupts();
        }
    }
    pub fn take_snapshot_full(&mut self) -> snapshot::Result<Vec<u8>> {
        self.take_snapshot_with_options(snapshot::SaveOptions::default())
    }

    pub fn save_snapshot_full_to<W: Write + Seek>(&mut self, w: &mut W) -> snapshot::Result<()> {
        self.save_snapshot_to(w, snapshot::SaveOptions::default())
    }

    pub fn take_snapshot_dirty(&mut self) -> snapshot::Result<Vec<u8>> {
        let mut options = snapshot::SaveOptions::default();
        options.ram.mode = snapshot::RamMode::Dirty;
        self.take_snapshot_with_options(options)
    }

    pub fn save_snapshot_dirty_to<W: Write + Seek>(&mut self, w: &mut W) -> snapshot::Result<()> {
        let mut options = snapshot::SaveOptions::default();
        options.ram.mode = snapshot::RamMode::Dirty;
        self.save_snapshot_to(w, options)
    }

    pub fn restore_snapshot_bytes(&mut self, bytes: &[u8]) -> snapshot::Result<()> {
        self.restore_snapshot_from_checked(&mut Cursor::new(bytes))
    }

    pub fn restore_snapshot_from<R: Read>(&mut self, r: &mut R) -> snapshot::Result<()> {
        // Clear restore-only state before applying new sections.
        self.restored_disk_overlays = None;
        snapshot::restore_snapshot(r, self)
    }

    pub fn restore_snapshot_from_checked<R: Read + Seek>(
        &mut self,
        r: &mut R,
    ) -> snapshot::Result<()> {
        // Restoring a snapshot is conceptually "rewinding time", so discard any accumulated host
        // output/state from the current execution.
        self.detach_network();
        self.flush_serial();
        if let Some(uart) = &self.serial {
            let _ = uart.borrow_mut().take_tx();
        }
        self.serial_log.clear();
        self.debugcon_log.borrow_mut().clear();
        self.reset_latch.clear();
        // Clear restore-only state before applying new snapshot sections.
        self.restored_disk_overlays = None;

        let expected_parent_snapshot_id = self.last_snapshot_id;
        snapshot::restore_snapshot_with_options(
            r,
            self,
            snapshot::RestoreOptions {
                expected_parent_snapshot_id,
            },
        )
    }

    fn save_snapshot_to<W: Write + Seek>(
        &mut self,
        w: &mut W,
        options: snapshot::SaveOptions,
    ) -> snapshot::Result<()> {
        self.flush_serial();
        snapshot::save_snapshot(w, self, options)
    }

    fn take_snapshot_with_options(
        &mut self,
        options: snapshot::SaveOptions,
    ) -> snapshot::Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        self.save_snapshot_to(&mut cursor, options)?;
        Ok(cursor.into_inner())
    }

    /// Reset the machine and transfer control to firmware POST (boot sector).
    pub fn reset(&mut self) {
        self.reset_latch.clear();
        self.serial_log.clear();
        self.debugcon_log.borrow_mut().clear();
        self.ps2_mouse_buttons = 0;
        self.consumer_usage_backend.fill(0);
        self.input_batch_pressed_keyboard_usages.fill(0);
        self.input_batch_pressed_keyboard_usage_count = 0;
        self.input_batch_keyboard_backend = 0;
        self.input_batch_mouse_buttons_mask = 0;
        self.input_batch_mouse_backend = 0;
        self.guest_time.reset();
        self.uhci_ns_remainder = 0;
        self.ehci_ns_remainder = 0;
        self.xhci_ns_remainder = 0;
        self.restored_disk_overlays = None;
        self.display_fb.clear();
        self.display_width = 0;
        self.display_height = 0;
        self.ide_irq14_line = None;
        self.ide_irq15_line = None;

        // Reset chipset lines.
        self.chipset.a20().set_enabled(false);

        // Rebuild port I/O devices for deterministic power-on state.
        self.io = IoPortBus::new();
        // Expose the Bochs/QEMU-style debug console port (0xE9) for low-overhead early-boot output.
        if self.cfg.enable_debugcon {
            register_debugcon(&mut self.io, self.debugcon_log.clone());
        }

        let use_legacy_vga = self.cfg.enable_vga && !self.cfg.enable_aerogpu;

        // `enable_vga` controls whether the machine wires the legacy VGA/VBE device model.
        //
        // Note: The physical memory bus persists across `Machine::reset()` calls, so any MMIO
        // mappings that reference the VGA device must keep a stable `Rc` identity across resets.
        // When VGA is disabled we drop the device handle, but any previously-installed MMIO
        // mappings (if the machine was ever reset with VGA enabled) will still point at the old
        // instance.
        if use_legacy_vga && !self.cfg.enable_pc_platform {
            // VGA is a special legacy device whose MMIO window lives in the low 1MiB region. The
            // physical bus supports MMIO overlays on top of RAM, so mapping this window is safe
            // even when guest RAM is a dense `[0, ram_size_bytes)` range.
            const VGA_LEGACY_MMIO_BASE: u64 = aero_gpu_vga::VGA_LEGACY_MEM_START as u64;
            const VGA_LEGACY_MMIO_SIZE: u64 = aero_gpu_vga::VGA_LEGACY_MEM_LEN as u64;

            // Shared device instances must remain stable across resets because MMIO mappings in the
            // physical memory bus persist. Reset device state in-place while keeping `Rc`
            // identities stable.
            let vga: Rc<RefCell<VgaDevice>> = match &self.vga {
                Some(vga) => {
                    let cfg = vga.borrow().config();
                    *vga.borrow_mut() = VgaDevice::new_with_config(cfg);
                    vga.clone()
                }
                None => {
                    let vga = Rc::new(RefCell::new(VgaDevice::new_with_config(
                        self.legacy_vga_device_config(),
                    )));
                    self.vga = Some(vga.clone());
                    vga
                }
            };

            self.mem
                .map_mmio_once(VGA_LEGACY_MMIO_BASE, VGA_LEGACY_MMIO_SIZE, || {
                    Box::new(VgaLegacyMmioHandler {
                        base_paddr: VGA_LEGACY_MMIO_BASE as u32,
                        dev: vga.clone(),
                    })
                });

            // Register VGA ports (attribute/sequencer/graphics/CRTC/DAC + Bochs VBE_DISPI).
            self.io.register_range(
                aero_gpu_vga::VGA_LEGACY_IO_START,
                aero_gpu_vga::VGA_LEGACY_IO_LEN,
                Box::new(VgaPortIoDevice { dev: vga.clone() }),
            );
            self.io.register_range(
                aero_gpu_vga::VBE_DISPI_IO_START,
                aero_gpu_vga::VBE_DISPI_IO_LEN,
                Box::new(VgaPortIoDevice { dev: vga.clone() }),
            );

            // Map the VBE/SVGA linear framebuffer (LFB) at the VGA device's configured base.
            let (lfb_base, lfb_len) = {
                let vga = vga.borrow();
                (u64::from(vga.lfb_base()), vga.vram_size() as u64)
            };
            self.mem.map_mmio_once(lfb_base, lfb_len, || {
                Box::new(VgaLfbMmioHandler { dev: vga.clone() })
            });
        } else if !self.cfg.enable_pc_platform {
            self.vga = None;
        }

        if self.cfg.enable_pc_platform {
            // PC platform shared device instances must remain stable across resets because MMIO
            // mappings in the physical memory bus persist. Reset device state in-place while
            // keeping `Rc` identities stable.

            // Deterministic clock: reset back to 0 ns.
            let clock = match &self.platform_clock {
                Some(clock) => {
                    clock.set_ns(0);
                    clock.clone()
                }
                None => {
                    let clock = ManualClock::new();
                    self.platform_clock = Some(clock.clone());
                    clock
                }
            };

            // Interrupt controller complex (PIC + IOAPIC + LAPIC).
            let interrupts: Rc<RefCell<PlatformInterrupts>> = match &self.interrupts {
                Some(ints) => {
                    ints.borrow_mut().reset();
                    ints.clone()
                }
                None => {
                    let ints = Rc::new(RefCell::new(PlatformInterrupts::new_with_cpu_count(
                        self.cfg.cpu_count,
                    )));
                    self.interrupts = Some(ints.clone());
                    ints
                }
            };

            if self.cfg.enable_ide {
                // IDE legacy compatibility mode uses ISA IRQ14/IRQ15 rather than PCI INTx.
                //
                // Drive these lines through `PlatformIrqLine` so repeated polling does not
                // over/under-count assertions in the ref-counted `PlatformInterrupts` sink.
                self.ide_irq14_line = Some(PlatformIrqLine::isa(interrupts.clone(), 14));
                self.ide_irq15_line = Some(PlatformIrqLine::isa(interrupts.clone(), 15));
            }

            PlatformInterrupts::register_imcr_ports(&mut self.io, interrupts.clone());
            register_pic8259_on_platform_interrupts(&mut self.io, interrupts.clone());

            let dma = Rc::new(RefCell::new(Dma8237::new()));
            register_dma8237(&mut self.io, dma);

            // PIT 8254.
            let pit: SharedPit8254 = match &self.pit {
                Some(pit) => {
                    *pit.borrow_mut() = Pit8254::new();
                    pit.clone()
                }
                None => {
                    let pit: SharedPit8254 = Rc::new(RefCell::new(Pit8254::new()));
                    self.pit = Some(pit.clone());
                    pit
                }
            };
            pit.borrow_mut()
                .connect_irq0_to_platform_interrupts(interrupts.clone());
            register_pit8254(&mut self.io, pit.clone());

            // RTC CMOS.
            let rtc_irq8 = PlatformIrqLine::isa(interrupts.clone(), 8);
            let rtc: SharedRtcCmos<ManualClock, PlatformIrqLine> = match &self.rtc {
                Some(rtc) => {
                    *rtc.borrow_mut() = RtcCmos::new(clock.clone(), rtc_irq8);
                    rtc.clone()
                }
                None => {
                    let rtc: SharedRtcCmos<ManualClock, PlatformIrqLine> =
                        Rc::new(RefCell::new(RtcCmos::new(clock.clone(), rtc_irq8)));
                    self.rtc = Some(rtc.clone());
                    rtc
                }
            };
            rtc.borrow_mut()
                .set_memory_size_bytes(self.cfg.ram_size_bytes);
            register_rtc_cmos(&mut self.io, rtc.clone());

            // ACPI PM. Wire SCI to ISA IRQ9.
            let acpi_pm: SharedAcpiPmIo<ManualClock> = match &self.acpi_pm {
                Some(acpi_pm) => {
                    acpi_pm.borrow_mut().reset();
                    acpi_pm.clone()
                }
                None => {
                    // Wire ACPI PM to the shared deterministic platform clock so `PM_TMR`
                    // progresses only when the host advances `ManualClock` (via
                    // `Machine::tick_platform`).
                    let acpi_pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
                        AcpiPmConfig::default(),
                        AcpiPmCallbacks {
                            sci_irq: Box::new(PlatformIrqLine::isa(interrupts.clone(), 9)),
                            request_sleep: None,
                            request_power_off: None,
                        },
                        clock.clone(),
                    )));
                    self.acpi_pm = Some(acpi_pm.clone());
                    acpi_pm
                }
            };
            register_acpi_pm(&mut self.io, acpi_pm.clone());

            if use_legacy_vga {
                // VGA/SVGA (VBE). Keep the device instance stable across resets so the MMIO mapping
                // remains valid.
                let vga: Rc<RefCell<VgaDevice>> = match &self.vga {
                    Some(vga) => {
                        let cfg = vga.borrow().config();
                        *vga.borrow_mut() = VgaDevice::new_with_config(cfg);
                        vga.clone()
                    }
                    None => {
                        let vga = Rc::new(RefCell::new(VgaDevice::new_with_config(
                            self.legacy_vga_device_config(),
                        )));
                        self.vga = Some(vga.clone());
                        vga
                    }
                };

                // Register legacy VGA + Bochs VBE ports.
                //
                // - VGA: 0x3B0..0x3DF (includes both mono and color decode ranges)
                // - Bochs VBE: aero_gpu_vga::VBE_DISPI_INDEX_PORT (index),
                //   aero_gpu_vga::VBE_DISPI_DATA_PORT (data)
                self.io.register_shared_range(
                    aero_gpu_vga::VGA_LEGACY_IO_START,
                    aero_gpu_vga::VGA_LEGACY_IO_LEN,
                    {
                        let vga = vga.clone();
                        move |_port| Box::new(VgaPortIoDevice { dev: vga.clone() })
                    },
                );
                self.io.register_shared_range(
                    aero_gpu_vga::VBE_DISPI_IO_START,
                    aero_gpu_vga::VBE_DISPI_IO_LEN,
                    {
                        let vga = vga.clone();
                        move |_port| Box::new(VgaPortIoDevice { dev: vga.clone() })
                    },
                );

                // Map the legacy VGA memory window (`0xA0000..0xC0000`). The SVGA linear framebuffer
                // is routed via PCI BAR/MMIO when the PC platform is enabled, so do not map it
                // directly here.
                let legacy_base = aero_gpu_vga::VGA_LEGACY_MEM_START as u64;
                let legacy_len = aero_gpu_vga::VGA_LEGACY_MEM_LEN as u64;
                self.mem.map_mmio_once(legacy_base, legacy_len, {
                    let vga = vga.clone();
                    move || {
                        Box::new(VgaLegacyMmioHandler {
                            base_paddr: aero_gpu_vga::VGA_LEGACY_MEM_START,
                            dev: vga,
                        })
                    }
                });
            } else {
                self.vga = None;
            }

            if self.cfg.enable_aerogpu {
                let aerogpu: Rc<RefCell<AeroGpuDevice>> = match &self.aerogpu {
                    Some(dev) => {
                        dev.borrow_mut().reset();
                        dev.clone()
                    }
                    None => {
                        let dev = Rc::new(RefCell::new(AeroGpuDevice::new()));
                        self.aerogpu = Some(dev.clone());
                        dev
                    }
                };
                match &self.aerogpu_mmio {
                    Some(dev) => {
                        let mut dev = dev.borrow_mut();
                        dev.reset();
                        dev.set_clock(clock.clone());
                    }
                    None => {
                        let dev = Rc::new(RefCell::new(AeroGpuMmioDevice::default()));
                        dev.borrow_mut().set_clock(clock.clone());
                        self.aerogpu_mmio = Some(dev);
                    }
                }

                // Minimal legacy VGA port decode (`0x3B0..0x3DF`).
                // The PC platform installs a range-based PCI I/O BAR router over most ports, so
                // legacy VGA ports must be wired as exact per-port mappings to avoid overlapping
                // range registrations (and to take precedence over the PCI I/O router).
                self.io.register_shared_range(
                    aero_gpu_vga::VGA_LEGACY_IO_START,
                    aero_gpu_vga::VGA_LEGACY_IO_LEN,
                    {
                        let aerogpu = aerogpu.clone();
                        let clock = clock.clone();
                        move |_port| {
                            Box::new(AeroGpuVgaPortWindow {
                                dev: aerogpu.clone(),
                                clock: clock.clone(),
                            })
                        }
                    },
                );
                self.io.register_shared_range(
                    aero_gpu_vga::VBE_DISPI_IO_START,
                    aero_gpu_vga::VBE_DISPI_IO_LEN,
                    {
                        let aerogpu = aerogpu.clone();
                        move |_port| {
                            Box::new(AeroGpuVbeDispiPortWindow {
                                dev: aerogpu.clone(),
                            })
                        }
                    },
                );

                // Map the legacy VGA memory window (`0xA0000..0xC0000`) as an MMIO overlay that
                // aliases `VRAM[0..128KiB]`.
                self.mem.map_mmio_once(
                    aero_gpu_vga::VGA_LEGACY_MEM_START as u64,
                    LEGACY_VGA_WINDOW_SIZE as u64,
                    {
                        let aerogpu = aerogpu.clone();
                        move || {
                            Box::new(AeroGpuLegacyVgaMmio {
                                dev: aerogpu.clone(),
                            })
                        }
                    },
                );
            } else {
                self.aerogpu = None;
                self.aerogpu_mmio = None;
            }
            // PCI config ports (config mechanism #1).
            let pci_cfg: SharedPciConfigPorts = match &self.pci_cfg {
                Some(pci_cfg) => {
                    *pci_cfg.borrow_mut() = PciConfigPorts::new();
                    pci_cfg.clone()
                }
                None => {
                    let pci_cfg: SharedPciConfigPorts =
                        Rc::new(RefCell::new(PciConfigPorts::new()));
                    self.pci_cfg = Some(pci_cfg.clone());
                    pci_cfg
                }
            };
            register_pci_config_ports(&mut self.io, pci_cfg.clone());
            if self.cfg.enable_aerogpu {
                // Canonical AeroGPU PCI identity contract (`00:07.0`, `A3A0:0001`).
                //
                // Expose the device so BIOS POST assigns BARs and BAR0 MMIO (ring/scanout regs) +
                // BAR1 VRAM are reachable via the PCI BAR MMIO router.
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::AEROGPU.bdf,
                    Box::new(AeroGpuPciConfigDevice::new()),
                );
            }
            // PCI INTx router.
            let pci_intx: Rc<RefCell<PciIntxRouter>> = match &self.pci_intx {
                Some(pci_intx) => {
                    *pci_intx.borrow_mut() = PciIntxRouter::new(PciIntxRouterConfig::default());
                    pci_intx.clone()
                }
                None => {
                    let pci_intx = Rc::new(RefCell::new(PciIntxRouter::new(
                        PciIntxRouterConfig::default(),
                    )));
                    self.pci_intx = Some(pci_intx.clone());
                    pci_intx
                }
            };

            // HPET.
            let hpet: Rc<RefCell<hpet::Hpet<ManualClock>>> = match &self.hpet {
                Some(hpet) => {
                    *hpet.borrow_mut() = hpet::Hpet::new_default(clock.clone());
                    hpet.clone()
                }
                None => {
                    let hpet = Rc::new(RefCell::new(hpet::Hpet::new_default(clock.clone())));
                    self.hpet = Some(hpet.clone());
                    hpet
                }
            };

            let ahci = if self.cfg.enable_ahci {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::SATA_AHCI_ICH9.bdf,
                    Box::new(AhciPciConfigDevice::new()),
                );

                match &self.ahci {
                    Some(ahci) => {
                        // Reset in-place while keeping the `Rc` identity stable for any persistent
                        // MMIO mappings. This intentionally preserves any attached disk backends.
                        ahci.borrow_mut().reset();
                        Some(ahci.clone())
                    }
                    None => {
                        let ahci = Rc::new(RefCell::new(AhciPciDevice::new(1)));
                        // Provide an MSI sink so the device model can inject MSI messages into the
                        // platform LAPIC when the guest enables MSI in PCI config space.
                        ahci.borrow_mut()
                            .set_msi_target(Some(Box::new(interrupts.clone())));
                        // On first initialization, attach the machine's canonical disk backend to
                        // AHCI port 0 so BIOS INT13 and controller-driven access see consistent
                        // bytes by default.
                        let drive = AtaDrive::new(Box::new(self.disk.clone()))
                            .expect("machine disk should be 512-byte aligned");
                        ahci.borrow_mut().attach_drive(0, drive);
                        Some(ahci)
                    }
                }
            } else {
                None
            };

            let nvme = if self.cfg.enable_nvme {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::NVME_CONTROLLER.bdf,
                    Box::new(NvmePciConfigDevice::new()),
                );

                match &self.nvme {
                    Some(nvme) => {
                        // Reset in-place while keeping the `Rc` identity stable for any persistent
                        // MMIO mappings.
                        nvme.borrow_mut().reset();
                        // Ensure an MSI sink is attached so the device can deliver MSI/MSI-X when
                        // the guest enables it.
                        nvme.borrow_mut()
                            .set_msi_target(Some(Box::new(interrupts.clone())));
                        Some(nvme.clone())
                    }
                    None => {
                        let dev =
                            NvmePciDevice::try_new_from_virtual_disk(Box::new(self.disk.clone()))
                                .expect("machine disk should be 512-byte aligned");
                        let nvme = Rc::new(RefCell::new(dev));
                        nvme.borrow_mut()
                            .set_msi_target(Some(Box::new(interrupts.clone())));
                        Some(nvme)
                    }
                }
            } else {
                None
            };

            // PIIX3 is a multi-function PCI device. Ensure function 0 exists and has the
            // multi-function bit set so OSes enumerate the IDE/UHCI functions at 00:01.1/00:01.2
            // reliably.
            if self.cfg.enable_ide || self.cfg.enable_uhci {
                let bdf = aero_devices::pci::profile::ISA_PIIX3.bdf;
                pci_cfg
                    .borrow_mut()
                    .bus_mut()
                    .add_device(bdf, Box::new(Piix3IsaPciConfigDevice::new()));
            }

            // When both UHCI and EHCI controllers are enabled, route the first two EHCI root ports
            // through a shared USB2 mux so CONFIGFLAG/PORT_OWNER handoff moves attached device
            // models between controllers.
            //
            // Important: create the mux only when both controllers are first created. On subsequent
            // machine resets the `Rc` identities for the PCI devices are preserved, so re-creating
            // and re-attaching a new mux would drop any attached USB device models.
            let attach_usb2_mux = self.cfg.enable_uhci
                && self.cfg.enable_ehci
                && self.uhci.is_none()
                && self.ehci.is_none();

            let uhci = if self.cfg.enable_uhci {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::USB_UHCI_PIIX3.bdf,
                    Box::new(UhciPciConfigDevice::new()),
                );
                match &self.uhci {
                    Some(uhci) => {
                        uhci.borrow_mut().reset();
                        Some(uhci.clone())
                    }
                    None => Some(Rc::new(RefCell::new(UhciPciDevice::default()))),
                }
            } else {
                None
            };

            let ehci = if self.cfg.enable_ehci {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::USB_EHCI_ICH9.bdf,
                    Box::new(EhciPciConfigDevice::new()),
                );
                match &self.ehci {
                    Some(ehci) => {
                        ehci.borrow_mut().reset();
                        Some(ehci.clone())
                    }
                    None => Some(Rc::new(RefCell::new(EhciPciDevice::default()))),
                }
            } else {
                None
            };

            if attach_usb2_mux {
                if let (Some(uhci), Some(ehci)) = (&uhci, &ehci) {
                    // UHCI exposes two root hub ports. EHCI defaults to six; mux the first two so
                    // the guest can route them via CONFIGFLAG/PORT_OWNER.
                    let mux = Rc::new(RefCell::new(Usb2PortMux::new(2)));
                    {
                        let mut uhci_dev = uhci.borrow_mut();
                        let hub = uhci_dev.controller_mut().hub_mut();
                        hub.attach_usb2_port_mux(0, mux.clone(), 0);
                        hub.attach_usb2_port_mux(1, mux.clone(), 1);
                    }
                    {
                        let mut ehci_dev = ehci.borrow_mut();
                        let hub = ehci_dev.controller_mut().hub_mut();
                        hub.attach_usb2_port_mux(0, mux.clone(), 0);
                        hub.attach_usb2_port_mux(1, mux.clone(), 1);
                    }
                }
            }

            let xhci = if self.cfg.enable_xhci {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::USB_XHCI_QEMU.bdf,
                    Box::new(XhciPciConfigDevice::new()),
                );
                match &self.xhci {
                    Some(dev) => {
                        dev.borrow_mut().reset();
                        Some(dev.clone())
                    }
                    None => {
                        let dev = Rc::new(RefCell::new(XhciPciDevice::default()));
                        dev.borrow_mut()
                            .set_msi_target(Some(Box::new(interrupts.clone())));
                        Some(dev)
                    }
                }
            } else {
                None
            };

            let ide = if self.cfg.enable_ide {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::IDE_PIIX3.bdf,
                    Box::new(IdePciConfigDevice::new()),
                );
                match &self.ide {
                    Some(ide) => {
                        // Reset in-place while keeping the `Rc` identity stable for any persistent
                        // port I/O mappings. This intentionally preserves any attached disk/ISO
                        // backends so host-provided media remains available across reboots.
                        ide.borrow_mut().reset();
                        Some(ide.clone())
                    }
                    None => Some(Rc::new(RefCell::new(Piix3IdePciDevice::new()))),
                }
            } else {
                None
            };

            let virtio_net = if self.cfg.enable_virtio_net {
                let mac = self
                    .cfg
                    .virtio_net_mac_addr
                    .unwrap_or(DEFAULT_VIRTIO_NET_MAC_ADDR);

                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::VIRTIO_NET.bdf,
                    Box::new(VirtioNetPciConfigDevice::new()),
                );

                let backend = self.virtio_net.as_ref().and_then(|dev| {
                    let mut dev = dev.borrow_mut();
                    dev.device_mut::<VirtioNet<VirtioNetBackendAdapter>>()
                        .and_then(|net| net.backend_mut().take_backend())
                });

                match &self.virtio_net {
                    Some(dev) => {
                        // Reset in-place while keeping `Rc` identity stable for persistent MMIO
                        // mappings.
                        *dev.borrow_mut() = VirtioPciDevice::new(
                            Box::new(VirtioNet::new(VirtioNetBackendAdapter::new(backend), mac)),
                            Box::new(VirtioMsixInterruptSink::new(interrupts.clone())),
                        );
                        Some(dev.clone())
                    }
                    None => Some(Rc::new(RefCell::new(VirtioPciDevice::new(
                        Box::new(VirtioNet::new(VirtioNetBackendAdapter::new(None), mac)),
                        Box::new(VirtioMsixInterruptSink::new(interrupts.clone())),
                    )))),
                }
            } else {
                None
            };

            let virtio_blk = if self.cfg.enable_virtio_blk {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::VIRTIO_BLK.bdf,
                    Box::new(VirtioBlkPciConfigDevice::new()),
                );
                match &self.virtio_blk {
                    Some(dev) => {
                        // Reset in-place while keeping `Rc` identity stable for persistent MMIO
                        // mappings. This intentionally preserves any host-attached disk backend
                        // owned by the inner virtio-blk device.
                        dev.borrow_mut().reset();
                        Some(dev.clone())
                    }
                    None => Some(Rc::new(RefCell::new(VirtioPciDevice::new(
                        Box::new(VirtioBlk::new(Box::new(self.disk.clone()))),
                        Box::new(VirtioMsixInterruptSink::new(interrupts.clone())),
                    )))),
                }
            } else {
                None
            };

            let virtio_input_keyboard = if self.cfg.enable_virtio_input {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD.bdf,
                    Box::new(VirtioInputKeyboardPciConfigDevice::new()),
                );
                match &self.virtio_input_keyboard {
                    Some(dev) => {
                        dev.borrow_mut().reset();
                        Some(dev.clone())
                    }
                    None => Some(Rc::new(RefCell::new(VirtioPciDevice::new(
                        Box::new(VirtioInput::new(VirtioInputDeviceKind::Keyboard)),
                        Box::new(VirtioMsixInterruptSink::new(interrupts.clone())),
                    )))),
                }
            } else {
                None
            };

            let virtio_input_mouse = if self.cfg.enable_virtio_input {
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::VIRTIO_INPUT_MOUSE.bdf,
                    Box::new(VirtioInputMousePciConfigDevice::new()),
                );
                match &self.virtio_input_mouse {
                    Some(dev) => {
                        dev.borrow_mut().reset();
                        Some(dev.clone())
                    }
                    None => Some(Rc::new(RefCell::new(VirtioPciDevice::new(
                        Box::new(VirtioInput::new(VirtioInputDeviceKind::Mouse)),
                        Box::new(VirtioMsixInterruptSink::new(interrupts.clone())),
                    )))),
                }
            } else {
                None
            };

            let e1000 = if self.cfg.enable_e1000 {
                let mac = self.cfg.e1000_mac_addr.unwrap_or(DEFAULT_E1000_MAC_ADDR);
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::NIC_E1000_82540EM.bdf,
                    Box::new(E1000PciConfigDevice::new()),
                );

                match &self.e1000 {
                    Some(e1000) => {
                        // Reset in-place while keeping the `Rc` identity stable for any persistent
                        // MMIO mappings.
                        *e1000.borrow_mut() = E1000Device::new(mac);
                        Some(e1000.clone())
                    }
                    None => Some(Rc::new(RefCell::new(E1000Device::new(mac)))),
                }
            } else {
                None
            };
            // Allocate PCI BAR resources and enable decoding so devices are reachable via MMIO/PIO
            // immediately after reset (without requiring the guest OS to assign BARs first).
            //
            // Note: When the standalone legacy VGA/VBE device model is enabled, the VBE linear
            // framebuffer base lives inside the PCI MMIO window. We reserve that fixed window in
            // the PCI BAR allocator so BIOS POST does not place other devices on top of it now that
            // the transitional VGA PCI stub is gone.
            let legacy_vga_lfb_reservation = if use_legacy_vga {
                let vga = self.vga.as_ref().expect("VGA enabled");
                let vga = vga.borrow();
                Some(PciBarRange {
                    kind: PciBarKind::Mmio32,
                    base: u64::from(vga.lfb_base()),
                    size: u64::try_from(vga.vram_size()).unwrap_or(0),
                })
            } else {
                None
            };
            let pci_allocator_cfg = PciResourceAllocatorConfig::default();
            {
                let mut pci_cfg = pci_cfg.borrow_mut();
                let mut allocator = PciResourceAllocator::new(pci_allocator_cfg.clone());
                // `bios_post` is deterministic and keeps existing fixed BAR bases intact.
                bios_post_with_extra_reservations(
                    pci_cfg.bus_mut(),
                    &mut allocator,
                    legacy_vga_lfb_reservation,
                )
                .expect("PCI BIOS POST resource assignment should succeed");
            }

            // Keep the device model's internal PCI command register mirrored from the canonical PCI
            // config space. The machine owns PCI enumeration state via `PciConfigPorts`, but the
            // E1000 model consults its own PCI config when gating DMA (COMMAND.BME) and INTx
            // assertion (COMMAND.INTX_DISABLE).
            //
            // This ensures snapshot/save_state sees a coherent view even before the first
            // `poll_network()` call.
            if let Some(e1000) = e1000.as_ref() {
                let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
                let (command, bar0_base, bar1_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg
                        .and_then(|cfg| cfg.bar_range(0))
                        .map(|range| range.base)
                        .unwrap_or(0);
                    let bar1_base = cfg
                        .and_then(|cfg| cfg.bar_range(1))
                        .map(|range| range.base)
                        .unwrap_or(0);
                    (command, bar0_base, bar1_base)
                };
                let mut nic = e1000.borrow_mut();
                nic.pci_config_write(0x04, 2, u32::from(command));
                if let Ok(bar0_base) = u32::try_from(bar0_base) {
                    nic.pci_config_write(0x10, 4, bar0_base);
                }
                if let Ok(bar1_base) = u32::try_from(bar1_base) {
                    nic.pci_config_write(0x14, 4, bar1_base);
                }
            }

            // Virtio devices also gate DMA and INTx semantics on the PCI command register, but the
            // machine owns the canonical PCI config space (`PciConfigPorts`) rather than the virtio
            // transport model. Mirror the command register so the transport observes the guest's
            // configuration.
            if let Some(virtio_net) = virtio_net.as_ref() {
                let bdf = aero_devices::pci::profile::VIRTIO_NET.bdf;
                let (command, bar0_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg.and_then(|cfg| cfg.bar_range(0)).map(|range| range.base);
                    (command, bar0_base)
                };
                let mut dev = virtio_net.borrow_mut();
                dev.set_pci_command(command);
                if let Some(bar0_base) = bar0_base {
                    dev.config_mut().set_bar_base(0, bar0_base);
                }
            }

            if let Some(virtio_input_keyboard) = virtio_input_keyboard.as_ref() {
                let bdf = aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD.bdf;
                let (command, bar0_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg.and_then(|cfg| cfg.bar_range(0)).map(|range| range.base);
                    (command, bar0_base)
                };
                let mut dev = virtio_input_keyboard.borrow_mut();
                dev.set_pci_command(command);
                if let Some(bar0_base) = bar0_base {
                    dev.config_mut().set_bar_base(0, bar0_base);
                }
            }

            if let Some(virtio_input_mouse) = virtio_input_mouse.as_ref() {
                let bdf = aero_devices::pci::profile::VIRTIO_INPUT_MOUSE.bdf;
                let (command, bar0_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg.and_then(|cfg| cfg.bar_range(0)).map(|range| range.base);
                    (command, bar0_base)
                };
                let mut dev = virtio_input_mouse.borrow_mut();
                dev.set_pci_command(command);
                if let Some(bar0_base) = bar0_base {
                    dev.config_mut().set_bar_base(0, bar0_base);
                }
            }

            if let Some(xhci) = xhci.as_ref() {
                let bdf = aero_devices::pci::profile::USB_XHCI_QEMU.bdf;
                let (command, bar0_base, msi_state, msix_state) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg
                        .and_then(|cfg| cfg.bar_range(XhciPciDevice::MMIO_BAR_INDEX))
                        .map(|range| range.base);
                    let msi_state =
                        cfg.and_then(|cfg| cfg.capability::<MsiCapability>())
                            .map(|msi| {
                                (
                                    msi.enabled(),
                                    msi.message_address(),
                                    msi.message_data(),
                                    msi.mask_bits(),
                                )
                            });
                    let msix_state = cfg
                        .and_then(|cfg| cfg.capability::<MsixCapability>())
                        .map(|msix| (msix.enabled(), msix.function_masked()));
                    (command, bar0_base, msi_state, msix_state)
                };

                let mut xhci = xhci.borrow_mut();
                let cfg = xhci.config_mut();
                cfg.set_command(command);
                if let Some(bar0_base) = bar0_base {
                    cfg.set_bar_base(XhciPciDevice::MMIO_BAR_INDEX, bar0_base);
                }
                if let Some((enabled, addr, data, mask)) = msi_state {
                    // Note: MSI pending bits are device-managed and must not be overwritten from the
                    // canonical PCI config space (which cannot observe them).
                    sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                }
                if let Some((enabled, function_masked)) = msix_state {
                    sync_msix_capability_into_config(cfg, enabled, function_masked);
                }
            }

            // Keep storage controller device models' internal PCI config state coherent with the
            // canonical PCI config space. These devices snapshot their internal PCI config image,
            // so this ensures `save_state()` sees a consistent view even before the first
            // `process_*()` call.
            if let Some(ahci) = ahci.as_ref() {
                let bdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
                let (command, bar5_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar5_base = cfg
                        .and_then(|cfg| {
                            cfg.bar_range(aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX)
                        })
                        .map(|range| range.base);
                    (command, bar5_base)
                };

                let mut ahci = ahci.borrow_mut();
                ahci.config_mut().set_command(command);
                if let Some(bar5_base) = bar5_base {
                    ahci.config_mut()
                        .set_bar_base(aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX, bar5_base);
                }
            }
            if let Some(nvme) = nvme.as_ref() {
                let bdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
                let (command, bar0_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg.and_then(|cfg| cfg.bar_range(0)).map(|range| range.base);
                    (command, bar0_base)
                };

                let mut nvme = nvme.borrow_mut();
                nvme.config_mut().set_command(command);
                if let Some(bar0_base) = bar0_base {
                    nvme.config_mut().set_bar_base(0, bar0_base);
                }
            }
            if let Some(ide) = ide.as_ref() {
                let bdf = aero_devices::pci::profile::IDE_PIIX3.bdf;
                let (command, bar4_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar4_base = cfg.and_then(|cfg| cfg.bar_range(4)).map(|range| range.base);
                    (command, bar4_base)
                };

                let mut ide = ide.borrow_mut();
                ide.config_mut().set_command(command);
                if let Some(bar4_base) = bar4_base {
                    ide.config_mut().set_bar_base(4, bar4_base);
                }
            }
            if let Some(virtio_blk) = virtio_blk.as_ref() {
                let bdf = aero_devices::pci::profile::VIRTIO_BLK.bdf;
                let (command, bar0_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg.and_then(|cfg| cfg.bar_range(0)).map(|range| range.base);
                    (command, bar0_base)
                };

                let mut virtio_blk = virtio_blk.borrow_mut();
                virtio_blk.set_pci_command(command);
                if let Some(bar0_base) = bar0_base {
                    virtio_blk.config_mut().set_bar_base(0, bar0_base);
                }
            }

            if let Some(ehci) = ehci.as_ref() {
                let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
                let (command, bar0_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg
                        .and_then(|cfg| cfg.bar_range(EhciPciDevice::MMIO_BAR_INDEX))
                        .map(|range| range.base)
                        .unwrap_or(0);
                    (command, bar0_base)
                };

                let mut ehci = ehci.borrow_mut();
                ehci.config_mut().set_command(command);
                ehci.config_mut()
                    .set_bar_base(EhciPciDevice::MMIO_BAR_INDEX, bar0_base);
            }

            let vga = self.vga.clone();
            let aerogpu = self.aerogpu.clone();
            let aerogpu_mmio = self.aerogpu_mmio.clone();
            let ehci = ehci.clone();
            let xhci = xhci.clone();

            // Map the full ACPI-reported PCI MMIO window so BAR relocation is reflected
            // immediately even when the guest OS programs a BAR outside the allocator's default
            // sub-window.
            self.mem.map_mmio_once(PCI_MMIO_BASE, PCI_MMIO_SIZE, || {
                let legacy_vga_lfb = vga.clone().map(|vga| {
                    let (base, size) = {
                        let vga = vga.borrow();
                        (
                            u64::from(vga.lfb_base()),
                            u64::try_from(vga.vram_size()).unwrap_or(0),
                        )
                    };
                    LegacyVgaLfbWindow {
                        base,
                        size,
                        handler: VgaLfbMmioHandler { dev: vga },
                    }
                });

                let mut router = PciBarMmioRouter::new(PCI_MMIO_BASE, pci_cfg.clone());
                if let Some(ahci) = ahci.clone() {
                    let bdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
                    router.register_handler(
                        bdf,
                        aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX,
                        PciConfigSyncedMmioBar::new(
                            pci_cfg.clone(),
                            ahci,
                            bdf,
                            aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX,
                        ),
                    );
                }
                if let Some(nvme) = nvme.clone() {
                    let bdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
                    router.register_handler(
                        bdf,
                        0,
                        PciConfigSyncedMmioBar::new(pci_cfg.clone(), nvme, bdf, 0),
                    );
                }
                if let Some(ehci) = ehci.clone() {
                    let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
                    router.register_handler(
                        bdf,
                        EhciPciDevice::MMIO_BAR_INDEX,
                        PciConfigSyncedMmioBar::new(
                            pci_cfg.clone(),
                            ehci,
                            bdf,
                            EhciPciDevice::MMIO_BAR_INDEX,
                        ),
                    );
                }
                if let Some(xhci) = xhci.clone() {
                    let bdf = aero_devices::pci::profile::USB_XHCI_QEMU.bdf;
                    router.register_handler(
                        bdf,
                        XhciPciDevice::MMIO_BAR_INDEX,
                        PciConfigSyncedMmioBar::new(
                            pci_cfg.clone(),
                            xhci,
                            bdf,
                            XhciPciDevice::MMIO_BAR_INDEX,
                        ),
                    );
                }
                if let Some(aerogpu) = aerogpu.clone() {
                    let bdf = aero_devices::pci::profile::AEROGPU.bdf;
                    router.register_handler(
                        bdf,
                        aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX,
                        AeroGpuBar1Mmio { dev: aerogpu },
                    );
                }
                if let Some(e1000) = e1000.clone() {
                    router.register_shared_handler(
                        aero_devices::pci::profile::NIC_E1000_82540EM.bdf,
                        0,
                        e1000,
                    );
                }
                if let Some(virtio_net) = virtio_net.clone() {
                    let bdf = aero_devices::pci::profile::VIRTIO_NET.bdf;
                    router.register_handler(
                        bdf,
                        0,
                        VirtioPciBar0Mmio::new(pci_cfg.clone(), virtio_net, bdf),
                    );
                }
                if let Some(virtio_blk) = virtio_blk.clone() {
                    let bdf = aero_devices::pci::profile::VIRTIO_BLK.bdf;
                    router.register_handler(
                        bdf,
                        0,
                        VirtioPciBar0Mmio::new(pci_cfg.clone(), virtio_blk, bdf),
                    );
                }
                if let Some(virtio_input_keyboard) = virtio_input_keyboard.clone() {
                    let bdf = aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD.bdf;
                    router.register_handler(
                        bdf,
                        0,
                        VirtioPciBar0Mmio::new(pci_cfg.clone(), virtio_input_keyboard, bdf),
                    );
                }
                if let Some(virtio_input_mouse) = virtio_input_mouse.clone() {
                    let bdf = aero_devices::pci::profile::VIRTIO_INPUT_MOUSE.bdf;
                    router.register_handler(
                        bdf,
                        0,
                        VirtioPciBar0Mmio::new(pci_cfg.clone(), virtio_input_mouse, bdf),
                    );
                }
                if let Some(aerogpu_mmio) = aerogpu_mmio.clone() {
                    router.register_shared_handler(
                        aero_devices::pci::profile::AEROGPU.bdf,
                        aero_devices::pci::profile::AEROGPU_BAR0_INDEX,
                        aerogpu_mmio,
                    );
                }
                Box::new(PciMmioWindow {
                    window_base: PCI_MMIO_BASE,
                    router,
                    legacy_vga_lfb,
                })
            });

            // Register dispatchers for PCI I/O BARs allocated by BIOS POST.
            //
            // Note: `IoPortBus` does not support overlapping ranges, so we cannot register a
            // catch-all router over the full `PCI0._CRS` I/O windows (which would collide with
            // legacy fixed-function devices like VGA, PIT, and i8042).
            //
            // Instead, we cover the deterministic BAR allocation window used by `bios_post`
            // (`PciResourceAllocatorConfig::default().io_base..+io_size`, currently 0x1000..0xF000).
            let io_router: SharedPciIoBarRouter =
                Rc::new(RefCell::new(PciIoBarRouter::new(pci_cfg.clone())));
            {
                let mut router = io_router.borrow_mut();
                if let Some(ide) = ide.clone() {
                    let bdf = aero_devices::pci::profile::IDE_PIIX3.bdf;
                    router.register_handler(
                        bdf,
                        4,
                        IdeBusMasterBar {
                            pci_cfg: pci_cfg.clone(),
                            ide,
                            bdf,
                        },
                    );
                }
                if let Some(uhci) = uhci.clone() {
                    let bdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
                    router.register_handler(
                        bdf,
                        UhciPciDevice::IO_BAR_INDEX,
                        UhciIoBar {
                            pci_cfg: pci_cfg.clone(),
                            uhci,
                            bdf,
                        },
                    );
                }
                if let Some(e1000) = e1000.clone() {
                    router.register_handler(
                        aero_devices::pci::profile::NIC_E1000_82540EM.bdf,
                        1,
                        E1000PciIoBar { dev: e1000 },
                    );
                }
            }
            let io_base = u16::try_from(pci_allocator_cfg.io_base)
                .expect("PCI I/O BAR base should fit in u16");
            let io_len = u16::try_from(pci_allocator_cfg.io_size)
                .expect("PCI I/O BAR window size should fit in u16");
            self.io.register_range(
                io_base,
                io_len,
                Box::new(PciIoBarWindow { router: io_router }),
            );

            // Register IDE legacy I/O ports after BIOS POST so the guest-visible PCI command/BAR
            // state is initialized. Bus Master IDE (BAR4) is routed through the PCI I/O window so
            // BAR relocation is reflected immediately.
            if let Some(ide_dev) = ide.as_ref() {
                let bdf = aero_devices::pci::profile::IDE_PIIX3.bdf;

                // Primary command block (0x1F0..=0x1F7).
                for port in PRIMARY_PORTS.cmd_base..PRIMARY_PORTS.cmd_base + 8 {
                    self.io.register(
                        port,
                        Box::new(IdePort {
                            pci_cfg: pci_cfg.clone(),
                            ide: ide_dev.clone(),
                            bdf,
                            port,
                        }),
                    );
                }
                // Primary control block: 0x3F6..=0x3F7.
                for port in PRIMARY_PORTS.ctrl_base..PRIMARY_PORTS.ctrl_base + 2 {
                    self.io.register(
                        port,
                        Box::new(IdePort {
                            pci_cfg: pci_cfg.clone(),
                            ide: ide_dev.clone(),
                            bdf,
                            port,
                        }),
                    );
                }

                // Secondary command block (0x170..=0x177).
                for port in SECONDARY_PORTS.cmd_base..SECONDARY_PORTS.cmd_base + 8 {
                    self.io.register(
                        port,
                        Box::new(IdePort {
                            pci_cfg: pci_cfg.clone(),
                            ide: ide_dev.clone(),
                            bdf,
                            port,
                        }),
                    );
                }
                // Secondary control block: 0x376..=0x377.
                for port in SECONDARY_PORTS.ctrl_base..SECONDARY_PORTS.ctrl_base + 2 {
                    self.io.register(
                        port,
                        Box::new(IdePort {
                            pci_cfg: pci_cfg.clone(),
                            ide: ide_dev.clone(),
                            bdf,
                            port,
                        }),
                    );
                }
            }

            // Ensure options stay populated (for the first reset).
            self.platform_clock = Some(clock);
            self.interrupts = Some(interrupts);
            self.pit = Some(pit);
            self.rtc = Some(rtc);
            self.pci_cfg = Some(pci_cfg);
            self.pci_intx = Some(pci_intx);
            self.acpi_pm = Some(acpi_pm);
            self.hpet = Some(hpet);
            self.e1000 = e1000;
            self.virtio_net = virtio_net;
            self.virtio_input_keyboard = virtio_input_keyboard;
            self.virtio_input_mouse = virtio_input_mouse;
            self.ahci = ahci;
            self.nvme = nvme;
            self.ide = ide;
            self.virtio_blk = virtio_blk;
            self.uhci = uhci;
            self.ehci = ehci;
            self.xhci = xhci;

            // If enabled, ensure the canonical "external hub + synthetic HID" USB topology is
            // present immediately after reset.
            self.ensure_uhci_synthetic_usb_hid_topology();

            // MMIO mappings persist in the physical bus; ensure the canonical PC regions exist.
            self.map_pc_platform_mmio_regions();
        } else {
            self.platform_clock = None;
            self.interrupts = None;
            self.pit = None;
            self.rtc = None;
            self.pci_cfg = None;
            self.pci_intx = None;
            self.acpi_pm = None;
            self.hpet = None;
            self.e1000 = None;
            self.aerogpu = None;
            self.aerogpu_mmio = None;
            if !use_legacy_vga {
                self.vga = None;
            }
            self.aerogpu = None;
            self.ahci = None;
            self.nvme = None;
            self.virtio_net = None;
            self.virtio_input_keyboard = None;
            self.virtio_input_mouse = None;
            self.ide = None;
            self.virtio_blk = None;
            self.uhci = None;
            self.ehci = None;
            self.xhci = None;
        }
        if self.cfg.enable_serial {
            let uart: SharedSerial16550 = Rc::new(RefCell::new(Serial16550::new(0x3F8)));
            register_serial16550(&mut self.io, uart.clone());
            self.serial = Some(uart);
        } else {
            self.serial = None;
        }

        if self.cfg.enable_a20_gate {
            let dev = A20GateDevice::with_reset_sink(self.chipset.a20(), self.reset_latch.clone());
            self.io.register(A20_GATE_PORT, Box::new(dev));
        }

        if self.cfg.enable_reset_ctrl {
            self.io.register(
                RESET_CTRL_PORT,
                Box::new(ResetCtrl::new(self.reset_latch.clone())),
            );
        }

        if self.cfg.enable_i8042 {
            let ports = I8042Ports::new();
            // If the PC platform interrupt controller is enabled, wire i8042 IRQ1/IRQ12 pulses
            // into it so the guest can receive keyboard/mouse interrupts.
            if let Some(interrupts) = &self.interrupts {
                ports.connect_irqs_to_platform_interrupts(interrupts.clone());
            }
            let ctrl = ports.controller();
            aero_devices::i8042::register_i8042(&mut self.io, ctrl.clone());

            ctrl.borrow_mut().set_system_control_sink(Box::new(
                aero_devices::i8042::PlatformSystemControlSink::with_reset_sink(
                    self.chipset.a20(),
                    self.reset_latch.clone(),
                ),
            ));

            self.i8042 = Some(ctrl);
        } else {
            self.i8042 = None;
        }

        self.assist = AssistContext::default();
        self.cpu = CpuCore::new(CpuMode::Real);
        set_cpu_apic_base_bsp_bit(&mut self.cpu, true);
        // Application processors (APs) start with the BSP bit (IA32_APIC_BASE[8]) cleared.
        let mut ap_cpus = Vec::new();
        for apic_id in 1..self.cfg.cpu_count {
            let mut cpu = CpuCore::new(CpuMode::Real);
            // APs power up in a halted wait-for-SIPI state until started by the BSP.
            vcpu_init::reset_ap_vcpu_to_init_state(&mut cpu);
            // Optional: expose the APIC ID via RDTSCP (IA32_TSC_AUX).
            cpu.state.msr.tsc_aux = apic_id as u32;
            set_cpu_apic_base_bsp_bit(&mut cpu, false);
            // APs remain in a halted wait-for-SIPI state until started by the BSP.
            cpu.state.halted = true;
            ap_cpus.push(cpu);
        }
        self.ap_cpus = ap_cpus;
        self.guest_time = GuestTime::new_from_cpu(&self.cpu);
        self.mmu = aero_mmu::Mmu::new();

        // Run firmware POST (in Rust) to initialize IVT/BDA, map BIOS stubs, and load the boot
        // sector into RAM.
        //
        // ACPI tables must only be published when they describe real machine wiring. Since the
        // canonical PC platform devices are behind `enable_pc_platform`, gate ACPI publication on
        // both flags.
        //
        // Preserve any host-selected boot drive (e.g. CD vs HDD) across resets and snapshot
        // restores.
        let boot_drive = self.bios.config().boot_drive;
        let cd_boot_drive = self.bios.config().cd_boot_drive;
        let boot_from_cd_if_present = self.bios.config().boot_from_cd_if_present;
        self.boot_drive = boot_drive;
        // The BIOS is HLE and by default keeps the VBE linear framebuffer inside guest RAM so the
        // firmware-only tests can access it without MMIO routing.
        //
        // When legacy VGA is enabled, configure the BIOS to report the (MMIO-safe) LFB base that
        // our VGA device is mapped at (legacy default: `SVGA_LFB_BASE`) so OSes and bootloaders
        // see a stable framebuffer address.
        //
        // When VGA is disabled, keep the default LFB base in conventional RAM: pointing the BIOS
        // at the VGA MMIO LFB base would overlap the canonical PCI MMIO window (and could cause BIOS VBE
        // helpers like `int 0x10, ax=0x4F02` to scribble over PCI device BARs).
        let legacy_vga_lfb_base = if use_legacy_vga {
            self.vga
                .as_ref()
                .map(|vga| vga.borrow().lfb_base())
                .unwrap_or_else(|| self.legacy_vga_lfb_base())
        } else {
            0
        };
        self.bios = Bios::new(BiosConfig {
            memory_size_bytes: self.cfg.ram_size_bytes,
            boot_drive,
            cd_boot_drive,
            boot_from_cd_if_present,
            cpu_count: self.cfg.cpu_count,
            smbios_uuid_seed: self.cfg.smbios_uuid_seed,
            enable_acpi: self.cfg.enable_pc_platform && self.cfg.enable_acpi,
            vbe_lfb_base: use_legacy_vga.then_some(legacy_vga_lfb_base),
            ..Default::default()
        });
        // Patch the BIOS's VBE controller `TotalMemory` reporting when the active framebuffer is
        // backed by a device-owned VRAM aperture (e.g. AeroGPU BAR1) rather than the firmware test
        // default in guest RAM.
        if self.cfg.enable_aerogpu {
            let blocks = aero_devices::pci::profile::AEROGPU_VRAM_SIZE.div_ceil(64 * 1024);
            self.bios.video.vbe.total_memory_64kb_blocks = blocks.min(u64::from(u16::MAX)) as u16;
        } else if use_legacy_vga {
            let vram_bytes = self
                .vga
                .as_ref()
                .map(|vga| vga.borrow().vram_size() as u64)
                .unwrap_or(aero_gpu_vga::DEFAULT_VRAM_SIZE as u64);
            let blocks = vram_bytes.div_ceil(64 * 1024);
            self.bios.video.vbe.total_memory_64kb_blocks = blocks.min(u64::from(u16::MAX)) as u16;
        }

        let bus: &mut dyn BiosBus = &mut self.mem;
        // Optional ISO install media: expose it to the BIOS as a CD-ROM backend (2048-byte
        // sectors), alongside the primary HDD BlockDevice.
        let mut cdrom = self.install_media.as_ref().and_then(InstallMedia::upgrade);
        let cdrom_ref = cdrom
            .as_mut()
            .map(|cdrom| cdrom as &mut dyn firmware::bios::CdromDevice);

        if let Some(pci_cfg) = &self.pci_cfg {
            let mut pci = SharedPciConfigPortsBiosAdapter::new(pci_cfg.clone());
            self.bios.post_with_pci(
                &mut self.cpu.state,
                bus,
                &mut self.disk,
                cdrom_ref,
                Some(&mut pci),
            );
        } else {
            self.bios
                .post(&mut self.cpu.state, bus, &mut self.disk, cdrom_ref);
        }

        // The firmware's BDA initialization derives the "fixed disk count" (0x40:0x75) from the
        // configured boot drive number. When booting via El Torito (`DL=0xE0..=0xEF`), the firmware
        // sets this count to 0 to avoid inflating it based on the CD drive number.
        //
        // In the canonical machine, however, we always expose HDD0 at `DL=0x80` *in addition to*
        // any CD boot device. Patch the BDA so BIOS INT 13h `drive_present()` checks can still
        // succeed for HDD accesses while booting from CD.
        self.mem.write_u8(firmware::bios::BDA_BASE + 0x75, 1);
        // Once PCI BARs are assigned, update the BIOS VBE LFB base for any display configurations
        // that derive it from PCI resources (AeroGPU BAR1).
        self.sync_bios_vbe_lfb_base_to_display_wiring();
        // The HLE BIOS maintains its own copy of the VBE palette for INT 10h AX=4F09 services, but
        // does not perform VGA port I/O. Keep the AeroGPU-emulated VGA DAC palette coherent with
        // the BIOS palette so 8bpp VBE modes (which are palette-indexed) have a sensible default
        // palette before the guest programs it.
        //
        // This also keeps DAC port reads (`0x3C7/0x3C9`) coherent with BIOS-driven palette changes
        // during early boot.
        if let Some(aerogpu) = &self.aerogpu {
            let bits = self.bios.video.vbe.dac_width_bits;
            let pal = &self.bios.video.vbe.palette;
            let mut dev = aerogpu.borrow_mut();
            dev.vga_port_write_u8(0x3C8, 0); // set DAC write index
            for idx in 0..256usize {
                let base = idx * 4;
                let b = pal[base];
                let g = pal[base + 1];
                let r = pal[base + 2];

                let (r, g, b) = if bits >= 8 {
                    (r >> 2, g >> 2, b >> 2)
                } else {
                    (r & 0x3F, g & 0x3F, b & 0x3F)
                };

                // VGA DAC write order is R, G, B.
                dev.vga_port_write_u8(0x3C9, r);
                dev.vga_port_write_u8(0x3C9, g);
                dev.vga_port_write_u8(0x3C9, b);
            }
        }
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        if self.bios.video.vbe.current_mode.is_none() {
            self.sync_text_mode_cursor_bda_to_vga_crtc();
        }

        // Reset returns the machine to legacy text mode; publish this so external presentation
        // layers can follow (and so any previous WDDM claim is cleared on reset).
        #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
        if let Some(scanout_state) = &self.scanout_state {
            let _ = scanout_state.try_publish(ScanoutStateUpdate {
                source: SCANOUT_SOURCE_LEGACY_TEXT,
                base_paddr_lo: 0,
                base_paddr_hi: 0,
                width: 0,
                height: 0,
                pitch_bytes: 0,
                format: SCANOUT_FORMAT_B8G8R8X8,
            });
        }

        // Reset returns the machine to a legacy (non-WDDM) scanout; also disable the hardware
        // cursor so hosts don't display stale WDDM cursor state after a reset.
        #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
        if let Some(cursor_state) = &self.cursor_state {
            let _ = cursor_state.try_publish(CursorStateUpdate {
                enable: 0,
                x: 0,
                y: 0,
                hot_x: 0,
                hot_y: 0,
                width: 0,
                height: 0,
                pitch_bytes: 0,
                format: CURSOR_FORMAT_B8G8R8A8,
                base_paddr_lo: 0,
                base_paddr_hi: 0,
            });
        }

        // If firmware POST failed and halted the CPU via `Bios::bios_panic`, mirror the panic text
        // into COM1 so host runtimes that monitor serial output can surface the failure reason.
        if self.cpu.state.halted {
            self.mirror_bios_panic_to_serial();
        }
        self.mem.clear_dirty();
    }

    /// Allow the AHCI controller (if present) to make forward progress (DMA).
    ///
    /// This mirrors the behaviour of [`aero_pc_platform::PcPlatform::process_ahci`].
    pub fn process_ahci(&mut self) {
        let (Some(ahci), Some(pci_cfg)) = (&self.ahci, &self.pci_cfg) else {
            return;
        };

        let bdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
        let (command, bar5_base, msi_state) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config_mut(bdf);
            let command = cfg.as_ref().map(|cfg| cfg.command()).unwrap_or(0);
            let bar5_base = cfg
                .as_ref()
                .and_then(|cfg| cfg.bar_range(aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX))
                .map(|range| range.base);
            let msi_state = cfg
                .as_ref()
                .and_then(|cfg| cfg.capability::<MsiCapability>())
                .map(|msi| {
                    (
                        msi.enabled(),
                        msi.message_address(),
                        msi.message_data(),
                        msi.mask_bits(),
                    )
                });
            (command, bar5_base, msi_state)
        };

        let bus_master_enabled = (command & (1 << 2)) != 0;

        let mut dev = ahci.borrow_mut();
        {
            let cfg = dev.config_mut();
            cfg.set_command(command);
            if let Some(bar5_base) = bar5_base {
                cfg.set_bar_base(aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX, bar5_base);
            }
            if let Some((enabled, addr, data, mask)) = msi_state {
                // Note: MSI pending bits are device-managed and must not be overwritten from the
                // canonical PCI config space (which cannot observe them).
                sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
            }
        }

        if bus_master_enabled {
            dev.process(&mut self.mem);
        }
    }

    /// Allow the IDE controller (if present) to make forward progress (Bus Master DMA).
    ///
    /// This mirrors the behaviour of [`aero_pc_platform::PcPlatform::process_ide`].
    pub fn process_ide(&mut self) {
        let (Some(ide), Some(pci_cfg)) = (&self.ide, &self.pci_cfg) else {
            return;
        };

        let bdf = aero_devices::pci::profile::IDE_PIIX3.bdf;
        let (command, bar4_base) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar4_base = cfg.and_then(|cfg| cfg.bar_range(4)).map(|range| range.base);
            (command, bar4_base)
        };

        let mut dev = ide.borrow_mut();
        dev.config_mut().set_command(command);
        if let Some(bar4_base) = bar4_base {
            dev.config_mut().set_bar_base(4, bar4_base);
        }

        dev.tick(&mut self.mem);
    }

    /// Allow the NVMe controller (if present) to make forward progress (DMA).
    pub fn process_nvme(&mut self) {
        let (Some(nvme), Some(pci_cfg)) = (&self.nvme, &self.pci_cfg) else {
            return;
        };

        let bdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
        let (command, msi_state, msix_state) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let msi_state = cfg
                .and_then(|cfg| cfg.capability::<MsiCapability>())
                .map(|msi| {
                    (
                        msi.enabled(),
                        msi.message_address(),
                        msi.message_data(),
                        msi.mask_bits(),
                    )
                });
            let msix_state = cfg
                .and_then(|cfg| cfg.capability::<MsixCapability>())
                .map(|msix| (msix.enabled(), msix.function_masked()));
            (command, msi_state, msix_state)
        };

        let mut dev = nvme.borrow_mut();
        {
            // Keep the NVMe model's view of PCI config state in sync so it can apply bus mastering
            // gating and deliver MSI/MSI-X during `process()`.
            //
            // Note: MSI pending bits are device-managed and must not be overwritten from the
            // canonical PCI config space (which cannot observe them).
            let cfg = dev.config_mut();
            cfg.set_command(command);
            if let Some((enabled, addr, data, mask)) = msi_state {
                sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
            }
            if let Some((enabled, function_masked)) = msix_state {
                sync_msix_capability_into_config(cfg, enabled, function_masked);
            }
        }
        dev.process(&mut self.mem);
    }

    /// Allow the AeroGPU device (if present) to process doorbells and complete fences.
    ///
    /// The in-tree Win7 AeroGPU KMD relies on the ring transport (doorbell -> consume submit desc
    /// -> advance head -> update completed fence + fence page) to make forward progress during
    /// `StartDevice` and early submission.
    pub fn process_aerogpu(&mut self) {
        let (Some(aerogpu), Some(pci_cfg)) = (&self.aerogpu_mmio, &self.pci_cfg) else {
            return;
        };

        // Some integration tests drive the machine by advancing the deterministic `platform_clock`
        // and then calling `process_aerogpu()` (rather than using the full device tick loop). Keep
        // the AeroGPU device model's internal timebase coherent with that clock so vblank-paced
        // fences can make forward progress.
        let platform_now_ns = self.platform_clock.as_ref().map(|clock| clock.now_ns());

        let bdf = aero_devices::pci::profile::AEROGPU.bdf;
        let (command, bar0_base, bar1_base) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar0_base = cfg
                .and_then(|cfg| cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR0_INDEX))
                .map(|range| range.base)
                .unwrap_or(0);
            let bar1_base = cfg
                .and_then(|cfg| cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX))
                .map(|range| range.base)
                .unwrap_or(0);
            (command, bar0_base, bar1_base)
        };

        let mut dev = aerogpu.borrow_mut();
        // Keep the AeroGPU model's internal PCI config image coherent with the canonical PCI config
        // space owned by the machine.
        dev.config_mut().set_command(command);
        dev.config_mut()
            .set_bar_base(aero_devices::pci::profile::AEROGPU_BAR0_INDEX, bar0_base);
        dev.config_mut().set_bar_base(
            aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX,
            bar1_base,
        );

        if let Some(now_ns) = platform_now_ns {
            dev.tick_vblank(now_ns);
        }
        dev.process(&mut self.mem);

        // Publish WDDM scanout state updates based on BAR0 scanout registers.
        //
        // This is gated behind builds that support publishing into the shared scanout header:
        // native builds and the wasm32 `wasm-threaded` build.
        #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
        if let Some(scanout_state) = &self.scanout_state {
            let legacy_text = ScanoutStateUpdate {
                source: SCANOUT_SOURCE_LEGACY_TEXT,
                base_paddr_lo: 0,
                base_paddr_hi: 0,
                width: 0,
                height: 0,
                pitch_bytes: 0,
                format: SCANOUT_FORMAT_B8G8R8X8,
            };

            let current_legacy_scanout_descriptor = || -> ScanoutStateUpdate {
                // BIOS-driven VBE modes (INT 10h / HLE BIOS).
                if let Some(mode) = self.bios.video.vbe.current_mode {
                    if let Some(mode_info) = self.bios.video.vbe.find_mode(mode) {
                        // This legacy VBE scanout publication path currently only supports the
                        // canonical boot pixel formats:
                        // - 32bpp packed pixels `B8G8R8X8`
                        // - 16bpp packed pixels `B5G6R5`
                        //
                        // If the guest selected a palettized VBE mode (e.g. 8bpp), fall back to the
                        // implicit legacy path rather than publishing a misleading descriptor.
                        let (format, bytes_per_pixel) = match mode_info.bpp {
                            32 => (SCANOUT_FORMAT_B8G8R8X8, 4u64),
                            16 => (SCANOUT_FORMAT_B5G6R5, 2u64),
                            _ => return legacy_text,
                        };

                        // Keep the published legacy scanout descriptor consistent with the BIOS VBE
                        // state used by the AeroGPU VBE/text fallback renderer
                        // (`display_present_aerogpu_vbe_lfb`).
                        //
                        // `ScanoutState` has no explicit panning fields, so display-start offsets must
                        // be encoded by adjusting the base address.
                        let pitch = u64::from(
                            self.bios
                                .video
                                .vbe
                                .bytes_per_scan_line
                                .max(mode_info.bytes_per_scan_line()),
                        );
                        if pitch == 0 {
                            return legacy_text;
                        }
                        let base = u64::from(self.bios.video.vbe.lfb_base)
                            .saturating_add(
                                u64::from(self.bios.video.vbe.display_start_y)
                                    .saturating_mul(pitch),
                            )
                            .saturating_add(
                                u64::from(self.bios.video.vbe.display_start_x)
                                    .saturating_mul(bytes_per_pixel),
                            );
                        return ScanoutStateUpdate {
                            source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
                            base_paddr_lo: base as u32,
                            base_paddr_hi: (base >> 32) as u32,
                            width: u32::from(mode_info.width),
                            height: u32::from(mode_info.height),
                            pitch_bytes: pitch as u32,
                            format,
                        };
                    }
                    return legacy_text;
                }

                // Guest-driven Bochs VBE_DISPI programming (0x01CE/0x01CF). This allows a guest to
                // select a VBE LFB mode without going through BIOS INT 10h, so publish it as a
                // legacy VBE scanout when WDDM is not active.
                let Some(aerogpu) = self.aerogpu.as_ref() else {
                    return legacy_text;
                };
                let dev = aerogpu.borrow();
                if !dev.vbe_dispi_enabled() {
                    return legacy_text;
                }

                // Only publish scanout formats the shared scanout descriptor can represent.
                let (format, bytes_per_pixel) = match dev.vbe_dispi_bpp {
                    32 => (SCANOUT_FORMAT_B8G8R8X8, 4u64),
                    16 => (SCANOUT_FORMAT_B5G6R5, 2u64),
                    _ => return legacy_text,
                };

                let width = u32::from(dev.vbe_dispi_xres);
                let height = u32::from(dev.vbe_dispi_yres);
                if width == 0 || height == 0 {
                    return legacy_text;
                }

                let pitch_pixels = if dev.vbe_dispi_virt_width != 0 {
                    dev.vbe_dispi_virt_width
                } else {
                    dev.vbe_dispi_xres
                };
                let pitch = u64::from(pitch_pixels).saturating_mul(bytes_per_pixel);
                if pitch == 0 {
                    return legacy_text;
                }

                let base = u64::from(self.bios.video.vbe.lfb_base)
                    .saturating_add(u64::from(dev.vbe_dispi_y_offset).saturating_mul(pitch))
                    .saturating_add(
                        u64::from(dev.vbe_dispi_x_offset).saturating_mul(bytes_per_pixel),
                    );
                ScanoutStateUpdate {
                    source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
                    base_paddr_lo: base as u32,
                    base_paddr_hi: (base >> 32) as u32,
                    width,
                    height,
                    pitch_bytes: pitch as u32,
                    format,
                }
            };

            let maybe_publish_legacy_descriptor =
                |scanout_state: &ScanoutState, update: ScanoutStateUpdate| {
                    let matches_current = scanout_state.try_snapshot().is_some_and(|snap| {
                        snap.source == update.source
                            && snap.base_paddr_lo == update.base_paddr_lo
                            && snap.base_paddr_hi == update.base_paddr_hi
                            && snap.width == update.width
                            && snap.height == update.height
                            && snap.pitch_bytes == update.pitch_bytes
                            && snap.format == update.format
                    });
                    if matches_current {
                        return;
                    }
                    let _ = scanout_state.try_publish(update);
                };

            if let Some(update) = dev.take_scanout0_state_update() {
                // Publish WDDM scanout updates derived from BAR0 scanout registers once the guest
                // has claimed WDDM ownership.
                let _ = scanout_state.try_publish(update);
            }

            // If scanout has not yet been claimed by a valid WDDM configuration, keep the published
            // shared scanout descriptor coherent with the current legacy state.
            //
            // This is required for guests that program VBE modes via Bochs VBE_DISPI ports (without
            // BIOS INT 10h).
            //
            // Note: Once WDDM has claimed scanout, it remains authoritative (even when scanout is
            // temporarily disabled) until reset, so do not override WDDM-published disabled
            // descriptors with legacy scanout updates.
            let state = dev.scanout0_state();
            if !state.wddm_scanout_active {
                let update = current_legacy_scanout_descriptor();
                maybe_publish_legacy_descriptor(scanout_state, update);
            }
        }

        // Publish hardware cursor updates based on BAR0 cursor registers.
        #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
        if let Some(cursor_state) = &self.cursor_state {
            if let Some(update) = dev.take_cursor_state_update() {
                let _ = cursor_state.try_publish(update);
            }
        }
    }

    /// Drain newly-decoded AeroGPU submissions.
    ///
    /// This is primarily used by browser/WASM integrations that execute AeroGPU command streams
    /// out-of-process (e.g. a GPU worker). Native builds generally rely on in-process rendering
    /// paths instead.
    pub fn aerogpu_drain_submissions(&mut self) -> Vec<AerogpuSubmission> {
        let Some(aerogpu) = &self.aerogpu_mmio else {
            return Vec::new();
        };
        aerogpu.borrow_mut().drain_pending_submissions()
    }

    /// Enable the AeroGPU submission bridge (external executor mode).
    ///
    /// When enabled, the in-process AeroGPU device model no longer treats submissions as completed
    /// immediately; callers must invoke [`Machine::aerogpu_complete_fence`] for forward progress.
    pub fn aerogpu_enable_submission_bridge(&mut self) {
        let Some(aerogpu) = &self.aerogpu_mmio else {
            return;
        };
        aerogpu.borrow_mut().enable_submission_bridge();
    }

    /// Mark an AeroGPU submission fence as completed by an out-of-process executor.
    pub fn aerogpu_complete_fence(&mut self, fence: u64) {
        let (Some(aerogpu), Some(pci_cfg)) = (&self.aerogpu_mmio, &self.pci_cfg) else {
            return;
        };

        // Keep the AeroGPU model's internal PCI config image coherent with the canonical PCI
        // config space owned by the machine so the device can apply COMMAND.BME gating.
        let bdf = aero_devices::pci::profile::AEROGPU.bdf;
        let (command, bar0_base, bar1_base) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar0_base = cfg
                .and_then(|cfg| cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR0_INDEX))
                .map(|range| range.base)
                .unwrap_or(0);
            let bar1_base = cfg
                .and_then(|cfg| cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX))
                .map(|range| range.base)
                .unwrap_or(0);
            (command, bar0_base, bar1_base)
        };

        let mut dev = aerogpu.borrow_mut();
        dev.config_mut().set_command(command);
        dev.config_mut()
            .set_bar_base(aero_devices::pci::profile::AEROGPU_BAR0_INDEX, bar0_base);
        dev.config_mut().set_bar_base(
            aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX,
            bar1_base,
        );
        dev.complete_fence_from_backend(&mut self.mem, fence);
    }

    /// Allow the virtio-blk controller (if present) to make forward progress (DMA).
    pub fn process_virtio_blk(&mut self) {
        let (Some(virtio_blk), Some(pci_cfg)) = (&self.virtio_blk, &self.pci_cfg) else {
            return;
        };

        let bdf = aero_devices::pci::profile::VIRTIO_BLK.bdf;
        let (command, msix_enabled, msix_masked) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            match pci_cfg.bus_mut().device_config(bdf) {
                Some(cfg) => {
                    let msix = cfg.capability::<MsixCapability>();
                    (
                        cfg.command(),
                        msix.is_some_and(|msix| msix.enabled()),
                        msix.is_some_and(|msix| msix.function_masked()),
                    )
                }
                None => (0, false, false),
            }
        };

        {
            let mut virtio_blk = virtio_blk.borrow_mut();
            virtio_blk.set_pci_command(command);
            sync_virtio_msix_from_platform(&mut virtio_blk, msix_enabled, msix_masked);
        }

        // Respect PCI Bus Master Enable (bit 2). Virtio DMA is undefined without it.
        if (command & (1 << 2)) == 0 {
            return;
        }

        let mut dma = VirtioDmaMemory::new(&mut self.mem);
        virtio_blk.borrow_mut().process_notified_queues(&mut dma);
    }

    /// Allow virtio-input devices (if present) to make forward progress (DMA).
    pub fn process_virtio_input(&mut self) {
        let Some(pci_cfg) = self.pci_cfg.clone() else {
            return;
        };

        if let Some(virtio) = self.virtio_input_keyboard.clone() {
            self.process_virtio_input_device(
                &virtio,
                aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD.bdf,
                &pci_cfg,
            );
        }
        if let Some(virtio) = self.virtio_input_mouse.clone() {
            self.process_virtio_input_device(
                &virtio,
                aero_devices::pci::profile::VIRTIO_INPUT_MOUSE.bdf,
                &pci_cfg,
            );
        }
    }

    fn process_virtio_input_device(
        &mut self,
        virtio: &Rc<RefCell<VirtioPciDevice>>,
        bdf: PciBdf,
        pci_cfg: &SharedPciConfigPorts,
    ) {
        let (command, msix_enabled, msix_masked) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            match pci_cfg.bus_mut().device_config(bdf) {
                Some(cfg) => {
                    let msix = cfg.capability::<MsixCapability>();
                    (
                        cfg.command(),
                        msix.is_some_and(|msix| msix.enabled()),
                        msix.is_some_and(|msix| msix.function_masked()),
                    )
                }
                None => (0, false, false),
            }
        };

        {
            let mut virtio = virtio.borrow_mut();
            virtio.set_pci_command(command);
            sync_virtio_msix_from_platform(&mut virtio, msix_enabled, msix_masked);
        }

        // Respect PCI Bus Master Enable (bit 2). Virtio DMA is undefined without it.
        if (command & (1 << 2)) == 0 {
            return;
        }

        let mut dma = VirtioDmaMemory::new(&mut self.mem);
        // Clamp all guest-driven work so a corrupted/malicious driver cannot cause unbounded work
        // in a single call.
        const MAX_CHAINS_PER_QUEUE_PER_POLL: usize = 64;

        let mut virtio = virtio.borrow_mut();
        virtio.process_notified_queues_bounded(&mut dma, MAX_CHAINS_PER_QUEUE_PER_POLL);
        // Poll device-driven paths (host-injected input events) without consuming additional avail
        // entries beyond the per-queue budget above.
        virtio.poll_bounded(&mut dma, 0);
    }

    /// Poll any enabled NIC + host network backend bridge once.
    ///
    /// This is safe to call even when no NIC is enabled; it will no-op.
    pub fn poll_network(&mut self) {
        const MAX_FRAMES_PER_POLL: usize = aero_net_pump::DEFAULT_MAX_FRAMES_PER_POLL;

        if let Some(e1000) = &self.e1000 {
            let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
            let (command, bar0_base, bar1_base) = self
                .pci_cfg
                .as_ref()
                .map(|pci_cfg| {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg
                        .and_then(|cfg| cfg.bar_range(0))
                        .map(|range| range.base)
                        .unwrap_or(0);
                    let bar1_base = cfg
                        .and_then(|cfg| cfg.bar_range(1))
                        .map(|range| range.base)
                        .unwrap_or(0);
                    (command, bar0_base, bar1_base)
                })
                .unwrap_or((0, 0, 0));

            // Keep the device model's internal PCI config registers in sync with the canonical PCI
            // bus owned by the machine.
            //
            // The E1000 model gates DMA on COMMAND.BME (bit 2) by consulting its own PCI config state,
            // while the machine maintains a separate canonical config space for enumeration.
            //
            // The shared `aero-net-pump` helper assumes the NIC's internal PCI command state is already
            // up to date.
            let mut nic = e1000.borrow_mut();
            nic.pci_config_write(0x04, 2, u32::from(command));
            if let Ok(bar0_base) = u32::try_from(bar0_base) {
                nic.pci_config_write(0x10, 4, bar0_base);
            }
            if let Ok(bar1_base) = u32::try_from(bar1_base) {
                nic.pci_config_write(0x14, 4, bar1_base);
            }

            // `Option<B>` implements `NetworkBackend`, so when no backend is installed this still
            // drains guest TX frames (dropping them) while making no forward progress on host RX.
            tick_e1000(
                &mut nic,
                &mut self.mem,
                &mut self.network_backend,
                MAX_FRAMES_PER_POLL,
                MAX_FRAMES_PER_POLL,
            );
            return;
        }

        let Some(virtio) = &self.virtio_net else {
            return;
        };

        // Respect PCI Bus Master Enable (bit 2). Virtio DMA is undefined without it.
        let bdf = aero_devices::pci::profile::VIRTIO_NET.bdf;
        let (command, msix_enabled, msix_masked) = self
            .pci_cfg
            .as_ref()
            .and_then(|pci_cfg| {
                let mut pci_cfg = pci_cfg.borrow_mut();
                match pci_cfg.bus_mut().device_config(bdf) {
                    Some(cfg) => {
                        let msix = cfg.capability::<MsixCapability>();
                        Some((
                            cfg.command(),
                            msix.is_some_and(|msix| msix.enabled()),
                            msix.is_some_and(|msix| msix.function_masked()),
                        ))
                    }
                    None => None,
                }
            })
            .unwrap_or((0, false, false));

        // Keep the virtio transport's internal PCI command register in sync with the canonical PCI
        // config space owned by the machine.
        //
        // `VirtioPciDevice` gates all guest-memory DMA (virtqueue processing) on COMMAND.BME (bit
        // 2), so if we don't mirror the command register the device will never make forward
        // progress even after the guest enables bus mastering via PCI config ports.
        let mut virtio = virtio.borrow_mut();
        virtio.set_pci_command(command);
        sync_virtio_msix_from_platform(&mut virtio, msix_enabled, msix_masked);

        if (command & (1 << 2)) == 0 {
            return;
        }

        let mut dma = VirtioDmaMemory::new(&mut self.mem);
        // Clamp all work so a guest or backend cannot cause unbounded processing within a single
        // `poll_network()` call.
        const MAX_CHAINS_PER_QUEUE_PER_POLL: usize = MAX_FRAMES_PER_POLL;

        tick_virtio_net(
            &mut virtio,
            &mut dma,
            MAX_CHAINS_PER_QUEUE_PER_POLL,
            MAX_FRAMES_PER_POLL,
        );
    }

    fn run_ap_cpus(&mut self, cfg: &Tier0Config, max_insts: u64) {
        if self.cfg.cpu_count <= 1 || max_insts == 0 {
            return;
        }

        let interrupts = self.interrupts.clone();

        // Run each AP for a bounded number of instructions. This is intentionally a very simple
        // cooperative scheduler (BSP-driven) that is "good enough" for SMP bring-up tests like
        // INIT+SIPI.
        let ap_count = self.ap_cpus.len();
        for idx in 0..ap_count {
            // Split the AP array so the currently executing AP (`cpu`) is disjoint from the slices
            // stored in the per-vCPU memory bus adapter. This avoids aliasing mutable references
            // while still allowing the bus to deliver INIT/SIPI effects to *other* APs.
            let (before, rest) = self.ap_cpus.split_at_mut(idx);
            let Some((cpu, after)) = rest.split_first_mut() else {
                break;
            };

            let apic_id = (idx as u8).saturating_add(1);

            // Ensure APs can be woken from HLT by timer/IOAPIC delivery.
            //
            // The BSP uses `poll_platform_interrupt` to translate the platform interrupt controller
            // (PIC/IOAPIC+LAPIC) into `cpu.pending.external_interrupts`. Do the same for APs so they
            // can observe LAPIC timer interrupts and IOAPIC destination routing.
            const MAX_QUEUED_EXTERNAL_INTERRUPTS: usize = 1;
            let _ = Self::poll_platform_interrupt_for_apic(
                interrupts.as_ref(),
                apic_id,
                cpu,
                MAX_QUEUED_EXTERNAL_INTERRUPTS,
            );
            if cpu.state.halted {
                continue;
            }

            // Keep the core's A20 view coherent with the chipset latch.
            cpu.state.a20_enabled = self.chipset.a20().enabled();

            // Wrap the shared `SystemMemory` in the per-vCPU LAPIC routing adapter.
            let phys = PerCpuSystemMemoryBus::new(
                apic_id,
                interrupts.clone(),
                ApCpus::Split { before, after },
                &mut self.mem,
            );
            let mut inner =
                aero_cpu_core::PagingBus::new_with_io(phys, StrictIoPortBus { io: &mut self.io });
            std::mem::swap(&mut self.mmu, inner.mmu_mut());
            let mut bus = MachineCpuBus {
                a20: self.chipset.a20(),
                reset: self.reset_latch.clone(),
                inner,
            };

            let _ =
                run_batch_cpu_core_with_assists(cfg, &mut self.assist, cpu, &mut bus, max_insts);
            std::mem::swap(&mut self.mmu, bus.inner.mmu_mut());
        }
    }

    /// Run the CPU for at most `max_insts` guest instructions.
    pub fn run_slice(&mut self, max_insts: u64) -> RunExit {
        let mut executed = 0u64;
        // Keep Tier-0 instruction gating coherent with the CPUID surface that assists expose to the
        // guest.
        let cfg = Tier0Config::from_cpuid(&self.assist.features);
        while executed < max_insts {
            if let Some(kind) = self.reset_latch.take() {
                self.flush_serial();
                return RunExit::ResetRequested { kind, executed };
            }

            // Keep the core's A20 view coherent with the chipset latch.
            self.cpu.state.a20_enabled = self.chipset.a20().enabled();

            // Allow storage controllers to make forward progress even while the CPU is halted.
            //
            // AHCI completes DMA asynchronously and signals completion via interrupts; those
            // interrupts must be able to wake a HLT'd CPU.
            self.process_ahci();
            self.process_nvme();
            self.process_virtio_blk();
            self.process_aerogpu();
            self.process_virtio_input();

            self.poll_network();
            self.process_ahci();
            self.process_nvme();
            self.process_virtio_blk();
            self.process_aerogpu();
            self.process_ide();

            // Poll the platform interrupt controller (PIC/IOAPIC+LAPIC) and enqueue at most one
            // pending external interrupt vector into the CPU core.
            //
            // Tier-0 only delivers interrupts that are already present in
            // `cpu.pending.external_interrupts`; it does not poll an interrupt controller itself.
            //
            // Keep this polling bounded so a level-triggered interrupt line that remains asserted
            // cannot cause an unbounded growth of the external interrupt FIFO when the guest has
            // interrupts masked (IF=0) or otherwise cannot accept delivery yet.
            const MAX_QUEUED_EXTERNAL_INTERRUPTS: usize = 1;
            let _ = self.poll_platform_interrupt(MAX_QUEUED_EXTERNAL_INTERRUPTS);

            let mut remaining = max_insts - executed;
            // `CpuState::apply_a20` masks bit 20 in real/v8086 mode when `state.a20_enabled` is
            // false. If the guest enables A20 via port I/O, the chipset latch updates immediately,
            // but `state.a20_enabled` is only synchronized here at the outer loop boundary.
            //
            // Run one instruction per batch while A20 is disabled so any enable transition is
            // observed before the subsequent instruction executes.
            if matches!(self.cpu.state.mode, CpuMode::Real | CpuMode::Vm86)
                && !self.cpu.state.a20_enabled
            {
                remaining = remaining.min(1);
            }
            // The LAPIC MMIO page is per-vCPU, so wrap the shared `SystemMemory` in a per-vCPU
            // adapter that can route `0xFEE0_0000..+0x1000` accesses to the correct LAPIC instance.
            //
            // Today `Machine` only executes vCPU0, so the APIC ID is always 0 here; multi-vCPU
            // scheduling can pass the correct APIC ID when additional `CpuCore` instances are
            // introduced.
            let phys = PerCpuSystemMemoryBus::new(
                0,
                self.interrupts.clone(),
                ApCpus::All(self.ap_cpus.as_mut_slice()),
                &mut self.mem,
            );
            let mut inner =
                aero_cpu_core::PagingBus::new_with_io(phys, StrictIoPortBus { io: &mut self.io });
            std::mem::swap(&mut self.mmu, inner.mmu_mut());
            let mut bus = MachineCpuBus {
                a20: self.chipset.a20(),
                reset: self.reset_latch.clone(),
                inner,
            };

            let batch = run_batch_cpu_core_with_assists(
                &cfg,
                &mut self.assist,
                &mut self.cpu,
                &mut bus,
                remaining,
            );
            std::mem::swap(&mut self.mmu, bus.inner.mmu_mut());
            executed = executed.saturating_add(batch.executed);

            // Deterministically advance platform time based on executed CPU cycles.
            self.tick_platform_from_cycles(batch.executed);

            if let Some(kind) = self.reset_latch.take() {
                self.flush_serial();
                return RunExit::ResetRequested { kind, executed };
            }

            // Allow any started application processors (APs) to run a bounded amount of work per
            // host slice. APs begin in a halted wait-for-SIPI state and become runnable once the
            // BSP delivers a SIPI.
            self.run_ap_cpus(&cfg, remaining);

            match batch.exit {
                BatchExit::Completed => {
                    // `BatchExit::Completed` means the inner Tier-0 batch hit its instruction
                    // budget. Normally that budget is the remaining slice budget, but when A20 is
                    // disabled we may intentionally run smaller batches (see `remaining` above).
                    //
                    // Only treat this as a slice completion when we've consumed the full slice
                    // budget.
                    if executed >= max_insts {
                        self.flush_serial();
                        return RunExit::Completed { executed };
                    }
                    continue;
                }
                BatchExit::Branch => continue,
                BatchExit::Halted => {
                    // After advancing timers, poll again so any newly-due timer interrupts are
                    // injected into `cpu.pending.external_interrupts`.
                    //
                    // Only poll after the batch when we are going to re-enter execution within the
                    // same `run_slice` call. This avoids acknowledging interrupts at the end of a
                    // slice boundary (e.g. after an `STI` interrupt shadow expires) when the CPU
                    // will not execute another instruction until the host calls `run_slice` again.
                    //
                    // Process AHCI once more here so guests that issue an AHCI command and then
                    // execute `HLT` can still make DMA progress and be woken by INTx within the
                    // same `run_slice` call.
                    //
                    // Note: `poll_platform_interrupt` synchronizes PCI INTx source levels into the
                    // platform interrupt controller before polling, so we do not need an explicit
                    // `sync_pci_intx_sources_to_interrupts` call here.
                    self.process_ide();
                    self.process_ahci();
                    self.process_nvme();
                    self.process_virtio_blk();
                    self.process_aerogpu();
                    self.process_virtio_input();
                    // Like storage controllers, the guest may have kicked a NIC queue immediately
                    // before executing `HLT` (e.g. E1000 TX descriptor doorbell). Poll the network
                    // bridge again here so the device can complete DMA and raise INTx to wake the
                    // halted CPU within the same `run_slice` call.
                    self.poll_network();
                    if self.poll_platform_interrupt(MAX_QUEUED_EXTERNAL_INTERRUPTS) {
                        continue;
                    }

                    // When halted, advance platform time so timer interrupts can wake the CPU.
                    self.idle_tick_platform_1ms();
                    self.process_ide();
                    self.process_ahci();
                    self.process_nvme();
                    self.process_virtio_blk();
                    self.process_aerogpu();
                    self.process_virtio_input();
                    self.poll_network();
                    if self.poll_platform_interrupt(MAX_QUEUED_EXTERNAL_INTERRUPTS) {
                        continue;
                    }
                    self.flush_serial();
                    return RunExit::Halted { executed };
                }
                BatchExit::BiosInterrupt(vector) => {
                    self.handle_bios_interrupt(vector);
                }
                BatchExit::Assist(reason) => {
                    self.flush_serial();
                    return RunExit::Assist { reason, executed };
                }
                BatchExit::Exception(exception) => {
                    self.flush_serial();
                    return RunExit::Exception {
                        exception,
                        executed,
                    };
                }
                BatchExit::CpuExit(exit) => {
                    self.flush_serial();
                    return RunExit::CpuExit { exit, executed };
                }
            }
        }

        self.flush_serial();
        RunExit::Completed { executed }
    }

    fn with_legacy_vga_frontend_mut<F, R>(&mut self, f: F) -> Option<R>
    where
        F: FnOnce(&mut dyn LegacyVgaFrontend) -> R,
    {
        if let Some(vga) = &self.vga {
            let mut vga = vga.borrow_mut();
            return Some(f(&mut *vga));
        }

        if let Some(aerogpu) = &self.aerogpu {
            let mut dev = aerogpu.borrow_mut();
            return Some(f(&mut *dev));
        }

        None
    }

    fn sync_text_mode_cursor_bda_to_vga_crtc(&mut self) {
        if self.vga.is_none() && self.aerogpu.is_none() {
            return;
        }

        // BIOS Data Area (BDA) cursor state is the canonical source of truth for text-mode cursor
        // position/shape in our HLE BIOS. The emulated VGA device renders the cursor overlay based
        // on CRTC registers, so we mirror the BDA fields into those regs when in BIOS text mode.
        //
        // BDA layout (see `firmware::bda`):
        // - screen cols (u16)
        // - active page (u8)
        // - cursor pos for each page (row, col); we mirror the active page cursor into the CRTC
        // - cursor shape (start, end)
        let cols = BiosDataArea::read_screen_cols(&mut self.mem).max(1);
        let page = BiosDataArea::read_active_page(&mut self.mem);
        let (row, col) = BiosDataArea::read_cursor_pos(&mut self.mem, page);
        let (cursor_start, cursor_end) = BiosDataArea::read_cursor_shape(&mut self.mem);

        let cell_index = u16::from(row)
            .saturating_mul(cols)
            .saturating_add(u16::from(col));

        // CRTC index/data ports are 0x3D4/0x3D5.
        //
        // The BIOS cursor position is stored as a row/column pair, but the VGA CRTC cursor
        // location registers are in units of character cells relative to the current text start
        // address (CRTC regs 0x0C/0x0D). Read the current start address so the cursor remains
        // visible if the guest has panned the text window.
        let _ = self.with_legacy_vga_frontend_mut(|vga| {
            vga.port_write(0x3D4, 1, 0x0C);
            let start_hi = vga.port_read(0x3D5, 1) as u8;
            vga.port_write(0x3D4, 1, 0x0D);
            let start_lo = vga.port_read(0x3D5, 1) as u8;
            let start_addr = (((u16::from(start_hi)) << 8) | u16::from(start_lo)) & 0x3FFF;
            let cursor_addr = start_addr.wrapping_add(cell_index) & 0x3FFF;

            vga.port_write(0x3D4, 1, 0x0A);
            vga.port_write(0x3D5, 1, cursor_start as u32);
            vga.port_write(0x3D4, 1, 0x0B);
            vga.port_write(0x3D5, 1, cursor_end as u32);
            vga.port_write(0x3D4, 1, 0x0E);
            vga.port_write(0x3D5, 1, u32::from((cursor_addr >> 8) as u8));
            vga.port_write(0x3D4, 1, 0x0F);
            vga.port_write(0x3D5, 1, u32::from((cursor_addr & 0x00FF) as u8));
        });
    }

    fn aerogpu_bar1_base(&self) -> Option<u64> {
        let pci_cfg = self.pci_cfg.as_ref()?;
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(aero_devices::pci::profile::AEROGPU.bdf)?;
        cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
            .map(|range| range.base)
            .filter(|&base| base != 0)
    }

    fn legacy_vga_vram_size_bytes(&self) -> usize {
        self.legacy_vga_pci_bar_size_bytes() as usize
    }

    fn legacy_vga_pci_bar_size_bytes(&self) -> u32 {
        Self::legacy_vga_pci_bar_size_bytes_for_cfg(&self.cfg)
    }

    fn legacy_vga_pci_bar_size_bytes_for_cfg(cfg: &MachineConfig) -> u32 {
        // PCI BAR sizing must be a power of two, and the `Mmio32` definition expects a `u32`.
        let vram_size = cfg
            .vga_vram_size_bytes
            .unwrap_or(aero_gpu_vga::DEFAULT_VRAM_SIZE)
            .max(aero_gpu_vga::VGA_VRAM_SIZE);
        let vram_size = u32::try_from(vram_size).unwrap_or(u32::MAX);
        vram_size
            .max(0x10)
            .checked_next_power_of_two()
            // Largest power-of-two that fits in `u32` (required by `Mmio32` BARs).
            .unwrap_or(0x8000_0000)
    }

    fn legacy_vga_lfb_base(&self) -> u32 {
        let bar_size = self.legacy_vga_pci_bar_size_bytes();
        Self::legacy_vga_lfb_base_for_cfg(&self.cfg, bar_size).1
    }

    fn legacy_vga_lfb_base_for_cfg(cfg: &MachineConfig, bar_size: u32) -> (u32, u32) {
        // Keep the device model and LFB aperture base coherent by aligning down to the window size
        // (PCI config space masks BAR bases to the size's alignment, and the legacy VGA LFB uses
        // the same power-of-two alignment requirement).
        let lfb_offset = cfg
            .vga_lfb_offset
            .unwrap_or(aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET as u32);
        let requested_base = if let Some(base) = cfg.vga_lfb_base {
            base
        } else if let Some(vram_bar_base) = cfg.vga_vram_bar_base {
            vram_bar_base.wrapping_add(lfb_offset)
        } else {
            aero_gpu_vga::SVGA_LFB_BASE
        };
        let aligned_base = requested_base & !(bar_size.saturating_sub(1));
        (requested_base, aligned_base)
    }

    fn legacy_vga_device_config(&self) -> VgaConfig {
        let mut cfg = VgaConfig {
            vram_size: self.legacy_vga_vram_size_bytes(),
            ..Default::default()
        };
        if let Some(lfb_offset) = self.cfg.vga_lfb_offset {
            // Avoid panicking on invalid configuration; clamp to the allocated VRAM size so the
            // VGA device model's invariants hold.
            let max_off = u32::try_from(cfg.vram_size).unwrap_or(u32::MAX);
            cfg.lfb_offset = lfb_offset.min(max_off);
        }
        cfg.vram_bar_base = self.legacy_vga_lfb_base().wrapping_sub(cfg.lfb_offset);
        cfg
    }

    fn sync_bios_vbe_lfb_base_to_display_wiring(&mut self) {
        // Keep the BIOS-reported VBE linear framebuffer base coherent with the active display
        // device model.
        //
        // - Legacy VGA/VBE device model: the VGA device's configured LFB base.
        // - AeroGPU: BAR1_BASE + VBE_LFB_OFFSET (within the VRAM aperture).
        // - Headless: default RAM-backed base (safe, avoids overlap with PCI MMIO window).
        let use_legacy_vga = self.cfg.enable_vga && !self.cfg.enable_aerogpu;
        let lfb_base = if use_legacy_vga {
            self.vga
                .as_ref()
                .map(|vga| vga.borrow().lfb_base())
                .unwrap_or_else(|| self.legacy_vga_lfb_base())
        } else if self.cfg.enable_aerogpu {
            self.aerogpu_bar1_base()
                .and_then(|base| u32::try_from(base.saturating_add(VBE_LFB_OFFSET as u64)).ok())
                .unwrap_or(firmware::video::vbe::VbeDevice::LFB_BASE_DEFAULT)
        } else {
            firmware::video::vbe::VbeDevice::LFB_BASE_DEFAULT
        };

        self.bios.video.vbe.lfb_base = lfb_base;

        // Keep the AeroGPU legacy window mapping coherent with BIOS VBE state for banked access.
        if let Some(aerogpu) = &self.aerogpu {
            let bios_mode_id = self.bios.video.vbe.current_mode;
            let bios_vbe_active = bios_mode_id.is_some();

            let mut dev = aerogpu.borrow_mut();
            dev.vbe_mode_active = bios_vbe_active;
            if bios_vbe_active || !dev.vbe_dispi_guest_owned {
                dev.vbe_bank = self.bios.video.vbe.bank;
            }

            // Mirror BIOS-driven VBE mode state into the Bochs VBE_DISPI register file when the
            // guest has not explicitly taken ownership of the interface.
            if !dev.vbe_dispi_guest_owned {
                if let Some(mode_id) = bios_mode_id {
                    if let Some(mode) = self.bios.video.vbe.find_mode(mode_id) {
                        dev.vbe_dispi_xres = mode.width;
                        dev.vbe_dispi_yres = mode.height;
                        dev.vbe_dispi_bpp = mode.bpp as u16;
                        // Bochs VBE_DISPI enable: bit0=enable, bit6=linear framebuffer.
                        dev.vbe_dispi_enable = 0x0041;
                        dev.vbe_dispi_virt_width =
                            self.bios.video.vbe.logical_width_pixels.max(mode.width);
                        dev.vbe_dispi_virt_height = mode.height;
                        dev.vbe_dispi_x_offset = self.bios.video.vbe.display_start_x;
                        dev.vbe_dispi_y_offset = self.bios.video.vbe.display_start_y;
                    }
                } else {
                    // Ensure the Bochs register file does not advertise a stale enabled mode when
                    // the BIOS has reverted to text mode.
                    dev.vbe_dispi_xres = 0;
                    dev.vbe_dispi_yres = 0;
                    dev.vbe_dispi_bpp = 0;
                    dev.vbe_dispi_enable = 0;
                    dev.vbe_dispi_virt_width = 0;
                    dev.vbe_dispi_virt_height = 0;
                    dev.vbe_dispi_x_offset = 0;
                    dev.vbe_dispi_y_offset = 0;
                }
            }
        }
    }

    fn handle_bios_interrupt(&mut self, vector: u8) {
        let ax_before = self.cpu.state.gpr[gpr::RAX] as u16;
        let bx_before = self.cpu.state.gpr[gpr::RBX] as u16;
        let cx_before = self.cpu.state.gpr[gpr::RCX] as u16;
        let dx_before = self.cpu.state.gpr[gpr::RDX] as u16;
        let vbe_mode_before = self.bios.video.vbe.current_mode;
        #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
        let vbe_scanout_sig_before = vbe_mode_before.map(|mode| {
            (
                mode,
                self.bios.video.vbe.lfb_base,
                self.bios.video.vbe.bytes_per_scan_line,
                self.bios.video.vbe.display_start_x,
                self.bios.video.vbe.display_start_y,
            )
        });

        // Detect classic VGA mode sets (INT 10h AH=00h). The HLE BIOS updates BDA state and clears
        // framebuffer memory, but does not perform VGA port I/O itself. Program the emulated VGA
        // device so BIOS memory writes (e.g. clears) target the correct mapping.
        let int10_is_set_mode = vector == 0x10 && (ax_before & 0xFF00) == 0x0000;
        let set_mode = (ax_before as u8) & 0x7F;
        let is_set_mode_03h = int10_is_set_mode && set_mode == 0x03;
        let is_set_mode_13h = int10_is_set_mode && set_mode == 0x13;

        if int10_is_set_mode {
            let _ = self.with_legacy_vga_frontend_mut(|vga| {
                if is_set_mode_13h {
                    vga.set_mode_13h();
                } else if is_set_mode_03h {
                    vga.set_text_mode_80x25();
                }
            });
        }

        // VBE mode sets (INT 10h AX=4F02) optionally clear the framebuffer. The HLE BIOS
        // implements this as a byte-at-a-time write loop, which is unnecessarily slow when
        // targeting an emulated VGA device.
        //
        // When VGA is enabled and the guest did not request "no clear", force the BIOS path to
        // skip its slow clear and perform a fast host-side clear after the mode is enabled.
        let force_vbe_no_clear = vector == 0x10
            && ax_before == 0x4F02
            && (bx_before & 0x8000) == 0
            && (self.vga.is_some() || self.aerogpu.is_some());
        if force_vbe_no_clear {
            self.cpu.state.gpr[gpr::RBX] |= 0x8000;
        }

        // Keep the core's A20 view coherent with the chipset latch while executing BIOS services.
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        {
            let mut cdrom = self.install_media.as_ref().and_then(InstallMedia::upgrade);
            let cdrom = cdrom
                .as_mut()
                .map(|iso| iso as &mut dyn firmware::bios::CdromDevice);
            let bus: &mut dyn BiosBus = &mut self.mem;
            self.bios
                .dispatch_interrupt(vector, &mut self.cpu.state, bus, &mut self.disk, cdrom);
        }
        if force_vbe_no_clear {
            // Restore the guest-visible BX value (don't leak our forced no-clear flag).
            self.cpu.state.gpr[gpr::RBX] &= !0x8000;
        }
        let ax_after = self.cpu.state.gpr[gpr::RAX] as u16;

        // The HLE BIOS uses its own configuration to determine the VBE LFB base and may overwrite
        // `VbeDevice::lfb_base` during INT 10h services (e.g. mode sets). Keep it coherent with the
        // machine's active display wiring (VGA vs AeroGPU vs headless) so subsequent framebuffer
        // reads/writes target the correct backing store.
        if vector == 0x10 {
            self.sync_bios_vbe_lfb_base_to_display_wiring();
        }

        // If the BIOS halts the CPU during an interrupt (currently only via `bios_panic`), mirror
        // the panic message into COM1 so host callers can surface it via the serial log.
        if self.cpu.state.halted {
            self.mirror_bios_panic_to_serial();
        }

        // If we forced "no clear" for AeroGPU (MMIO-backed VRAM) mode sets, perform the clear
        // directly on the VRAM backing store instead of letting the BIOS run a slow byte loop.
        //
        // The VGA path does the same thing but clears the VGA device model's internal VRAM (see
        // below).
        if force_vbe_no_clear
            && self.vga.is_none()
            && self.aerogpu.is_some()
            && ax_before == 0x4F02
            && ax_after == 0x004F
            && (bx_before & 0x8000) == 0
        {
            if let (Some(mode_id), Some(bar1_base)) =
                (self.bios.video.vbe.current_mode, self.aerogpu_bar1_base())
            {
                if let Some(mode) = self.bios.video.vbe.find_mode(mode_id) {
                    let pitch = u64::from(
                        self.bios
                            .video
                            .vbe
                            .bytes_per_scan_line
                            .max(mode.bytes_per_scan_line()),
                    );
                    let clear_len = pitch.saturating_mul(u64::from(mode.height));

                    let lfb_base = u64::from(self.bios.video.vbe.lfb_base);
                    if lfb_base >= bar1_base {
                        let off = (lfb_base - bar1_base) as usize;
                        let end = off.saturating_add(clear_len as usize);
                        if let Some(aerogpu) = &self.aerogpu {
                            let mut dev = aerogpu.borrow_mut();
                            let end = end.min(dev.vram.len());
                            if off < end {
                                dev.vram[off..end].fill(0);
                            }
                        }
                    }
                }
            }
        }

        // The BIOS INT 10h implementation is HLE and only updates its internal `firmware::video`
        // state + writes to guest memory; it does not program VGA/VBE ports. Mirror relevant VBE
        // state into the emulated VGA device so mode sets immediately affect the visible output.
        if vector == 0x10 {
            if let Some(vga) = &self.vga {
                match self.bios.video.vbe.current_mode {
                    Some(mode) => {
                        if let Some(mode_info) = self.bios.video.vbe.find_mode(mode) {
                            let width = mode_info.width;
                            let height = mode_info.height;
                            let bpp = mode_info.bpp as u16;

                            let bank = self.bios.video.vbe.bank;
                            let virt_width = self.bios.video.vbe.logical_width_pixels.max(width);
                            let x_off = self.bios.video.vbe.display_start_x;
                            let y_off = self.bios.video.vbe.display_start_y;

                            let mut vga = vga.borrow_mut();
                            vga.set_svga_mode(width, height, bpp, /* lfb */ true);

                            // Mirror BIOS VBE state into Bochs VBE_DISPI regs via the port I/O path so
                            // the VGA device marks the output dirty when panning/stride changes.
                            vga.port_write(aero_gpu_vga::VBE_DISPI_INDEX_PORT, 2, 0x0005);
                            vga.port_write(aero_gpu_vga::VBE_DISPI_DATA_PORT, 2, u32::from(bank));
                            vga.port_write(aero_gpu_vga::VBE_DISPI_INDEX_PORT, 2, 0x0006);
                            vga.port_write(
                                aero_gpu_vga::VBE_DISPI_DATA_PORT,
                                2,
                                u32::from(virt_width),
                            );
                            vga.port_write(aero_gpu_vga::VBE_DISPI_INDEX_PORT, 2, 0x0008);
                            vga.port_write(aero_gpu_vga::VBE_DISPI_DATA_PORT, 2, u32::from(x_off));
                            vga.port_write(aero_gpu_vga::VBE_DISPI_INDEX_PORT, 2, 0x0009);
                            vga.port_write(aero_gpu_vga::VBE_DISPI_DATA_PORT, 2, u32::from(y_off));
                            vga.set_vbe_bytes_per_scan_line_override(
                                self.bios.video.vbe.bytes_per_scan_line,
                            );

                            // Palette updates: INT 10h AX=4F09 "Set Palette Data".
                            //
                            // The HLE BIOS stores VBE palette data internally but does not program the
                            // VGA DAC ports. For 8bpp VBE modes, mirror the updated palette into the
                            // device's DAC so rendered output matches BIOS state.
                            //
                            // Note: The VGA model's DAC programming path accepts 6-bit components; when
                            // the BIOS is configured for an 8-bit DAC, we downscale (>>2).
                            let bl = (bx_before & 0x00FF) as u8;
                            if bpp == 8
                                && ax_before == 0x4F09
                                && ax_after == 0x004F
                                && (bl & 0x7F) == 0
                            {
                                let start = (dx_before as usize).min(255);
                                let count = (cx_before as usize).min(256 - start);
                                if count != 0 {
                                    // Set DAC write index to `start`.
                                    vga.port_write(0x3C8, 1, start as u32);

                                    let bits = self.bios.video.vbe.dac_width_bits;
                                    for idx in start..start + count {
                                        let base = idx * 4;
                                        let b = self.bios.video.vbe.palette[base];
                                        let g = self.bios.video.vbe.palette[base + 1];
                                        let r = self.bios.video.vbe.palette[base + 2];

                                        let (r, g, b) = if bits >= 8 {
                                            (r >> 2, g >> 2, b >> 2)
                                        } else {
                                            (r & 0x3F, g & 0x3F, b & 0x3F)
                                        };

                                        // VGA DAC write order is R, G, B.
                                        vga.port_write(0x3C9, 1, r as u32);
                                        vga.port_write(0x3C9, 1, g as u32);
                                        vga.port_write(0x3C9, 1, b as u32);
                                    }
                                }
                            }

                            // If the guest requested a clear (BX bit15 clear), perform an efficient
                            // host-side clear after enabling the mode.
                            //
                            // Note: The machine forces the BIOS to skip its byte-at-a-time clear loop
                            // by temporarily setting the VBE no-clear flag before dispatching the
                            // interrupt (see `force_vbe_no_clear` above).
                            if ax_before == 0x4F02
                                && ax_after == 0x004F
                                && (bx_before & 0x8000) == 0
                            {
                                let bytes_per_pixel = (bpp as usize).div_ceil(8);
                                let clear_len = (width as usize)
                                    .saturating_mul(height as usize)
                                    .saturating_mul(bytes_per_pixel.max(1));
                                let fb_base = VBE_LFB_OFFSET;
                                let vram_len = vga.vram().len();
                                if fb_base < vram_len {
                                    let vbe_len = vram_len - fb_base;
                                    let clear_len = clear_len.min(vbe_len);
                                    let end = fb_base + clear_len;
                                    vga.vram_mut()[fb_base..end].fill(0);
                                }
                            }
                        }
                    }
                    None => {
                        // Only reset the VGA register file when the BIOS actually performed a mode
                        // set (e.g. INT 10h AH=00 AL=03h / 13h), or when transitioning from a VBE mode
                        // back to BIOS text mode.
                        //
                        // This avoids clobbering guest-programmed text registers (like the CRTC start
                        // address used for paging) on unrelated INT 10h text services (cursor moves,
                        // teletype output, etc.).
                        if is_set_mode_13h {
                            vga.borrow_mut().set_mode_13h();
                        } else if is_set_mode_03h || vbe_mode_before.is_some() {
                            vga.borrow_mut().set_text_mode_80x25();
                        }
                    }
                }
            }

            // Mirror VBE palette updates into the legacy VGA frontend even when the standalone VGA
            // device model is disabled (AeroGPU-backed legacy VGA decode).
            //
            // Note: The AeroGPU VBE scanout path (`display_present_aerogpu_vbe_lfb`) renders using
            // the BIOS' canonical palette array, so this primarily exists for guest software that
            // expects VGA DAC ports (0x3C8/0x3C9) to reflect BIOS palette state.
            if self.vga.is_none() {
                let bl = (bx_before & 0x00FF) as u8;
                if ax_before == 0x4F09 && ax_after == 0x004F && (bl & 0x7F) == 0 {
                    if let Some(mode_id) = self.bios.video.vbe.current_mode {
                        if let Some(mode_info) = self.bios.video.vbe.find_mode(mode_id) {
                            if mode_info.bpp == 8 {
                                let start = (dx_before as usize).min(255);
                                let count = (cx_before as usize).min(256 - start);
                                if count != 0 {
                                    let bits = self.bios.video.vbe.dac_width_bits;
                                    let mut entries: Vec<(u8, u8, u8)> = Vec::with_capacity(count);
                                    for idx in start..start + count {
                                        let base = idx * 4;
                                        let b = self.bios.video.vbe.palette[base];
                                        let g = self.bios.video.vbe.palette[base + 1];
                                        let r = self.bios.video.vbe.palette[base + 2];
                                        let (r, g, b) = if bits >= 8 {
                                            (r >> 2, g >> 2, b >> 2)
                                        } else {
                                            (r & 0x3F, g & 0x3F, b & 0x3F)
                                        };
                                        entries.push((r, g, b));
                                    }

                                    let _ = self.with_legacy_vga_frontend_mut(|vga| {
                                        vga.port_write(0x3C8, 1, start as u32);
                                        for (r, g, b) in entries {
                                            vga.port_write(0x3C9, 1, r as u32);
                                            vga.port_write(0x3C9, 1, g as u32);
                                            vga.port_write(0x3C9, 1, b as u32);
                                        }
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // INT 10h AH=05 "Select Active Display Page" updates the BDA but does not program VGA
            // ports in our HLE BIOS. Mirror the active-page start address into the CRTC start
            // address regs so the visible text window matches BIOS state (and so guests that read
            // VGA ports observe coherent values even when AeroGPU owns the legacy VGA decode).
            if self.bios.video.vbe.current_mode.is_none() && (ax_before & 0xFF00) == 0x0500 {
                let page = BiosDataArea::read_active_page(&mut self.mem);
                let page_size_bytes = BiosDataArea::read_page_size(&mut self.mem);
                let cells_per_page = page_size_bytes / 2;
                let start_addr = u16::from(page).saturating_mul(cells_per_page) & 0x3FFF;
                let _ = self.with_legacy_vga_frontend_mut(|vga| {
                    vga.port_write(0x3D4, 1, 0x0C);
                    vga.port_write(0x3D5, 1, u32::from((start_addr >> 8) as u8));
                    vga.port_write(0x3D4, 1, 0x0D);
                    vga.port_write(0x3D5, 1, u32::from((start_addr & 0x00FF) as u8));
                });
            }

            // Keep the AeroGPU legacy window coherent with BIOS VBE state so the A0000 banked window
            // behaves like the VBE window advertised by `VbeDevice::write_mode_info`.
            //
            // Additionally, mirror VBE palette updates (INT 10h AX=4F09 "Set Palette Data") into the
            // AeroGPU-emulated VGA DAC ports so software that uses BIOS palette services observes
            // coherent state via both paths.
            if let Some(aerogpu) = &self.aerogpu {
                let bl = (bx_before & 0x00FF) as u8;

                // Palette updates: INT 10h AX=4F09 "Set Palette Data".
                //
                // The HLE BIOS stores palette entries internally as B,G,R,0 but does not perform
                // port I/O. For 8bpp VBE modes, mirror the updated palette into the device's DAC.
                //
                // Note: The AeroGPU DAC (like the VGA model) stores 6-bit components. When the BIOS
                // is configured for an 8-bit DAC, downscale (>>2) so palette reads via ports return
                // the expected 6-bit values.
                let sync_range = if ax_before == 0x4F09
                    && ax_after == 0x004F
                    && (bl & 0x7F) == 0
                    && self
                        .bios
                        .video
                        .vbe
                        .current_mode
                        .and_then(|mode| self.bios.video.vbe.find_mode(mode))
                        .is_some_and(|mode| mode.bpp == 8)
                {
                    let start = (dx_before as usize).min(255);
                    let count = (cx_before as usize).min(256 - start);
                    (count != 0).then_some((start, count))
                } else {
                    None
                };

                let mut dev = aerogpu.borrow_mut();
                if let Some((start, count)) = sync_range {
                    // Set DAC write index to `start`.
                    dev.vga_port_write_u8(0x3C8, start as u8);

                    let bits = self.bios.video.vbe.dac_width_bits;
                    for idx in start..start + count {
                        let base = idx * 4;
                        let b = self.bios.video.vbe.palette[base];
                        let g = self.bios.video.vbe.palette[base + 1];
                        let r = self.bios.video.vbe.palette[base + 2];

                        let (r, g, b) = if bits >= 8 {
                            (r >> 2, g >> 2, b >> 2)
                        } else {
                            (r & 0x3F, g & 0x3F, b & 0x3F)
                        };

                        // VGA DAC write order is R, G, B.
                        dev.vga_port_write_u8(0x3C9, r);
                        dev.vga_port_write_u8(0x3C9, g);
                        dev.vga_port_write_u8(0x3C9, b);
                    }
                }
            }
        }
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();

        // INT 10h text services update the BDA cursor state but do not perform VGA port I/O.
        // Keep the emulated VGA device coherent so its cursor overlay matches BIOS state during
        // early boot.
        if vector == 0x10 && self.bios.video.vbe.current_mode.is_none() {
            self.sync_text_mode_cursor_bda_to_vga_crtc();
        }

        // Publish legacy scanout transitions (text <-> VBE LFB) so external presentation layers
        // can follow BIOS-driven mode sets and panning/stride updates.
        //
        // If the scanout is currently owned by the WDDM path, do not allow legacy INT 10h calls
        // to steal it back until the VM resets. `SCANOUT0_ENABLE=0` is treated as a visibility
        // toggle (blanking) and does not release WDDM ownership back to legacy VGA/VBE.
        #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
        if vector == 0x10 {
            let vbe_scanout_sig_after = self.bios.video.vbe.current_mode.map(|mode| {
                (
                    mode,
                    self.bios.video.vbe.lfb_base,
                    self.bios.video.vbe.bytes_per_scan_line,
                    self.bios.video.vbe.display_start_x,
                    self.bios.video.vbe.display_start_y,
                )
            });

            if vbe_scanout_sig_before != vbe_scanout_sig_after {
                let Some(scanout_state) = &self.scanout_state else {
                    return;
                };
                match scanout_state.try_snapshot() {
                    Some(snap) if snap.source == SCANOUT_SOURCE_WDDM => return,
                    None => return,
                    _ => {}
                }

                match vbe_scanout_sig_after {
                    None => {
                        let _ = scanout_state.try_publish(ScanoutStateUpdate {
                            source: SCANOUT_SOURCE_LEGACY_TEXT,
                            base_paddr_lo: 0,
                            base_paddr_hi: 0,
                            width: 0,
                            height: 0,
                            pitch_bytes: 0,
                            format: SCANOUT_FORMAT_B8G8R8X8,
                        });
                    }
                    Some((mode, lfb_base, bytes_per_scan_line, start_x, start_y)) => {
                        if let Some(mode_info) = self.bios.video.vbe.find_mode(mode) {
                            let legacy_text = ScanoutStateUpdate {
                                source: SCANOUT_SOURCE_LEGACY_TEXT,
                                base_paddr_lo: 0,
                                base_paddr_hi: 0,
                                width: 0,
                                height: 0,
                                pitch_bytes: 0,
                                format: SCANOUT_FORMAT_B8G8R8X8,
                            };

                            // This legacy VBE scanout publication path currently only supports the
                            // canonical boot pixel format: 32bpp packed pixels `B8G8R8X8`.
                            //
                            // If the guest selects a palettized VBE mode (e.g. 8bpp), fall back to
                            // the implicit legacy path rather than publishing a misleading 32bpp
                            // descriptor.
                            if mode_info.bpp != 32 {
                                let _ = scanout_state.try_publish(legacy_text);
                            } else {
                                let pitch = u64::from(
                                    bytes_per_scan_line.max(mode_info.bytes_per_scan_line()),
                                );
                                // 32bpp direct-color VBE modes use little-endian packed pixels
                                // B8G8R8X8.
                                let bytes_per_pixel = 4u64;
                                let base = u64::from(lfb_base)
                                    .saturating_add(u64::from(start_y).saturating_mul(pitch))
                                    .saturating_add(
                                        u64::from(start_x).saturating_mul(bytes_per_pixel),
                                    );

                                let _ = scanout_state.try_publish(ScanoutStateUpdate {
                                    source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
                                    base_paddr_lo: base as u32,
                                    base_paddr_hi: (base >> 32) as u32,
                                    width: u32::from(mode_info.width),
                                    height: u32::from(mode_info.height),
                                    pitch_bytes: pitch as u32,
                                    format: SCANOUT_FORMAT_B8G8R8X8,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    fn poll_platform_interrupt(&mut self, max_queued: usize) -> bool {
        // Synchronize PCI INTx sources (e.g. E1000) into the platform interrupt controller *before*
        // we poll/acknowledge for pending vectors.
        //
        // This must happen even when the guest cannot currently accept maskable interrupts (IF=0 /
        // interrupt shadow), and even when our external-interrupt FIFO is at capacity, so
        // level-triggered lines remain accurately asserted/deasserted until delivery is possible.
        self.sync_pci_intx_sources_to_interrupts();

        if self.cpu.pending.external_interrupts().len() >= max_queued {
            return false;
        }

        // Only acknowledge/present a maskable interrupt to the CPU when it can be delivered.
        //
        // The platform interrupt controller (PIC/IOAPIC+LAPIC) latches interrupts until the CPU
        // performs an acknowledge handshake. If we acknowledge while the CPU is unable to accept
        // delivery (IF=0, interrupt shadow, pending exception), we could incorrectly clear the
        // controller and lose the interrupt.
        if self.cpu.pending.has_pending_event()
            || (self.cpu.state.rflags() & RFLAGS_IF) == 0
            || self.cpu.pending.interrupt_inhibit() != 0
        {
            return false;
        }

        let Some(interrupts) = &self.interrupts else {
            return false;
        };

        let mut interrupts = interrupts.borrow_mut();
        let vector = PlatformInterruptController::get_pending(&*interrupts);
        let Some(vector) = vector else {
            return false;
        };

        PlatformInterruptController::acknowledge(&mut *interrupts, vector);
        self.cpu.pending.inject_external_interrupt(vector);
        true
    }

    fn poll_platform_interrupt_for_apic(
        interrupts: Option<&Rc<RefCell<PlatformInterrupts>>>,
        apic_id: u8,
        cpu: &mut CpuCore,
        max_queued: usize,
    ) -> bool {
        if cpu.pending.external_interrupts().len() >= max_queued {
            return false;
        }

        // Only acknowledge/present a maskable interrupt to the vCPU when it can be delivered.
        //
        // If we acknowledge while the vCPU is unable to accept delivery (IF=0, interrupt shadow,
        // pending exception), we could incorrectly clear the controller and lose the interrupt.
        if cpu.pending.has_pending_event()
            || (cpu.state.rflags() & RFLAGS_IF) == 0
            || cpu.pending.interrupt_inhibit() != 0
        {
            return false;
        }

        let Some(interrupts) = interrupts else {
            return false;
        };

        let mut interrupts = interrupts.borrow_mut();
        let Some(vector) = interrupts.get_pending_for_apic(apic_id) else {
            return false;
        };

        interrupts.acknowledge_for_apic(apic_id, vector);
        cpu.pending.inject_external_interrupt(vector);
        // Wake the vCPU from a `HLT` wait state.
        cpu.state.halted = false;
        true
    }

    fn flush_serial(&mut self) {
        let Some(uart) = &self.serial else {
            return;
        };
        let mut uart = uart.borrow_mut();
        let tx = uart.take_tx();
        if !tx.is_empty() {
            self.serial_log.extend_from_slice(&tx);
        }
    }

    fn mirror_bios_panic_to_serial(&mut self) {
        let Some(uart) = &self.serial else {
            return;
        };
        let tty = self.bios.tty_output();
        if tty.is_empty() {
            return;
        }

        // Best-effort: extract the last non-empty line so we don't spam the full rolling TTY log
        // (which may contain unrelated debug output).
        let mut end = tty.len();
        while end > 0 && tty[end - 1] == b'\n' {
            end -= 1;
        }
        if end == 0 {
            return;
        }
        let start = tty[..end]
            .iter()
            .rposition(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let line = &tty[start..end];

        let mut uart = uart.borrow_mut();
        for &b in b"BIOS panic: " {
            uart.write_u8(0x3F8, b);
        }
        for &b in line {
            uart.write_u8(0x3F8, b);
        }
        uart.write_u8(0x3F8, b'\n');
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MachineUsbSnapshot {
    uhci: Option<Vec<u8>>,
    uhci_ns_remainder: u64,
    ehci: Option<Vec<u8>>,
    ehci_ns_remainder: u64,
    xhci: Option<Vec<u8>>,
    xhci_ns_remainder: u64,
}

impl MachineUsbSnapshot {
    const TAG_UHCI_NS_REMAINDER: u16 = 1;
    const TAG_UHCI_STATE: u16 = 2;
    const TAG_EHCI_NS_REMAINDER: u16 = 3;
    const TAG_EHCI_STATE: u16 = 4;
    const TAG_XHCI_NS_REMAINDER: u16 = 5;
    const TAG_XHCI_STATE: u16 = 6;
}

impl IoSnapshot for MachineUsbSnapshot {
    const DEVICE_ID: [u8; 4] = *b"USBC";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 2);

    fn save_state(&self) -> Vec<u8> {
        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        if let Some(uhci) = &self.uhci {
            w.field_u64(Self::TAG_UHCI_NS_REMAINDER, self.uhci_ns_remainder);
            w.field_bytes(Self::TAG_UHCI_STATE, uhci.clone());
        }
        if let Some(ehci) = &self.ehci {
            w.field_u64(Self::TAG_EHCI_NS_REMAINDER, self.ehci_ns_remainder);
            w.field_bytes(Self::TAG_EHCI_STATE, ehci.clone());
        }
        if let Some(xhci) = &self.xhci {
            w.field_u64(Self::TAG_XHCI_NS_REMAINDER, self.xhci_ns_remainder);
            w.field_bytes(Self::TAG_XHCI_STATE, xhci.clone());
        }
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> IoSnapshotResult<()> {
        let r = IoSnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        self.uhci_ns_remainder = r.u64(Self::TAG_UHCI_NS_REMAINDER)?.unwrap_or(0);
        self.uhci = r.bytes(Self::TAG_UHCI_STATE).map(|buf| buf.to_vec());

        self.ehci_ns_remainder = r.u64(Self::TAG_EHCI_NS_REMAINDER)?.unwrap_or(0);
        self.ehci = r.bytes(Self::TAG_EHCI_STATE).map(|buf| buf.to_vec());

        self.xhci_ns_remainder = r.u64(Self::TAG_XHCI_NS_REMAINDER)?.unwrap_or(0);
        self.xhci = r.bytes(Self::TAG_XHCI_STATE).map(|buf| buf.to_vec());
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MachineVirtioInputSnapshot {
    keyboard: Option<Vec<u8>>,
    mouse: Option<Vec<u8>>,
}

impl MachineVirtioInputSnapshot {
    const TAG_KEYBOARD_STATE: u16 = 1;
    const TAG_MOUSE_STATE: u16 = 2;
}

impl IoSnapshot for MachineVirtioInputSnapshot {
    const DEVICE_ID: [u8; 4] = *b"VINP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        if let Some(kbd) = &self.keyboard {
            w.field_bytes(Self::TAG_KEYBOARD_STATE, kbd.clone());
        }
        if let Some(mouse) = &self.mouse {
            w.field_bytes(Self::TAG_MOUSE_STATE, mouse.clone());
        }
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> IoSnapshotResult<()> {
        let r = IoSnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        self.keyboard = r.bytes(Self::TAG_KEYBOARD_STATE).map(|buf| buf.to_vec());
        self.mouse = r.bytes(Self::TAG_MOUSE_STATE).map(|buf| buf.to_vec());
        Ok(())
    }
}

impl snapshot::SnapshotSource for Machine {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        let snapshot_id = self.next_snapshot_id;
        self.next_snapshot_id = self.next_snapshot_id.saturating_add(1);

        #[cfg(target_arch = "wasm32")]
        let created_unix_ms = 0u64;
        #[cfg(not(target_arch = "wasm32"))]
        let created_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);

        let meta = snapshot::SnapshotMeta {
            snapshot_id,
            parent_snapshot_id: self.last_snapshot_id,
            created_unix_ms,
            label: None,
        };
        self.last_snapshot_id = Some(snapshot_id);
        meta
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::cpu_state_from_cpu_core(&self.cpu.state)
    }

    fn cpu_states(&self) -> Vec<snapshot::VcpuSnapshot> {
        let cpu_count = self.cfg.cpu_count as usize;
        let mut cpus = Vec::with_capacity(cpu_count);

        cpus.push(snapshot::VcpuSnapshot {
            apic_id: 0,
            cpu: snapshot::cpu_state_from_cpu_core(&self.cpu.state),
            internal_state: Vec::new(),
        });

        for (idx, cpu) in self.ap_cpus.iter().enumerate() {
            cpus.push(snapshot::VcpuSnapshot {
                apic_id: (idx + 1) as u32,
                cpu: snapshot::cpu_state_from_cpu_core(&cpu.state),
                internal_state: Vec::new(),
            });
        }

        cpus
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::mmu_state_from_cpu_core(&self.cpu.state)
    }

    fn mmu_states(&self) -> Vec<snapshot::VcpuMmuSnapshot> {
        let cpu_count = self.cfg.cpu_count as usize;
        let mut mmus = Vec::with_capacity(cpu_count);

        mmus.push(snapshot::VcpuMmuSnapshot {
            apic_id: 0,
            mmu: snapshot::mmu_state_from_cpu_core(&self.cpu.state),
        });

        for (idx, cpu) in self.ap_cpus.iter().enumerate() {
            mmus.push(snapshot::VcpuMmuSnapshot {
                apic_id: (idx + 1) as u32,
                mmu: snapshot::mmu_state_from_cpu_core(&cpu.state),
            });
        }

        mmus
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        const V1: u16 = 1;
        let mut devices = Vec::new();
        const MSIX_MESSAGE_CONTROL_OFFSET: u16 = 0x02;
        // MSI-X capability Message Control bits we mirror from canonical PCI config space into
        // runtime device models:
        // - bit 15: MSI-X Enable
        // - bit 14: Function Mask
        const MSIX_MESSAGE_CONTROL_MIRROR_MASK: u16 = (1 << 15) | (1 << 14);

        // Firmware snapshot: required for deterministic BIOS interrupt behavior.
        let bios_snapshot = self.bios.snapshot();
        let mut bios_bytes = Vec::new();
        if bios_snapshot.encode(&mut bios_bytes).is_ok() {
            devices.push(snapshot::DeviceState {
                id: snapshot::DeviceId::BIOS,
                version: V1,
                flags: 0,
                data: bios_bytes,
            });
        }

        // Memory/chipset glue.
        devices.push(snapshot::DeviceState {
            id: snapshot::DeviceId::MEMORY,
            version: V1,
            flags: 0,
            data: {
                // Legacy snapshots stored only A20 enabled state.
                // Newer snapshots append the platform clock (ns) so time-based device models
                // (RTC/HPET/AeroGPU vblank scheduling) can be restored deterministically.
                let mut data = Vec::with_capacity(1 + 8);
                data.push(self.chipset.a20().enabled() as u8);
                let now_ns = self
                    .platform_clock
                    .as_ref()
                    .map(aero_interrupts::clock::Clock::now_ns)
                    .unwrap_or(0);
                data.extend_from_slice(&now_ns.to_le_bytes());
                data
            },
        });

        // Accumulated serial output (drained from the UART by `Machine::run_slice`).
        devices.push(snapshot::DeviceState {
            id: snapshot::DeviceId::SERIAL,
            version: V1,
            flags: 0,
            data: self.serial_log.clone(),
        });

        // VGA/VBE (registers + full VRAM).
        if let Some(vga) = &self.vga {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::VGA,
                &*vga.borrow(),
            ));
        }

        if let (Some(aerogpu), Some(aerogpu_mmio)) = (&self.aerogpu, &self.aerogpu_mmio) {
            devices.push(snapshot::DeviceState {
                id: snapshot::DeviceId::AEROGPU,
                version: AEROGPU_SNAPSHOT_VERSION_V2,
                flags: 0,
                data: encode_aerogpu_snapshot_v2(&aerogpu.borrow(), &aerogpu_mmio.borrow()),
            });
        }

        // Optional PC platform devices.
        //
        // Note: We snapshot the combined PIC + IOAPIC + LAPIC router state via `PlatformInterrupts`.
        // Prefer the dedicated `DeviceId::PLATFORM_INTERRUPTS` id; keep accepting the historical
        // `DeviceId::APIC` id for backward compatibility when restoring older snapshots.
        if let Some(interrupts) = &self.interrupts {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PLATFORM_INTERRUPTS,
                &*interrupts.borrow(),
            ));
        }
        if let Some(pit) = &self.pit {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PIT,
                &*pit.borrow(),
            ));
        }
        if let Some(rtc) = &self.rtc {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::RTC,
                &*rtc.borrow(),
            ));
        }
        // PCI core state (config ports + INTx router).
        //
        // Canonical full-machine snapshots store these as separate outer device entries to avoid
        // `DEVICES` duplicate `(id, version, flags)` collisions:
        // - `DeviceId::PCI_CFG` for `PciConfigPorts` (`PCPT`)
        // - `DeviceId::PCI_INTX_ROUTER` for `PciIntxRouter` (`INTX`)
        if let Some(pci_cfg) = &self.pci_cfg {
            // Canonical outer ID for legacy PCI config mechanism #1 ports (`0xCF8/0xCFC`) and
            // PCI bus config-space state.
            //
            // NOTE: `PciConfigPorts` snapshots cover both the config mechanism #1 address latch
            // and the per-device config space/BAR state, so this one entry is sufficient to
            // restore guest-programmed BARs and command bits.
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PCI_CFG,
                &*pci_cfg.borrow(),
            ));
        }
        if let Some(pci_intx) = &self.pci_intx {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PCI_INTX_ROUTER,
                &*pci_intx.borrow(),
            ));
        }

        if let Some(e1000) = &self.e1000 {
            // The E1000 model snapshots its internal PCI config image. Mirror the canonical PCI
            // command/BAR programming owned by `PciConfigPorts` so snapshots capture a coherent view
            // even when the guest reprograms BARs (including to base=0).
            let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
            let (command, bar0_base, bar1_base) = self
                .pci_cfg
                .as_ref()
                .map(|pci_cfg| {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg
                        .and_then(|cfg| cfg.bar_range(0))
                        .map(|range| range.base)
                        .unwrap_or(0);
                    let bar1_base = cfg
                        .and_then(|cfg| cfg.bar_range(1))
                        .map(|range| range.base)
                        .unwrap_or(0);
                    (command, bar0_base, bar1_base)
                })
                .unwrap_or((0, 0, 0));

            let mut nic = e1000.borrow_mut();
            nic.pci_config_write(0x04, 2, u32::from(command));
            if let Ok(bar0_base) = u32::try_from(bar0_base) {
                nic.pci_config_write(0x10, 4, bar0_base);
            }
            if let Ok(bar1_base) = u32::try_from(bar1_base) {
                nic.pci_config_write(0x14, 4, bar1_base);
            }

            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::E1000,
                &*nic,
            ));
        }
        if let Some(virtio_net) = &self.virtio_net {
            // Virtio devices gate DMA and legacy INTx semantics on the PCI command register, but the
            // machine owns the canonical PCI config space (`PciConfigPorts`). Mirror the command
            // register so the serialized virtio transport state reflects the guest-visible PCI
            // configuration.
            let bdf = aero_devices::pci::profile::VIRTIO_NET.bdf;
            if let Some(pci_cfg) = &self.pci_cfg {
                let (command, bar0_base, msix_ctrl_bits) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let mut command = 0;
                    let mut bar0_base = None;
                    let mut msix_ctrl_bits = None;
                    if let Some(cfg) = pci_cfg.bus_mut().device_config_mut(bdf) {
                        command = cfg.command();
                        bar0_base = cfg.bar_range(0).map(|range| range.base);
                        if let Some(msix_off) = cfg.find_capability(PCI_CAP_ID_MSIX) {
                            let ctrl = cfg
                                .read(u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET, 2)
                                as u16;
                            msix_ctrl_bits = Some(ctrl & MSIX_MESSAGE_CONTROL_MIRROR_MASK);
                        }
                    }
                    (command, bar0_base, msix_ctrl_bits)
                };
                let mut virtio_net = virtio_net.borrow_mut();
                virtio_net.set_pci_command(command);
                if let Some(bar0_base) = bar0_base {
                    virtio_net.config_mut().set_bar_base(0, bar0_base);
                }

                if let Some(msix_ctrl_bits) = msix_ctrl_bits {
                    if let Some(msix_off) = virtio_net.config_mut().find_capability(PCI_CAP_ID_MSIX)
                    {
                        let ctrl_off = u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET;
                        let runtime_ctrl = virtio_net.config_mut().read(ctrl_off, 2) as u16;
                        let new_ctrl =
                            (runtime_ctrl & !MSIX_MESSAGE_CONTROL_MIRROR_MASK) | msix_ctrl_bits;
                        virtio_net
                            .config_mut()
                            .write(ctrl_off, 2, u32::from(new_ctrl));
                    }
                }
            }

            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::VIRTIO_NET,
                &*virtio_net.borrow(),
            ));
        }
        if let Some(virtio_input_keyboard) = &self.virtio_input_keyboard {
            let bdf = aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD.bdf;
            if let Some(pci_cfg) = &self.pci_cfg {
                let (command, bar0_base, msix_ctrl_bits) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let mut command = 0;
                    let mut bar0_base = None;
                    let mut msix_ctrl_bits = None;
                    if let Some(cfg) = pci_cfg.bus_mut().device_config_mut(bdf) {
                        command = cfg.command();
                        bar0_base = cfg.bar_range(0).map(|range| range.base);
                        if let Some(msix_off) = cfg.find_capability(PCI_CAP_ID_MSIX) {
                            let ctrl = cfg
                                .read(u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET, 2)
                                as u16;
                            msix_ctrl_bits = Some(ctrl & MSIX_MESSAGE_CONTROL_MIRROR_MASK);
                        }
                    }
                    (command, bar0_base, msix_ctrl_bits)
                };

                let mut dev = virtio_input_keyboard.borrow_mut();
                dev.set_pci_command(command);
                if let Some(bar0_base) = bar0_base {
                    dev.config_mut().set_bar_base(0, bar0_base);
                }

                if let Some(msix_ctrl_bits) = msix_ctrl_bits {
                    if let Some(msix_off) = dev.config_mut().find_capability(PCI_CAP_ID_MSIX) {
                        let ctrl_off = u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET;
                        let runtime_ctrl = dev.config_mut().read(ctrl_off, 2) as u16;
                        let new_ctrl =
                            (runtime_ctrl & !MSIX_MESSAGE_CONTROL_MIRROR_MASK) | msix_ctrl_bits;
                        dev.config_mut().write(ctrl_off, 2, u32::from(new_ctrl));
                    }
                }
            }

            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::VIRTIO_INPUT_KEYBOARD,
                &*virtio_input_keyboard.borrow(),
            ));
        }

        if let Some(virtio_input_mouse) = &self.virtio_input_mouse {
            let bdf = aero_devices::pci::profile::VIRTIO_INPUT_MOUSE.bdf;
            if let Some(pci_cfg) = &self.pci_cfg {
                let (command, bar0_base, msix_ctrl_bits) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let mut command = 0;
                    let mut bar0_base = None;
                    let mut msix_ctrl_bits = None;
                    if let Some(cfg) = pci_cfg.bus_mut().device_config_mut(bdf) {
                        command = cfg.command();
                        bar0_base = cfg.bar_range(0).map(|range| range.base);
                        if let Some(msix_off) = cfg.find_capability(PCI_CAP_ID_MSIX) {
                            let ctrl = cfg
                                .read(u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET, 2)
                                as u16;
                            msix_ctrl_bits = Some(ctrl & MSIX_MESSAGE_CONTROL_MIRROR_MASK);
                        }
                    }
                    (command, bar0_base, msix_ctrl_bits)
                };

                let mut dev = virtio_input_mouse.borrow_mut();
                dev.set_pci_command(command);
                if let Some(bar0_base) = bar0_base {
                    dev.config_mut().set_bar_base(0, bar0_base);
                }

                if let Some(msix_ctrl_bits) = msix_ctrl_bits {
                    if let Some(msix_off) = dev.config_mut().find_capability(PCI_CAP_ID_MSIX) {
                        let ctrl_off = u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET;
                        let runtime_ctrl = dev.config_mut().read(ctrl_off, 2) as u16;
                        let new_ctrl =
                            (runtime_ctrl & !MSIX_MESSAGE_CONTROL_MIRROR_MASK) | msix_ctrl_bits;
                        dev.config_mut().write(ctrl_off, 2, u32::from(new_ctrl));
                    }
                }
            }

            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::VIRTIO_INPUT_MOUSE,
                &*virtio_input_mouse.borrow(),
            ));
        }
        if self.uhci.is_some() || self.ehci.is_some() || self.xhci.is_some() {
            let mut wrapper = MachineUsbSnapshot::default();

            if let Some(uhci) = &self.uhci {
                let bdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
                if let Some(pci_cfg) = &self.pci_cfg {
                    let (command, bar4_base) = {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let bar4_base = cfg
                            .and_then(|cfg| cfg.bar_range(UhciPciDevice::IO_BAR_INDEX))
                            .map(|range| range.base);
                        (command, bar4_base)
                    };

                    let mut uhci = uhci.borrow_mut();
                    uhci.config_mut().set_command(command);
                    if let Some(bar4_base) = bar4_base {
                        uhci.config_mut()
                            .set_bar_base(UhciPciDevice::IO_BAR_INDEX, bar4_base);
                    }
                }

                wrapper.uhci = Some(uhci.borrow().save_state());
                wrapper.uhci_ns_remainder = self.uhci_ns_remainder;
            }

            if let Some(ehci) = &self.ehci {
                let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
                if let Some(pci_cfg) = &self.pci_cfg {
                    let (command, bar0_base) = {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let bar0_base = cfg
                            .and_then(|cfg| cfg.bar_range(EhciPciDevice::MMIO_BAR_INDEX))
                            .map(|range| range.base);
                        (command, bar0_base)
                    };

                    let mut ehci = ehci.borrow_mut();
                    ehci.config_mut().set_command(command);
                    if let Some(bar0_base) = bar0_base {
                        ehci.config_mut()
                            .set_bar_base(EhciPciDevice::MMIO_BAR_INDEX, bar0_base);
                    }
                }

                wrapper.ehci = Some(ehci.borrow().save_state());
                wrapper.ehci_ns_remainder = self.ehci_ns_remainder;
            }

            if let Some(xhci) = &self.xhci {
                let bdf = aero_devices::pci::profile::USB_XHCI_QEMU.bdf;
                let (command, bar0_base, msi_state, msix_state) = self
                    .pci_cfg
                    .as_ref()
                    .map(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        let cfg = pci_cfg.bus_mut().device_config(bdf);
                        let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                        let bar0_base = cfg
                            .and_then(|cfg| cfg.bar_range(XhciPciDevice::MMIO_BAR_INDEX))
                            .map(|range| range.base);
                        let msi_state =
                            cfg.and_then(|cfg| cfg.capability::<MsiCapability>())
                                .map(|msi| {
                                    (
                                        msi.enabled(),
                                        msi.message_address(),
                                        msi.message_data(),
                                        msi.mask_bits(),
                                    )
                                });
                        let msix_state = cfg
                            .and_then(|cfg| cfg.capability::<MsixCapability>())
                            .map(|msix| (msix.enabled(), msix.function_masked()));
                        (command, bar0_base, msi_state, msix_state)
                    })
                    .unwrap_or((0, None, None, None));

                {
                    let mut xhci = xhci.borrow_mut();
                    let cfg = xhci.config_mut();
                    cfg.set_command(command);
                    if let Some(bar0_base) = bar0_base {
                        cfg.set_bar_base(XhciPciDevice::MMIO_BAR_INDEX, bar0_base);
                    }
                    if let Some((enabled, addr, data, mask)) = msi_state {
                        // Note: MSI pending bits are device-managed and must not be overwritten from the
                        // canonical PCI config space (which cannot observe them).
                        sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                    }
                    if let Some((enabled, function_masked)) = msix_state {
                        sync_msix_capability_into_config(cfg, enabled, function_masked);
                    }
                }

                wrapper.xhci = Some(xhci.borrow().save_state());
                wrapper.xhci_ns_remainder = self.xhci_ns_remainder;
            }

            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::USB,
                &wrapper,
            ));
        }
        // Storage controller(s).
        //
        // Canonical encoding for the outer `DeviceId::DISK_CONTROLLER` entry is the `DSKC` wrapper
        // (`DiskControllersSnapshot`). This allows a single device entry to carry multiple
        // different controller snapshots keyed by PCI BDF, avoiding `(id, version, flags)`
        // collisions when multiple controllers share the same inner `SnapshotVersion`.
        let mut disk_controllers = DiskControllersSnapshot::new();

        // Note: these models snapshot their own PCI config space state. Since `Machine` maintains a
        // separate canonical PCI config space for guest enumeration, mirror the live PCI command
        // (and relevant BAR bases) into the device models before snapshotting so the serialized
        // device blobs are coherent with the platform PCI state.
        if let Some(ahci) = &self.ahci {
            let bdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
            if let Some(pci_cfg) = &self.pci_cfg {
                let (command, bar5_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar5_base = cfg
                        .and_then(|cfg| {
                            cfg.bar_range(aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX)
                        })
                        .map(|range| range.base);
                    (command, bar5_base)
                };
                let mut ahci = ahci.borrow_mut();
                ahci.config_mut().set_command(command);
                if let Some(bar5_base) = bar5_base {
                    ahci.config_mut()
                        .set_bar_base(aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX, bar5_base);
                }
            }
            disk_controllers.insert(bdf.pack_u16(), ahci.borrow().save_state());
        }
        if let Some(nvme) = &self.nvme {
            let bdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
            if let Some(pci_cfg) = &self.pci_cfg {
                let (command, bar0_base, msix_ctrl_bits) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let mut command = 0;
                    let mut bar0_base = None;
                    let mut msix_ctrl_bits = None;
                    if let Some(cfg) = pci_cfg.bus_mut().device_config_mut(bdf) {
                        command = cfg.command();
                        bar0_base = cfg.bar_range(0).map(|range| range.base);
                        if let Some(msix_off) = cfg.find_capability(PCI_CAP_ID_MSIX) {
                            let ctrl = cfg
                                .read(u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET, 2)
                                as u16;
                            msix_ctrl_bits = Some(ctrl & MSIX_MESSAGE_CONTROL_MIRROR_MASK);
                        }
                    }
                    (command, bar0_base, msix_ctrl_bits)
                };
                let mut nvme = nvme.borrow_mut();
                nvme.config_mut().set_command(command);
                if let Some(bar0_base) = bar0_base {
                    nvme.config_mut().set_bar_base(0, bar0_base);
                }
                if let Some(msix_ctrl_bits) = msix_ctrl_bits {
                    if let Some(msix_off) = nvme.config_mut().find_capability(PCI_CAP_ID_MSIX) {
                        let ctrl_off = u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET;
                        let runtime_ctrl = nvme.config_mut().read(ctrl_off, 2) as u16;
                        let new_ctrl =
                            (runtime_ctrl & !MSIX_MESSAGE_CONTROL_MIRROR_MASK) | msix_ctrl_bits;
                        nvme.config_mut().write(ctrl_off, 2, u32::from(new_ctrl));
                    }
                }
            }
            disk_controllers.insert(bdf.pack_u16(), nvme.borrow().save_state());
        }
        if let Some(ide) = &self.ide {
            let bdf = aero_devices::pci::profile::IDE_PIIX3.bdf;
            if let Some(pci_cfg) = &self.pci_cfg {
                let (command, bar4_base) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar4_base = cfg.and_then(|cfg| cfg.bar_range(4)).map(|range| range.base);
                    (command, bar4_base)
                };
                let mut ide = ide.borrow_mut();
                ide.config_mut().set_command(command);
                if let Some(bar4_base) = bar4_base {
                    ide.config_mut().set_bar_base(4, bar4_base);
                }
            }
            disk_controllers.insert(bdf.pack_u16(), ide.borrow().save_state());
        }
        if let Some(virtio_blk) = &self.virtio_blk {
            let bdf = aero_devices::pci::profile::VIRTIO_BLK.bdf;
            if let Some(pci_cfg) = &self.pci_cfg {
                let (command, bar0_base, msix_ctrl_bits) = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let mut command = 0;
                    let mut bar0_base = None;
                    let mut msix_ctrl_bits = None;
                    if let Some(cfg) = pci_cfg.bus_mut().device_config_mut(bdf) {
                        command = cfg.command();
                        bar0_base = cfg.bar_range(0).map(|range| range.base);
                        if let Some(msix_off) = cfg.find_capability(PCI_CAP_ID_MSIX) {
                            let ctrl = cfg
                                .read(u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET, 2)
                                as u16;
                            msix_ctrl_bits = Some(ctrl & MSIX_MESSAGE_CONTROL_MIRROR_MASK);
                        }
                    }
                    (command, bar0_base, msix_ctrl_bits)
                };

                let mut virtio_blk = virtio_blk.borrow_mut();
                virtio_blk.set_pci_command(command);
                if let Some(bar0_base) = bar0_base {
                    virtio_blk.config_mut().set_bar_base(0, bar0_base);
                }
                if let Some(msix_ctrl_bits) = msix_ctrl_bits {
                    if let Some(msix_off) = virtio_blk.config_mut().find_capability(PCI_CAP_ID_MSIX)
                    {
                        let ctrl_off = u16::from(msix_off) + MSIX_MESSAGE_CONTROL_OFFSET;
                        let runtime_ctrl = virtio_blk.config_mut().read(ctrl_off, 2) as u16;
                        let new_ctrl =
                            (runtime_ctrl & !MSIX_MESSAGE_CONTROL_MIRROR_MASK) | msix_ctrl_bits;
                        virtio_blk
                            .config_mut()
                            .write(ctrl_off, 2, u32::from(new_ctrl));
                    }
                }
            }
            disk_controllers.insert(bdf.pack_u16(), virtio_blk.borrow().save_state());
        }
        if !disk_controllers.controllers().is_empty() {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::DISK_CONTROLLER,
                &disk_controllers,
            ));
        }
        if let Some(acpi_pm) = &self.acpi_pm {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::ACPI_PM,
                &*acpi_pm.borrow(),
            ));
        }
        if let Some(hpet) = &self.hpet {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::HPET,
                &*hpet.borrow(),
            ));
        }

        if let Some(ctrl) = &self.i8042 {
            let ctrl = ctrl.borrow();
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::I8042,
                &*ctrl,
            ));
        }

        // CPU_INTERNAL: non-architectural Tier-0 bookkeeping required for deterministic resume.
        let cpu_internal = snapshot::cpu_internal_state_from_cpu_core(&self.cpu);
        devices.push(
            cpu_internal
                .to_device_state()
                .expect("failed to encode CPU_INTERNAL CpuInternalState device state"),
        );
        devices
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        let mut disks = Vec::new();
        // Deterministic ordering (by stable disk_id); see `docs/16-snapshots.md`.
        //
        // Always emit entries for the canonical Win7 disks so `disk_id` mapping remains stable
        // even when the host has not populated overlay refs yet.
        disks.push(
            self.ahci_port0_overlay
                .clone()
                .unwrap_or(snapshot::DiskOverlayRef {
                    disk_id: Self::DISK_ID_PRIMARY_HDD,
                    base_image: String::new(),
                    overlay_image: String::new(),
                }),
        );
        disks.push(self.ide_secondary_master_atapi_overlay.clone().unwrap_or(
            snapshot::DiskOverlayRef {
                disk_id: Self::DISK_ID_INSTALL_MEDIA,
                base_image: String::new(),
                overlay_image: String::new(),
            },
        ));
        if let Some(disk) = self.ide_primary_master_overlay.clone() {
            disks.push(disk);
        }
        snapshot::DiskOverlayRefs { disks }
    }

    fn ram_len(&self) -> usize {
        usize::try_from(self.cfg.ram_size_bytes).unwrap_or(0)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        let low_ram_end = firmware::bios::PCIE_ECAM_BASE;
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if end > self.cfg.ram_size_bytes {
            return Err(snapshot::SnapshotError::Corrupt("ram read out of range"));
        }
        // Snapshots encode RAM as a dense byte array of length `ram_size_bytes` (not including any
        // guest-physical MMIO holes). When RAM is remapped above 4GiB to make room for the PCIe
        // ECAM/PCI hole, translate dense RAM offsets into the corresponding guest-physical
        // addresses.
        let ram = self.mem.bus.ram();

        let mut cur_offset = offset;
        let mut remaining = buf;
        while !remaining.is_empty() {
            let phys = if cur_offset < low_ram_end {
                cur_offset
            } else {
                FOUR_GIB + (cur_offset - low_ram_end)
            };

            let chunk_len = if cur_offset < low_ram_end {
                let until_boundary = low_ram_end - cur_offset;
                let until_boundary = usize::try_from(until_boundary).unwrap_or(remaining.len());
                remaining.len().min(until_boundary)
            } else {
                remaining.len()
            };

            ram.read_into(phys, &mut remaining[..chunk_len]).map_err(
                |_err: GuestMemoryError| snapshot::SnapshotError::Corrupt("ram read failed"),
            )?;
            cur_offset += chunk_len as u64;
            remaining = &mut remaining[chunk_len..];
        }
        Ok(())
    }

    fn dirty_page_size(&self) -> u32 {
        SNAPSHOT_DIRTY_PAGE_SIZE
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(self.mem.take_dirty_pages())
    }
}

impl snapshot::SnapshotTarget for Machine {
    fn pre_restore(&mut self) {
        // Clear restore-only state before applying any snapshot sections.
        //
        // `aero_snapshot` restore is section-order-independent, so this must not happen in a
        // per-section callback like `restore_cpu_state`.
        self.restored_disk_overlays = None;
        // Storage controller snapshots (IDE/ATAPI) intentionally drop attached host backends, so
        // any install media handle we currently hold is stale after restore and can interfere with
        // re-attaching the same ISO on OPFS (sync access handles are exclusive per file). Drop it
        // eagerly so restore leaves the machine in a "backends must be reattached" state.
        self.install_media = None;
        self.restore_error = None;
        // Reset host-side UHCI tick remainder before applying any snapshot sections. Newer
        // snapshots restore this field from `DeviceId::USB`; older snapshots will leave it at the
        // deterministic default (0).
        self.uhci_ns_remainder = 0;
        // Reset host-side EHCI tick remainder before applying any snapshot sections. Newer
        // snapshots restore this field from `DeviceId::USB`; older snapshots will leave it at the
        // deterministic default (0).
        self.ehci_ns_remainder = 0;
        self.xhci_ns_remainder = 0;
    }

    fn restore_meta(&mut self, meta: snapshot::SnapshotMeta) {
        self.last_snapshot_id = Some(meta.snapshot_id);
        self.next_snapshot_id = self
            .next_snapshot_id
            .max(meta.snapshot_id.saturating_add(1));
    }

    fn restore_cpu_state(&mut self, state: snapshot::CpuState) {
        snapshot::apply_cpu_state_to_cpu_core(&state, &mut self.cpu.state);
    }

    fn restore_cpu_states(&mut self, states: Vec<snapshot::VcpuSnapshot>) -> snapshot::Result<()> {
        let expected = self.cfg.cpu_count as usize;
        if states.len() != expected {
            return Err(snapshot::SnapshotError::Corrupt("CPU count mismatch"));
        }

        let mut seen = vec![false; expected];

        for state in states {
            let apic_id = usize::try_from(state.apic_id)
                .map_err(|_| snapshot::SnapshotError::Corrupt("unknown APIC ID"))?;
            if apic_id >= expected {
                return Err(snapshot::SnapshotError::Corrupt("unknown APIC ID"));
            }
            if seen[apic_id] {
                return Err(snapshot::SnapshotError::Corrupt("duplicate APIC ID"));
            }
            seen[apic_id] = true;

            if apic_id == 0 {
                snapshot::apply_cpu_state_to_cpu_core(&state.cpu, &mut self.cpu.state);
            } else {
                let idx = apic_id - 1;
                let Some(cpu) = self.ap_cpus.get_mut(idx) else {
                    return Err(snapshot::SnapshotError::Corrupt("unknown APIC ID"));
                };
                snapshot::apply_cpu_state_to_cpu_core(&state.cpu, &mut cpu.state);
            }
        }

        Ok(())
    }

    fn restore_mmu_state(&mut self, state: snapshot::MmuState) {
        snapshot::apply_mmu_state_to_cpu_core(&state, &mut self.cpu.state);
        self.cpu.time.set_tsc(self.cpu.state.msr.tsc);
    }

    fn restore_mmu_states(
        &mut self,
        states: Vec<snapshot::VcpuMmuSnapshot>,
    ) -> snapshot::Result<()> {
        let expected = self.cfg.cpu_count as usize;
        if states.len() != expected {
            return Err(snapshot::SnapshotError::Corrupt("MMU count mismatch"));
        }

        let mut seen = vec![false; expected];

        for state in states {
            let apic_id = usize::try_from(state.apic_id)
                .map_err(|_| snapshot::SnapshotError::Corrupt("unknown APIC ID"))?;
            if apic_id >= expected {
                return Err(snapshot::SnapshotError::Corrupt("unknown APIC ID"));
            }
            if seen[apic_id] {
                return Err(snapshot::SnapshotError::Corrupt("duplicate APIC ID"));
            }
            seen[apic_id] = true;

            if apic_id == 0 {
                snapshot::apply_mmu_state_to_cpu_core(&state.mmu, &mut self.cpu.state);
                self.cpu.time.set_tsc(self.cpu.state.msr.tsc);
            } else {
                let idx = apic_id - 1;
                let Some(cpu) = self.ap_cpus.get_mut(idx) else {
                    return Err(snapshot::SnapshotError::Corrupt("unknown APIC ID"));
                };
                snapshot::apply_mmu_state_to_cpu_core(&state.mmu, &mut cpu.state);
                cpu.time.set_tsc(cpu.state.msr.tsc);
            }
        }

        Ok(())
    }

    fn restore_device_states(&mut self, states: Vec<snapshot::DeviceState>) {
        use std::collections::HashMap;

        // Reset pending CPU bookkeeping to a deterministic baseline, so restores from older
        // snapshots (that lack `CPU_INTERNAL`) still clear stale pending state.
        self.cpu.pending = Default::default();
        // Clear any deferred restore error from a previous restore attempt.
        self.restore_error = None;
        // Reset host-side UHCI tick remainder so restores from older snapshots (that lack this
        // field) do not preserve stale partial-tick state from the pre-restore execution.
        self.uhci_ns_remainder = 0;
        self.ehci_ns_remainder = 0;
        self.xhci_ns_remainder = 0;

        // Track whether the snapshot includes an xHCI payload so we can deterministically reset the
        // controller when restoring older snapshots that only contain UHCI/EHCI state.
        //
        // This must *not* eagerly reset/replace the controller, because xHCI snapshot restore prefers
        // preserving existing host-attached device instances (e.g. HID handles) when possible.
        let mut saw_xhci_state_in_snapshot = false;

        // Restore ordering must be explicit and independent of snapshot file ordering so device
        // state is deterministic (especially for interrupt lines and PCI INTx routing).
        let mut by_id: HashMap<snapshot::DeviceId, snapshot::DeviceState> =
            HashMap::with_capacity(states.len());
        let mut disk_controller_states: Vec<snapshot::DeviceState> = Vec::new();
        for state in states {
            if state.id == snapshot::DeviceId::DISK_CONTROLLER {
                // `DeviceId::DISK_CONTROLLER` is a logical grouping that can contain multiple
                // io-snapshot devices (e.g. AHCI v1.0 and IDE v2.0). Preserve all entries so we can
                // restore each controller deterministically.
                disk_controller_states.push(state);
            } else {
                // Snapshot format already rejects duplicate (id, version, flags) tuples; for
                // multiple entries with the same outer ID (forward-compatible versions), prefer
                // the first one.
                by_id.entry(state.id).or_insert(state);
            }
        }
        disk_controller_states.sort_by_key(|s| (s.version, s.flags));

        let use_legacy_vga = self.cfg.enable_vga && !self.cfg.enable_aerogpu;

        // Firmware snapshot: required for deterministic BIOS interrupt behaviour.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::BIOS) {
            if state.version == 1 {
                if let Ok(mut snapshot) =
                    firmware::bios::BiosSnapshot::decode(&mut Cursor::new(&state.data))
                {
                    // `vbe_lfb_base` is a firmware configuration knob; treat the machine wiring as
                    // the source of truth when restoring.
                    let legacy_vga_lfb_base = self
                        .vga
                        .as_ref()
                        .map(|vga| vga.borrow().lfb_base())
                        .unwrap_or_else(|| self.legacy_vga_lfb_base());
                    snapshot.config.vbe_lfb_base = use_legacy_vga.then_some(legacy_vga_lfb_base);
                    self.bios.restore_snapshot(snapshot, &mut self.mem);
                }
            }
        }
        // Keep the BIOS VBE `TotalMemory` field coherent with whichever VRAM aperture the machine
        // configuration intends to expose, even if the snapshot did not include a BIOS section (or
        // it failed to decode).
        if self.cfg.enable_aerogpu {
            let blocks = aero_devices::pci::profile::AEROGPU_VRAM_SIZE.div_ceil(64 * 1024);
            self.bios.video.vbe.total_memory_64kb_blocks = blocks.min(u64::from(u16::MAX)) as u16;
        } else if use_legacy_vga {
            let vram_bytes = self
                .vga
                .as_ref()
                .map(|vga| vga.borrow().vram_size() as u64)
                .unwrap_or(aero_gpu_vga::DEFAULT_VRAM_SIZE as u64);
            let blocks = vram_bytes.div_ceil(64 * 1024);
            self.bios.video.vbe.total_memory_64kb_blocks = blocks.min(u64::from(u16::MAX)) as u16;
        }

        // Memory/chipset glue.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::MEMORY) {
            if state.version == 1 {
                let enabled = state.data.first().copied().unwrap_or(0) != 0;
                self.chipset.a20().set_enabled(enabled);
                self.cpu.state.a20_enabled = enabled;

                // Newer snapshots append the platform clock `now_ns` after the A20 byte. Restore it
                // *early* so time-based devices (RTC/HPET/AeroGPU vblank scheduling) observe the
                // correct time during their own `load_state()` calls.
                if let Some(clock) = &self.platform_clock {
                    if state.data.len() > 8 {
                        let mut buf = [0u8; 8];
                        buf.copy_from_slice(&state.data[1..9]);
                        clock.set_ns(u64::from_le_bytes(buf));
                    }
                }
            }
        }

        // Accumulated serial output.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::SERIAL) {
            if state.version == 1 {
                if let Some(uart) = &self.serial {
                    let _ = uart.borrow_mut().take_tx();
                }
                self.serial_log = state.data;
            }
        }

        // VGA/VBE.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::VGA) {
            // When VGA is disabled, ignore any VGA snapshot payloads.
            //
            // The machine's physical memory bus persists across `reset()` and does not support
            // unmapping MMIO regions. When VGA is disabled we map the canonical PCI MMIO window
            // (`PCI_MMIO_BASE..PCI_MMIO_END_EXCLUSIVE`), which overlaps the VGA MMIO LFB base.
            // Attempting to restore a VGA snapshot would therefore panic due to MMIO overlap.
            if !use_legacy_vga {
                // Treat this as a config mismatch (snapshot taken with VGA enabled, restored into a
                // headless machine).
                self.vga = None;
            } else {
                // Ensure a VGA device exists before restoring.
                let vga: Rc<RefCell<VgaDevice>> = match &self.vga {
                    Some(vga) => vga.clone(),
                    None => {
                        let vga = Rc::new(RefCell::new(VgaDevice::new_with_config(
                            self.legacy_vga_device_config(),
                        )));
                        self.vga = Some(vga.clone());

                        // Port mappings are part of machine wiring, not the snapshot payload, so
                        // install the default VGA port ranges now.
                        self.io.register_shared_range(
                            aero_gpu_vga::VGA_LEGACY_IO_START,
                            aero_gpu_vga::VGA_LEGACY_IO_LEN,
                            {
                                let vga = vga.clone();
                                move |_port| Box::new(VgaPortIoDevice { dev: vga.clone() })
                            },
                        );
                        self.io.register_shared_range(
                            aero_gpu_vga::VBE_DISPI_IO_START,
                            aero_gpu_vga::VBE_DISPI_IO_LEN,
                            {
                                let vga = vga.clone();
                                move |_port| Box::new(VgaPortIoDevice { dev: vga.clone() })
                            },
                        );

                        // MMIO mappings persist in the physical bus; install legacy + LFB.
                        let legacy_base = aero_gpu_vga::VGA_LEGACY_MEM_START as u64;
                        let legacy_len = aero_gpu_vga::VGA_LEGACY_MEM_LEN as u64;
                        self.mem.map_mmio_once(legacy_base, legacy_len, {
                            let vga = vga.clone();
                            move || {
                                Box::new(VgaLegacyMmioHandler {
                                    base_paddr: aero_gpu_vga::VGA_LEGACY_MEM_START,
                                    dev: vga,
                                })
                            }
                        });
                        let (lfb_base, lfb_len) = {
                            let vga = vga.borrow();
                            (u64::from(vga.lfb_base()), vga.vram_size() as u64)
                        };
                        self.mem.map_mmio_once(lfb_base, lfb_len, {
                            let vga = vga.clone();
                            move || Box::new(VgaLfbMmioHandler { dev: vga })
                        });

                        vga
                    }
                };

                // Prefer the io-snapshot (`VGAD`) encoding; fall back to the legacy `VgaSnapshotV1`
                // / `VgaSnapshotV2` payloads for backward compatibility.
                let io_result = {
                    let mut vga_mut = vga.borrow_mut();
                    snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *vga_mut)
                };
                if io_result.is_err() {
                    if state.version == aero_gpu_vga::VgaSnapshotV2::VERSION {
                        if let Ok(vga_snap) = aero_gpu_vga::VgaSnapshotV2::decode(&state.data) {
                            vga.borrow_mut().restore_snapshot_v2(&vga_snap);
                        }
                    } else if state.version == aero_gpu_vga::VgaSnapshotV1::VERSION {
                        if let Ok(vga_snap) = aero_gpu_vga::VgaSnapshotV1::decode(&state.data) {
                            vga.borrow_mut().restore_snapshot_v1(&vga_snap);
                        }
                    }
                }
            }
        }

        if let Some(state) = by_id.remove(&snapshot::DeviceId::AEROGPU) {
            // When AeroGPU is disabled, ignore any AeroGPU snapshot payloads (config mismatch),
            // similar to the VGA restore behaviour above.
            if !self.cfg.enable_aerogpu {
                self.aerogpu = None;
                self.aerogpu_mmio = None;
            } else if state.version == 1 {
                if let (Some(vram_dev), Some(bar0_dev)) = (&self.aerogpu, &self.aerogpu_mmio) {
                    if let Some(decoded) = decode_aerogpu_snapshot_v1(&state.data) {
                        {
                            let mut dev = vram_dev.borrow_mut();
                            // Reset unsnapshotted state (e.g. legacy VGA port latches) to a
                            // deterministic baseline before applying the snapshotted VRAM state.
                            dev.reset();
                            let copy_len = decoded.vram.len().min(dev.vram.len());
                            dev.vram[..copy_len].copy_from_slice(&decoded.vram[..copy_len]);

                            if let Some(dac) = &decoded.vga_dac {
                                dev.pel_mask = dac.pel_mask;
                                dev.dac_palette = dac.palette;
                            } else {
                                // Restore the VGA DAC palette from the BIOS VBE palette state. The BIOS
                                // snapshot captures VBE palette entries (B,G,R,0), and AeroGPU-backed
                                // 8bpp VBE rendering uses the emulated DAC palette.
                                let bits = self.bios.video.vbe.dac_width_bits;
                                let pal = &self.bios.video.vbe.palette;
                                dev.vga_port_write_u8(0x3C8, 0); // set DAC write index
                                for idx in 0..256usize {
                                    let base = idx * 4;
                                    let b = pal[base];
                                    let g = pal[base + 1];
                                    let r = pal[base + 2];

                                    let (r, g, b) = if bits >= 8 {
                                        (r >> 2, g >> 2, b >> 2)
                                    } else {
                                        (r & 0x3F, g & 0x3F, b & 0x3F)
                                    };

                                    // VGA DAC write order is R, G, B.
                                    dev.vga_port_write_u8(0x3C9, r);
                                    dev.vga_port_write_u8(0x3C9, g);
                                    dev.vga_port_write_u8(0x3C9, b);
                                }
                            }
                        }

                        {
                            let mut bar0 = bar0_dev.borrow_mut();
                            bar0.reset();
                            bar0.restore_snapshot_v1(&decoded.bar0);
                        }
                    }
                }
            } else if state.version == AEROGPU_SNAPSHOT_VERSION_V2 {
                if let (Some(vram_dev), Some(bar0_dev)) = (&self.aerogpu, &self.aerogpu_mmio) {
                    let mut vram = vram_dev.borrow_mut();
                    let mut bar0 = bar0_dev.borrow_mut();
                    let restored_dac = apply_aerogpu_snapshot_v2(&state.data, &mut vram, &mut bar0)
                        .unwrap_or(false);
                    drop(bar0);

                    // Backward-compatibility: v2 snapshots initially did not include VGA DAC state.
                    // Keep the AeroGPU-emulated DAC palette coherent with the BIOS VBE palette so
                    // 8bpp VBE output is deterministic even when restoring older snapshots.
                    if !restored_dac {
                        let bits = self.bios.video.vbe.dac_width_bits;
                        let pal = &self.bios.video.vbe.palette;
                        vram.vga_port_write_u8(0x3C8, 0); // set DAC write index
                        for idx in 0..256usize {
                            let base = idx * 4;
                            let b = pal[base];
                            let g = pal[base + 1];
                            let r = pal[base + 2];

                            let (r, g, b) = if bits >= 8 {
                                (r >> 2, g >> 2, b >> 2)
                            } else {
                                (r & 0x3F, g & 0x3F, b & 0x3F)
                            };

                            // VGA DAC write order is R, G, B.
                            vram.vga_port_write_u8(0x3C9, r);
                            vram.vga_port_write_u8(0x3C9, g);
                            vram.vga_port_write_u8(0x3C9, b);
                        }
                    }
                }
            }
        }

        // Optional PC platform devices.

        // 1) Restore interrupt controller complex first.
        //
        // Prefer the dedicated `PLATFORM_INTERRUPTS` id, but accept the historical `APIC` id for
        // backward compatibility with older snapshots.
        let mut restored_interrupts = false;
        // Prefer the dedicated `PLATFORM_INTERRUPTS` entry, but if it fails to apply (e.g. due to a
        // forward-incompatible version), fall back to the historical `APIC` entry when present.
        let interrupts_state = by_id.remove(&snapshot::DeviceId::PLATFORM_INTERRUPTS);
        let apic_state = by_id.remove(&snapshot::DeviceId::APIC);
        if let Some(interrupts) = &self.interrupts {
            let mut interrupts = interrupts.borrow_mut();
            if let Some(state) = interrupts_state {
                restored_interrupts = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                    &state,
                    &mut *interrupts,
                )
                .is_ok();
                if !restored_interrupts {
                    if let Some(state) = apic_state {
                        restored_interrupts =
                            snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &state,
                                &mut *interrupts,
                            )
                            .is_ok();
                    }
                }
            } else if let Some(state) = apic_state {
                restored_interrupts = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                    &state,
                    &mut *interrupts,
                )
                .is_ok();
            }
        }

        let mut restored_pci_intx = false;
        // 2) Restore PCI devices (config ports + INTx router).
        //
        // Canonical full-machine snapshots store these as separate outer device entries:
        // - `DeviceId::PCI_CFG` for `PciConfigPorts` (`PCPT`)
        // - `DeviceId::PCI_INTX_ROUTER` for `PciIntxRouter` (`INTX`)
        //
        // Backward compatibility: older snapshots stored one or both of these under the historical
        // `DeviceId::PCI` entry, either:
        // - as a combined `PciCoreSnapshot` wrapper (`PCIC`) containing both `PCPT` + `INTX`, or
        // - as a single `PCPT` (`PciConfigPorts`) payload, or
        // - as a single `INTX` (`PciIntxRouter`) payload.
        let pci_state = by_id.remove(&snapshot::DeviceId::PCI);
        let mut pci_cfg_state = by_id.remove(&snapshot::DeviceId::PCI_CFG);
        let mut pci_intx_state = by_id.remove(&snapshot::DeviceId::PCI_INTX_ROUTER);

        if let Some(state) = pci_state {
            if let (Some(pci_cfg), Some(pci_intx)) = (&self.pci_cfg, &self.pci_intx) {
                // Prefer decoding the combined PCI core wrapper (`PCIC`) first. If decoding fails,
                // treat `DeviceId::PCI` as the legacy `PCPT`/`INTX` payload.
                let core_result = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let mut pci_intx = pci_intx.borrow_mut();
                    let mut core = PciCoreSnapshot::new(&mut pci_cfg, &mut pci_intx);
                    snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut core)
                };

                match core_result {
                    Ok(()) => {
                        restored_pci_intx = true;
                        // If a dedicated `PCI_CFG` entry is also present, prefer it for config ports
                        // even if the combined core wrapper applied successfully.
                        if let Some(cfg_state) = pci_cfg_state.take() {
                            let mut cfg_ports = pci_cfg.borrow_mut();
                            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &cfg_state,
                                &mut *cfg_ports,
                            );
                        }
                        // If a dedicated `PCI_INTX_ROUTER` entry is also present, prefer it for INTx
                        // routing state even if the combined core wrapper applied successfully.
                        //
                        // This keeps restore behavior symmetric with `PCI_CFG` and allows snapshot
                        // coordinators to ship both legacy and split-out PCI entries (with the
                        // split-out entries taking precedence).
                        if let Some(intx_state) = pci_intx_state.take() {
                            let mut pci_intx = pci_intx.borrow_mut();
                            if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &intx_state,
                                &mut *pci_intx,
                            )
                            .is_ok()
                            {
                                restored_pci_intx = true;
                            }
                        }
                    }
                    Err(_) => {
                        // If a dedicated `PCI_CFG` entry is present, prefer it for config ports.
                        if let Some(cfg_state) = pci_cfg_state.take() {
                            let mut cfg_ports = pci_cfg.borrow_mut();
                            // Prefer split-out `PCI_CFG`, but fall back to the legacy `DeviceId::PCI`
                            // payload if the new entry fails to apply (e.g. unsupported future
                            // version).
                            if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &cfg_state,
                                &mut *cfg_ports,
                            )
                            .is_err()
                            {
                                let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                    &state,
                                    &mut *cfg_ports,
                                );
                            }
                        } else {
                            let mut cfg_ports = pci_cfg.borrow_mut();
                            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &state,
                                &mut *cfg_ports,
                            );
                        }

                        // Backward compatibility: some snapshots stored `PciIntxRouter` (`INTX`)
                        // directly under the historical `DeviceId::PCI`. However, if a dedicated
                        // `PCI_INTX_ROUTER` entry is present, prefer it.
                        if let Some(intx_state) = pci_intx_state.take() {
                            // Prefer the split-out `PCI_INTX_ROUTER` entry when present, but fall
                            // back to the legacy `DeviceId::PCI` payload if the new entry fails to
                            // apply (e.g. because it is from an unsupported future version).
                            let mut pci_intx = pci_intx.borrow_mut();
                            let restored =
                                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                    &intx_state,
                                    &mut *pci_intx,
                                )
                                .is_ok()
                                    || snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                        &state,
                                        &mut *pci_intx,
                                    )
                                    .is_ok();
                            if restored {
                                restored_pci_intx = true;
                            }
                        } else {
                            let mut pci_intx = pci_intx.borrow_mut();
                            if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &state,
                                &mut *pci_intx,
                            )
                            .is_ok()
                            {
                                restored_pci_intx = true;
                            }
                        }
                    }
                }
            } else if let Some(pci_cfg) = &self.pci_cfg {
                // Config ports only. Prefer the dedicated `PCI_CFG` entry if present.
                let mut cfg_ports = pci_cfg.borrow_mut();
                if let Some(cfg_state) = pci_cfg_state.take() {
                    if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                        &cfg_state,
                        &mut *cfg_ports,
                    )
                    .is_err()
                    {
                        let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                            &state,
                            &mut *cfg_ports,
                        );
                    }
                } else {
                    let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                        &state,
                        &mut *cfg_ports,
                    );
                }
            }
        } else {
            // No legacy PCI entry; restore config ports from the canonical `PCI_CFG` entry.
            if let (Some(pci_cfg), Some(cfg_state)) = (&self.pci_cfg, pci_cfg_state.take()) {
                let mut cfg_ports = pci_cfg.borrow_mut();
                let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                    &cfg_state,
                    &mut *cfg_ports,
                );
            }
        }

        // If we haven't restored the INTx router yet, fall back to a canonical/legacy
        // `PCI_INTX_ROUTER` entry.
        if !restored_pci_intx {
            if let (Some(pci_intx), Some(intx_state)) = (&self.pci_intx, pci_intx_state.take()) {
                let mut pci_intx = pci_intx.borrow_mut();
                restored_pci_intx = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                    &intx_state,
                    &mut *pci_intx,
                )
                .is_ok();
            }
        }

        // 3) After restoring both the interrupt controller and the PCI INTx router, re-drive any
        // asserted level-triggered GSIs into the interrupt sink.
        if restored_interrupts && restored_pci_intx {
            if let (Some(pci_intx), Some(interrupts)) = (&self.pci_intx, &self.interrupts) {
                let pci_intx = pci_intx.borrow();
                let mut interrupts = interrupts.borrow_mut();
                pci_intx.sync_levels_to_sink(&mut *interrupts);
            }
        }

        // 4) Restore storage controllers (AHCI + NVMe + IDE + virtio-blk). These must be restored after the interrupt
        // controller + PCI core so any restored interrupt state can be re-driven deterministically.
        for state in disk_controller_states {
            // Canonical encoding: `DeviceId::DISK_CONTROLLER` is a `DSKC` wrapper containing nested
            // controller snapshots keyed by packed PCI BDF. See `docs/16-snapshots.md`.
            if matches!(state.data.get(8..12), Some(id) if id == b"DSKC") {
                let mut wrapper = DiskControllersSnapshot::default();
                if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut wrapper)
                    .is_ok()
                {
                    for (&packed_bdf, nested) in wrapper.controllers() {
                        if packed_bdf == aero_devices::pci::profile::SATA_AHCI_ICH9.bdf.pack_u16() {
                            if let Some(ahci) = &self.ahci {
                                let _ = ahci.borrow_mut().load_state(nested);
                                // Storage controller snapshots intentionally drop attached host
                                // backends (e.g. `AtaDrive`), but `ahci_port0_auto_attach_shared_disk`
                                // is host configuration state and must be preserved so host calls
                                // like `set_disk_backend` can reattach the shared disk after
                                // restore.
                            }
                        } else if packed_bdf
                            == aero_devices::pci::profile::NVME_CONTROLLER.bdf.pack_u16()
                        {
                            if let Some(nvme) = &self.nvme {
                                let _ = nvme.borrow_mut().load_state(nested);
                            }
                        } else if packed_bdf == aero_devices::pci::profile::IDE_PIIX3.bdf.pack_u16()
                        {
                            if let Some(ide) = &self.ide {
                                let _ = ide.borrow_mut().load_state(nested);
                            }
                        } else if packed_bdf
                            == aero_devices::pci::profile::VIRTIO_BLK.bdf.pack_u16()
                        {
                            if let Some(virtio_blk) = &self.virtio_blk {
                                let _ = virtio_blk.borrow_mut().load_state(nested);
                            }
                        }
                    }
                }
                continue;
            }

            // Backward compatibility: some snapshots stored controllers directly under
            // `DeviceId::DISK_CONTROLLER` without the `DSKC` wrapper.
            let mut restored = false;
            if let Some(ahci) = &self.ahci {
                if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                    &state,
                    &mut *ahci.borrow_mut(),
                )
                .is_ok()
                {
                    restored = true;
                    // Storage controller snapshots intentionally drop attached host backends. Keep
                    // `ahci_port0_auto_attach_shared_disk` unchanged so host reattachment flows
                    // (e.g. `set_disk_backend`) can reattach the shared disk if auto-attach was
                    // enabled.
                }
            }
            if !restored {
                if let Some(nvme) = &self.nvme {
                    if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                        &state,
                        &mut *nvme.borrow_mut(),
                    )
                    .is_ok()
                    {
                        restored = true;
                    }
                }
            }
            if !restored {
                if let Some(virtio_blk) = &self.virtio_blk {
                    if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                        &state,
                        &mut *virtio_blk.borrow_mut(),
                    )
                    .is_ok()
                    {
                        restored = true;
                    }
                }
            }
            if !restored {
                if let Some(ide) = &self.ide {
                    let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                        &state,
                        &mut *ide.borrow_mut(),
                    );
                }
            }
        }

        // 5) Restore PIT + RTC + ACPI PM (these can drive IRQ lines during load_state()).
        if let (Some(pit), Some(state)) = (&self.pit, by_id.remove(&snapshot::DeviceId::PIT)) {
            let mut pit = pit.borrow_mut();
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *pit);
        }
        if let (Some(rtc), Some(state)) = (&self.rtc, by_id.remove(&snapshot::DeviceId::RTC)) {
            let mut rtc = rtc.borrow_mut();
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *rtc);
        }
        if let (Some(acpi_pm), Some(state)) =
            (&self.acpi_pm, by_id.remove(&snapshot::DeviceId::ACPI_PM))
        {
            let mut acpi_pm = acpi_pm.borrow_mut();
            let _ =
                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *acpi_pm);
        }

        // 6) Restore HPET.
        let mut restored_hpet = false;
        if let (Some(hpet), Some(state)) = (&self.hpet, by_id.remove(&snapshot::DeviceId::HPET)) {
            let mut hpet = hpet.borrow_mut();
            restored_hpet =
                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *hpet)
                    .is_ok();
        }

        // 7) After HPET restore, re-drive any pending level-triggered timer lines implied by the
        // restored interrupt status.
        if restored_hpet {
            if let (Some(hpet), Some(interrupts)) = (&self.hpet, &self.interrupts) {
                let mut hpet = hpet.borrow_mut();
                let mut interrupts = interrupts.borrow_mut();
                hpet.sync_levels_to_sink(&mut *interrupts);
            }
        }

        // Restore AeroGPU after the interrupt controller + PCI INTx router so any restored legacy
        // INTx level can be re-driven into the sink deterministically.
        if let (Some(aerogpu_mmio), Some(state)) = (
            &self.aerogpu_mmio,
            by_id.remove(&snapshot::DeviceId::AEROGPU),
        ) {
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                &state,
                &mut *aerogpu_mmio.borrow_mut(),
            );

            // Ensure the PCI INTx router's level for AeroGPU reflects the restored IRQ bits even if
            // the snapshot was taken before the machine polled/synced INTx sources.
            if let (Some(pci_intx), Some(interrupts)) = (&self.pci_intx, &self.interrupts) {
                let bdf = aero_devices::pci::profile::AEROGPU.bdf;
                let pin = PciInterruptPin::IntA;

                let command = self
                    .pci_cfg
                    .as_ref()
                    .and_then(|pci_cfg| {
                        let mut pci_cfg = pci_cfg.borrow_mut();
                        pci_cfg
                            .bus_mut()
                            .device_config(bdf)
                            .map(|cfg| cfg.command())
                    })
                    .unwrap_or(0);

                let mut level = aerogpu_mmio.borrow().irq_level();
                if (command & (1 << 10)) != 0 {
                    level = false;
                }

                let mut pci_intx = pci_intx.borrow_mut();
                let mut interrupts = interrupts.borrow_mut();
                pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
            }
        }

        // Restore E1000 after the interrupt controller + PCI INTx router so any restored
        // interrupt level can be re-driven into the sink immediately.
        if let (Some(e1000), Some(state)) = (&self.e1000, by_id.remove(&snapshot::DeviceId::E1000))
        {
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                &state,
                &mut *e1000.borrow_mut(),
            );
        }

        // Restore virtio-net after the interrupt controller + PCI INTx router so its restored
        // legacy INTx level can be re-driven into the sink deterministically.
        if let (Some(virtio), Some(state)) = (
            &self.virtio_net,
            by_id.remove(&snapshot::DeviceId::VIRTIO_NET),
        ) {
            let mut virtio = virtio.borrow_mut();

            // Virtio-net's RX queue pops available buffers and caches them internally (without
            // producing used entries) until a host frame arrives. Those cached buffers are
            // runtime-only and are not currently serialized in snapshot state. Clear them before
            // applying the transport snapshot, then rewind queue progress so the transport will
            // re-pop the guest-provided buffers post-restore.
            if let Some(net) = virtio.device_mut::<VirtioNet<VirtioNetBackendAdapter>>() {
                aero_virtio::devices::VirtioDevice::reset(net);
            } else if let Some(net) =
                virtio.device_mut::<VirtioNet<Option<Box<dyn NetworkBackend>>>>()
            {
                aero_virtio::devices::VirtioDevice::reset(net);
            }

            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *virtio);
            virtio.rewind_queue_next_avail_to_next_used(0);
        }

        // Restore virtio-input keyboard/mouse after the interrupt controller + PCI INTx router so
        // any restored legacy INTx level can be re-driven deterministically.
        //
        // Virtio-input can cache guest-provided event buffers internally without producing used
        // entries (waiting for host input events). Those cached descriptor chains are runtime-only
        // and are not currently serialized in snapshot state. Clear them before applying the
        // transport snapshot, then rewind the event queue progress so the transport will re-pop
        // the guest-provided buffers post-restore.
        let mut restored_virtio_input = false;
        if let (Some(virtio), Some(state)) = (
            &self.virtio_input_keyboard,
            by_id.remove(&snapshot::DeviceId::VIRTIO_INPUT_KEYBOARD),
        ) {
            let mut virtio = virtio.borrow_mut();
            if let Some(input) = virtio.device_mut::<VirtioInput>() {
                aero_virtio::devices::VirtioDevice::reset(input);
            }
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *virtio);
            virtio.rewind_queue_next_avail_to_next_used(0);
            restored_virtio_input = true;
        }
        if let (Some(virtio), Some(state)) = (
            &self.virtio_input_mouse,
            by_id.remove(&snapshot::DeviceId::VIRTIO_INPUT_MOUSE),
        ) {
            let mut virtio = virtio.borrow_mut();
            if let Some(input) = virtio.device_mut::<VirtioInput>() {
                aero_virtio::devices::VirtioDevice::reset(input);
            }
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *virtio);
            virtio.rewind_queue_next_avail_to_next_used(0);
            restored_virtio_input = true;
        }

        // Backward compatibility: older snapshots stored both virtio-input PCI functions under the
        // single wrapper id `DeviceId::VIRTIO_INPUT` (inner snapshot 4CC `VINP`).
        if !restored_virtio_input {
            if let Some(state) = by_id.remove(&snapshot::DeviceId::VIRTIO_INPUT) {
                if matches!(state.data.get(8..12), Some(id) if id == b"VINP") {
                    let mut wrapper = MachineVirtioInputSnapshot::default();
                    if wrapper.load_state(&state.data).is_ok() {
                        if let (Some(kbd), Some(kbd_state)) =
                            (&self.virtio_input_keyboard, wrapper.keyboard.as_deref())
                        {
                            let mut dev = kbd.borrow_mut();
                            if let Some(input) = dev.device_mut::<VirtioInput>() {
                                aero_virtio::devices::VirtioDevice::reset(input);
                            }
                            let _ = dev.load_state(kbd_state);
                            dev.rewind_queue_next_avail_to_next_used(0);
                        }

                        if let (Some(mouse), Some(mouse_state)) =
                            (&self.virtio_input_mouse, wrapper.mouse.as_deref())
                        {
                            let mut dev = mouse.borrow_mut();
                            if let Some(input) = dev.device_mut::<VirtioInput>() {
                                aero_virtio::devices::VirtioDevice::reset(input);
                            }
                            let _ = dev.load_state(mouse_state);
                            dev.rewind_queue_next_avail_to_next_used(0);
                        }
                    }
                }
            }
        }

        // Restore USB controller state after the interrupt controller + PCI core so its IRQ level
        // can be re-driven deterministically.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::USB) {
            // If synthetic USB HID devices are enabled, pre-attach the canonical external hub +
            // synthetic devices *before* loading the UHCI snapshot so `RootHub::load_snapshot_ports`
            // will reuse the existing device instances (handle stability).
            self.ensure_uhci_synthetic_usb_hid_topology();

            const NS_PER_MS: u64 = 1_000_000;
            let data = state.data;

            // Canonical encoding: `DeviceId::USB` stores a `USBC` wrapper that nests guest-visible
            // controller snapshots (UHCI/EHCI/xHCI) plus the machine's host-side sub-ms tick
            // remainders.
            //
            // Note: older snapshots stored a single controller PCI device snapshot (`UHCP` / `EHCP`
            // / `XHCP`) directly under `DeviceId::USB` and did not include sub-ms tick remainder
            // state. Default to 0 unless we successfully decode a `USBC` wrapper containing
            // remainder values.
            self.uhci_ns_remainder = 0;
            self.ehci_ns_remainder = 0;
            self.xhci_ns_remainder = 0;
            let inner_id = data.get(8..12).and_then(|id| <[u8; 4]>::try_from(id).ok());
            if inner_id == Some(*b"USBC") {
                let mut wrapper = MachineUsbSnapshot::default();
                if wrapper.load_state(&data).is_ok() {
                    saw_xhci_state_in_snapshot = wrapper.xhci.is_some();

                    if let Some(uhci_state) = wrapper.uhci.as_deref() {
                        if let Some(uhci) = &self.uhci {
                            self.uhci_ns_remainder = wrapper.uhci_ns_remainder % NS_PER_MS;
                            let _ = uhci.borrow_mut().load_state(uhci_state);
                        } else {
                            #[cfg(not(target_arch = "wasm32"))]
                            eprintln!(
                                "warning: snapshot contains UHCI state in USBC wrapper but machine has UHCI disabled; ignoring"
                            );
                        }
                    }

                    if let Some(ehci_state) = wrapper.ehci.as_deref() {
                        if let Some(ehci) = &self.ehci {
                            self.ehci_ns_remainder = wrapper.ehci_ns_remainder % NS_PER_MS;
                            let _ = ehci.borrow_mut().load_state(ehci_state);
                        } else {
                            #[cfg(not(target_arch = "wasm32"))]
                            eprintln!(
                                "warning: snapshot contains EHCI state in USBC wrapper but machine has EHCI disabled; ignoring"
                            );
                        }
                    }
                    if let Some(xhci_bytes) = wrapper.xhci.as_deref() {
                        match &self.xhci {
                            Some(xhci) => {
                                self.xhci_ns_remainder = wrapper.xhci_ns_remainder % NS_PER_MS;
                                let _ = xhci.borrow_mut().load_state(xhci_bytes);
                            }
                            None => {
                                // SnapshotTarget::restore_device_states cannot return a `Result`,
                                // so defer this config mismatch as a post-restore error.
                                self.restore_error = Some(snapshot::SnapshotError::Corrupt(
                                    "snapshot contains xHCI state but enable_xhci is false",
                                ));
                            }
                        }
                    }
                }
            } else {
                // Backward compatibility: older snapshots stored a single USB controller PCI device
                // snapshot (`UHCP` / `EHCP` / `XHCP`) directly under `DeviceId::USB`, without a
                // `USBC` wrapper. Best-effort apply the blob to any matching controller present in
                // the target machine.
                match inner_id.as_ref().map(|id| &id[..]) {
                    Some(b"UHCP") => {
                        if let Some(uhci) = &self.uhci {
                            let _ = uhci.borrow_mut().load_state(&data);
                        } else {
                            #[cfg(not(target_arch = "wasm32"))]
                            eprintln!(
                                "warning: snapshot contains legacy UHCI USB payload but machine has UHCI disabled; ignoring"
                            );
                        }
                    }
                    Some(b"UHCI") => {
                        if let Some(uhci) = &self.uhci {
                            let mut uhci = uhci.borrow_mut();
                            if uhci.controller_mut().load_state(&data).is_ok() {
                                uhci.controller_mut().reset_host_state_for_restore();
                            }
                        } else {
                            #[cfg(not(target_arch = "wasm32"))]
                            eprintln!(
                                "warning: snapshot contains legacy UHCI USB payload but machine has UHCI disabled; ignoring"
                            );
                        }
                    }
                    Some(b"EHCP") => {
                        if let Some(ehci) = &self.ehci {
                            let _ = ehci.borrow_mut().load_state(&data);
                        } else {
                            #[cfg(not(target_arch = "wasm32"))]
                            eprintln!(
                                "warning: snapshot contains legacy EHCI USB payload but machine has EHCI disabled; ignoring"
                            );
                        }
                    }
                    Some(b"EHCI") => {
                        if let Some(ehci) = &self.ehci {
                            let mut ehci = ehci.borrow_mut();
                            if ehci.controller_mut().load_state(&data).is_ok() {
                                ehci.controller_mut().reset_host_state_for_restore();
                            }
                        } else {
                            #[cfg(not(target_arch = "wasm32"))]
                            eprintln!(
                                "warning: snapshot contains legacy EHCI USB payload but machine has EHCI disabled; ignoring"
                            );
                        }
                    }
                    Some(b"XHCP") => {
                        saw_xhci_state_in_snapshot = true;
                        match &self.xhci {
                            Some(xhci) => {
                                let _ = xhci.borrow_mut().load_state(&data);
                            }
                            None => {
                                // SnapshotTarget::restore_device_states cannot return a `Result`,
                                // so defer this config mismatch as a post-restore error.
                                self.restore_error = Some(snapshot::SnapshotError::Corrupt(
                                    "snapshot contains xHCI state but enable_xhci is false",
                                ));
                            }
                        }
                    }
                    Some(b"XHCI") => {
                        saw_xhci_state_in_snapshot = true;
                        match &self.xhci {
                            Some(xhci) => {
                                // The xHCI PCI wrapper stores additional host-side interrupt edge
                                // state (`last_irq_level`). Reuse the wrapper's `load_state` logic
                                // by embedding the controller TLV blob in a minimal `XHCP`
                                // snapshot.
                                const TAG_XHCI_CONTROLLER: u16 = 4;
                                let version = <XhciPciDevice as IoSnapshot>::DEVICE_VERSION;
                                let mut w = SnapshotWriter::new(*b"XHCP", version);
                                w.field_bytes(TAG_XHCI_CONTROLLER, data);
                                let bytes = w.finish();
                                let _ = xhci.borrow_mut().load_state(&bytes);
                            }
                            None => {
                                // SnapshotTarget::restore_device_states cannot return a `Result`,
                                // so defer this config mismatch as a post-restore error.
                                self.restore_error = Some(snapshot::SnapshotError::Corrupt(
                                    "snapshot contains xHCI state but enable_xhci is false",
                                ));
                            }
                        }
                    }
                    _ => {
                        if let Some(uhci) = &self.uhci {
                            let _ = uhci.borrow_mut().load_state(&data);
                        }
                        if let Some(ehci) = &self.ehci {
                            let _ = ehci.borrow_mut().load_state(&data);
                        }
                        if let Some(xhci) = &self.xhci {
                            let _ = xhci.borrow_mut().load_state(&data);
                        }
                    }
                }
            }
        }

        // If the machine has xHCI enabled but the snapshot did not include any xHCI payload, reset
        // the controller to a deterministic baseline (while preserving any host-attached device
        // instances and wiring).
        if !saw_xhci_state_in_snapshot {
            if let Some(xhci) = &self.xhci {
                xhci.borrow_mut().reset();
            }
        }

        // Restore i8042 after the interrupt controller complex so any restored IRQ pulses are
        // delivered into the correct sink state.
        if let (Some(ctrl), Some(state)) = (&self.i8042, by_id.remove(&snapshot::DeviceId::I8042)) {
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                &state,
                &mut *ctrl.borrow_mut(),
            );
        }

        // Re-drive PCI INTx levels derived from restored device state (e.g. E1000). This is
        // required because `IoSnapshot::load_state()` cannot access the interrupt sink directly,
        // and some device models surface their INTx level via polling rather than storing it in
        // the router snapshot.
        self.sync_pci_intx_sources_to_interrupts();

        // Adopt any restored baseline GSI levels once all devices have had a chance to re-drive
        // their own interrupt outputs.
        if let Some(interrupts) = &self.interrupts {
            interrupts.borrow_mut().finalize_restore();
        }

        // CPU_INTERNAL: machine-defined CPU bookkeeping (interrupt shadow + external interrupt FIFO).
        if let Some(state) = by_id.remove(&snapshot::DeviceId::CPU_INTERNAL) {
            if let Ok(decoded) = snapshot::CpuInternalState::from_device_state(&state) {
                snapshot::apply_cpu_internal_state_to_cpu_core(&decoded, &mut self.cpu);
            }
        }

        // Ensure the BIOS VBE LFB base matches the machine's active display wiring (VGA configured
        // base, AeroGPU BAR1-derived base, or the default RAM-backed base for headless configs).
        self.sync_bios_vbe_lfb_base_to_display_wiring();
    }

    fn restore_disk_overlays(&mut self, mut overlays: snapshot::DiskOverlayRefs) {
        // Preserve a stable ordering for host integrations regardless of snapshot file ordering.
        overlays.disks.sort_by_key(|disk| disk.disk_id);

        // Precompute the machine's per-disk overlay refs before we move `overlays` into
        // `restored_disk_overlays`. This avoids cloning the full `DiskOverlayRefs` payload (which
        // can include many entries) just to retain a copy for the host.
        let ahci_port0_overlay = overlays
            .disks
            .iter()
            .find(|d| d.disk_id == Self::DISK_ID_PRIMARY_HDD)
            .cloned();
        let ide_secondary_master_atapi_overlay = overlays
            .disks
            .iter()
            .find(|d| d.disk_id == Self::DISK_ID_INSTALL_MEDIA)
            .cloned();
        let ide_primary_master_overlay = overlays
            .disks
            .iter()
            .find(|d| d.disk_id == Self::DISK_ID_IDE_PRIMARY_MASTER)
            .cloned();

        // Record the restored refs for the host/coordinator so it can re-open and re-attach the
        // appropriate storage backends after restore.
        self.restored_disk_overlays = Some(overlays);

        // Also update the machine's configured overlay refs so subsequent snapshots (and host-side
        // queries) reflect the restored configuration.
        self.ahci_port0_overlay = ahci_port0_overlay;
        self.ide_secondary_master_atapi_overlay = ide_secondary_master_atapi_overlay;
        self.ide_primary_master_overlay = ide_primary_master_overlay;
    }

    fn ram_len(&self) -> usize {
        usize::try_from(self.cfg.ram_size_bytes).unwrap_or(0)
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> snapshot::Result<()> {
        let low_ram_end = firmware::bios::PCIE_ECAM_BASE;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if end > self.cfg.ram_size_bytes {
            return Err(snapshot::SnapshotError::Corrupt("ram write out of range"));
        }
        let ram = self.mem.bus.ram_mut();

        let mut cur_offset = offset;
        let mut remaining = data;
        while !remaining.is_empty() {
            let phys = if cur_offset < low_ram_end {
                cur_offset
            } else {
                FOUR_GIB + (cur_offset - low_ram_end)
            };

            let chunk_len = if cur_offset < low_ram_end {
                let until_boundary = low_ram_end - cur_offset;
                let until_boundary = usize::try_from(until_boundary).unwrap_or(remaining.len());
                remaining.len().min(until_boundary)
            } else {
                remaining.len()
            };

            ram.write_from(phys, &remaining[..chunk_len])
                .map_err(|_err: GuestMemoryError| {
                    snapshot::SnapshotError::Corrupt("ram write failed")
                })?;
            cur_offset += chunk_len as u64;
            remaining = &remaining[chunk_len..];
        }
        Ok(())
    }

    fn post_restore(&mut self) -> snapshot::Result<()> {
        // Network backends are external host state (e.g. live proxy connections) and are not part
        // of the snapshot format. Ensure we always drop any previously attached backend after
        // restoring, even if the caller bypasses the `Machine::restore_snapshot_*` helper methods
        // and drives snapshot restore directly via `aero_snapshot::restore_snapshot`.
        self.detach_network();
        // `inject_ps2_mouse_buttons` maintains a host-side "previous buttons" cache to synthesize
        // per-button transitions from an absolute mask. Snapshot restore rewinds guest time and
        // restores guest device state; invalidate the cache so the next injection call can re-sync
        // correctly even if the guest mouse state differs from the cached host value.
        self.ps2_mouse_buttons = 0xFF;
        // `consumer_usage_backend` tracks which input backend (virtio vs synthetic USB
        // consumer-control) delivered the press for each consumer usage. Snapshot restore can be
        // applied into a new machine instance that lacks this host-side pairing state, so drop any
        // cached mapping after restore to avoid misrouting subsequent events.
        self.consumer_usage_backend.fill(0);
        // Snapshot restore can rewind input device state (including held keys) without capturing
        // host-side pressed-key tracking used for `inject_input_batch` backend switching. Drop the
        // cached pressed set so the next batch can re-sync based on new host input.
        self.input_batch_pressed_keyboard_usages.fill(0);
        self.input_batch_pressed_keyboard_usage_count = 0;
        self.input_batch_keyboard_backend = 0;
        self.input_batch_mouse_buttons_mask = 0;
        self.input_batch_mouse_backend = 0;
        self.reset_latch.clear();
        self.assist = AssistContext::default();
        self.display_fb.clear();
        self.display_width = 0;
        self.display_height = 0;
        // Snapshots restore RAM and paging control registers, but do not capture the MMU's internal
        // translation cache (TLB). Since `Machine` keeps a persistent MMU to warm the TLB across
        // batches, reset it here so restored execution never uses stale translations.
        self.mmu = aero_mmu::Mmu::new();
        self.cpu.state.sync_mmu(&mut self.mmu);
        self.mem.clear_dirty();
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        self.resync_guest_time_from_tsc();

        // Snapshot restore applies `DEVICES` before `RAM`, so any cursor sync that reads from the
        // BIOS Data Area must happen *after* RAM is restored (here in `post_restore`).
        //
        // Limit this to real/v8086 mode where the HLE BIOS/BDA contract is expected to be the
        // authoritative cursor source; once an OS takes over (protected/long mode), software may
        // update VGA registers directly without keeping the BDA coherent.
        if matches!(self.cpu.state.mode, CpuMode::Real | CpuMode::Vm86)
            && self.bios.video.vbe.current_mode.is_none()
        {
            self.sync_text_mode_cursor_bda_to_vga_crtc();
        }

        if let Some(err) = self.restore_error.take() {
            return Err(err);
        }
        Ok(())
    }
}

impl memory::MemoryBus for Machine {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.mem.read_physical(paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.mem.write_physical(paddr, buf);
    }
}

#[cfg(test)]
mod test_util;

#[cfg(test)]
mod virtio_intx_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::capture_panic_location;
    use aero_cpu_core::state::{gpr, CR0_PE, CR0_PG};
    use aero_devices::pci::PciInterruptPin;
    use pretty_assertions::assert_eq;
    use std::io::{Cursor, Read};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn build_serial_boot_sector(message: &[u8]) -> [u8; 512] {
        let mut sector = [0u8; 512];
        let mut i = 0usize;

        // mov dx, 0x3f8
        sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;

        for &b in message {
            // mov al, imm8
            sector[i..i + 2].copy_from_slice(&[0xB0, b]);
            i += 2;
            // out dx, al
            sector[i] = 0xEE;
            i += 1;
        }

        // hlt
        sector[i] = 0xF4;

        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    #[test]
    fn map_mmio_once_panics_at_call_site_on_overflow() {
        struct DummyMmio;
        impl memory::MmioHandler for DummyMmio {
            fn read(&mut self, _offset: u64, _size: usize) -> u64 {
                0
            }

            fn write(&mut self, _offset: u64, _size: usize, _value: u64) {}
        }

        let chipset = ChipsetState::new(true);
        let a20 = chipset.a20();
        let mut mem = SystemMemory::new(0, a20).expect("construct SystemMemory");

        let expected_file = file!();
        let expected_line = line!() + 2;
        let (file, line) = capture_panic_location(|| {
            mem.map_mmio_once(u64::MAX, 2, || Box::new(DummyMmio));
        });
        assert_eq!(file, expected_file);
        assert_eq!(line, expected_line);
    }

    #[test]
    fn aerogpu_legacy_vga_mmio_byte_iteration_does_not_wrap_u64_offsets() {
        // Regression test: `legacy_vga_read`/`legacy_vga_write` iterate bytewise for 1/2/4/8-byte
        // MMIO accesses. Those offsets are guest-controlled (via the MMIO router), so they must not
        // wrap around u64 space.
        //
        // Before the fix, an `offset = u64::MAX, size = 2` access would use `wrapping_add` and
        // accidentally touch `offset = 0` for the second byte.
        let mut dev = new_minimal_aerogpu_device_for_snapshot_tests();
        dev.vram = vec![0; 1];
        dev.vram[0] = 0xAA;

        assert_eq!(dev.legacy_vga_read(u64::MAX, 2), 0);

        dev.legacy_vga_write(u64::MAX, 2, 0xBBAA);
        assert_eq!(dev.vram[0], 0xAA);
    }

    #[test]
    fn ap_cpus_split_get_mut_excludes_current_index() {
        let mut cpus = [
            CpuCore::new(CpuMode::Real),
            CpuCore::new(CpuMode::Real),
            CpuCore::new(CpuMode::Real),
        ];

        // Exclude the middle AP (idx=1).
        {
            let (before, rest) = cpus.split_at_mut(1);
            let after = {
                let (_current, after) = rest.split_first_mut().expect("split_first_mut");
                after
            };
            let mut ap_cpus = ApCpus::Split { before, after };

            ap_cpus
                .get_mut(0)
                .expect("idx 0 should be accessible")
                .state
                .halted = true;
            assert!(
                ap_cpus.get_mut(1).is_none(),
                "current AP index must not be accessible via ApCpus::Split"
            );
            ap_cpus
                .get_mut(2)
                .expect("idx 2 should be accessible")
                .state
                .halted = true;
        }

        assert!(cpus[0].state.halted);
        assert!(!cpus[1].state.halted);
        assert!(cpus[2].state.halted);
    }

    #[test]
    fn ap_cpus_split_for_each_mut_excluding_current_uses_full_indices() {
        let mut cpus = [
            CpuCore::new(CpuMode::Real),
            CpuCore::new(CpuMode::Real),
            CpuCore::new(CpuMode::Real),
        ];

        // Exclude the middle AP (idx=1) and ensure callbacks see the original indices (0 and 2).
        let mut seen = Vec::new();
        {
            let (before, rest) = cpus.split_at_mut(1);
            let after = {
                let (_current, after) = rest.split_first_mut().expect("split_first_mut");
                after
            };
            let mut ap_cpus = ApCpus::Split { before, after };
            ap_cpus.for_each_mut_excluding_current(|idx, _cpu| seen.push(idx));
        }
        assert_eq!(seen, vec![0, 2]);
    }

    #[test]
    fn ap_cpus_split_for_each_mut_excluding_index_skips_requested_index() {
        let mut cpus = [
            CpuCore::new(CpuMode::Real),
            CpuCore::new(CpuMode::Real),
            CpuCore::new(CpuMode::Real),
        ];

        // Exclude the middle AP (idx=1) and then exclude the first AP (idx=0). We should be left
        // with only the last AP (idx=2).
        let mut seen = Vec::new();
        {
            let (before, rest) = cpus.split_at_mut(1);
            let after = {
                let (_current, after) = rest.split_first_mut().expect("split_first_mut");
                after
            };
            let mut ap_cpus = ApCpus::Split { before, after };
            ap_cpus.for_each_mut_excluding_index(0, |idx, _cpu| seen.push(idx));
        }
        assert_eq!(seen, vec![2]);
    }

    #[test]
    fn usb_snapshot_container_decodes_legacy_uhci_only_payload_and_defaults_ehci_fields() {
        let expected_uhci = vec![0x12, 0x34, 0x56];
        let expected_remainder = 42u64;

        // `USBC` v1.0 only contained UHCI fields. Ensure the v1.2 decoder continues accepting it
        // and defaults EHCI/xHCI to empty/none.
        let mut w = SnapshotWriter::new(*b"USBC", SnapshotVersion::new(1, 0));
        w.field_u64(
            MachineUsbSnapshot::TAG_UHCI_NS_REMAINDER,
            expected_remainder,
        );
        w.field_bytes(MachineUsbSnapshot::TAG_UHCI_STATE, expected_uhci.clone());
        let bytes = w.finish();

        let mut decoded = MachineUsbSnapshot::default();
        decoded.load_state(&bytes).expect("load legacy USBC v1.0");

        assert_eq!(decoded.uhci.as_deref(), Some(expected_uhci.as_slice()));
        assert_eq!(decoded.uhci_ns_remainder, expected_remainder);
        assert_eq!(decoded.ehci, None);
        assert_eq!(decoded.ehci_ns_remainder, 0);
        assert_eq!(decoded.xhci, None);
        assert_eq!(decoded.xhci_ns_remainder, 0);
    }

    #[test]
    fn usb_snapshot_container_decodes_legacy_uhci_and_ehci_payload_and_defaults_xhci_fields() {
        let expected_uhci = vec![0x12, 0x34, 0x56];
        let expected_uhci_remainder = 42u64;
        let expected_ehci = vec![0x99, 0x88];
        let expected_ehci_remainder = 7u64;

        // `USBC` v1.1 added optional EHCI fields. Ensure the v1.2 decoder continues accepting it
        // and defaults xHCI to empty/none.
        let mut w = SnapshotWriter::new(*b"USBC", SnapshotVersion::new(1, 1));
        w.field_u64(
            MachineUsbSnapshot::TAG_UHCI_NS_REMAINDER,
            expected_uhci_remainder,
        );
        w.field_bytes(MachineUsbSnapshot::TAG_UHCI_STATE, expected_uhci.clone());
        w.field_u64(
            MachineUsbSnapshot::TAG_EHCI_NS_REMAINDER,
            expected_ehci_remainder,
        );
        w.field_bytes(MachineUsbSnapshot::TAG_EHCI_STATE, expected_ehci.clone());
        let bytes = w.finish();

        let mut decoded = MachineUsbSnapshot::default();
        decoded.load_state(&bytes).expect("load legacy USBC v1.1");

        assert_eq!(decoded.uhci.as_deref(), Some(expected_uhci.as_slice()));
        assert_eq!(decoded.uhci_ns_remainder, expected_uhci_remainder);
        assert_eq!(decoded.ehci.as_deref(), Some(expected_ehci.as_slice()));
        assert_eq!(decoded.ehci_ns_remainder, expected_ehci_remainder);
        assert_eq!(decoded.xhci, None);
        assert_eq!(decoded.xhci_ns_remainder, 0);
    }

    #[test]
    fn usb_snapshot_container_roundtrips_uhci_xhci_and_ehci_state() {
        let snapshot = MachineUsbSnapshot {
            uhci: Some(vec![1, 2, 3]),
            uhci_ns_remainder: 500_123,
            ehci: Some(vec![4, 5, 6, 7]),
            ehci_ns_remainder: 250_456,
            xhci: Some(vec![0xaa, 0xbb]),
            xhci_ns_remainder: 750_789,
        };

        let bytes = snapshot.save_state();

        let mut decoded = MachineUsbSnapshot::default();
        decoded.load_state(&bytes).expect("load USBC v1.2");

        assert_eq!(decoded, snapshot);

        // Ensure we can re-encode deterministically after restore.
        assert_eq!(decoded.save_state(), bytes);
    }

    #[test]
    fn usb_snapshot_container_roundtrips_uhci_and_ehci_state_without_xhci() {
        let snapshot = MachineUsbSnapshot {
            uhci: Some(vec![1, 2, 3]),
            uhci_ns_remainder: 500_123,
            ehci: Some(vec![4, 5, 6, 7]),
            ehci_ns_remainder: 250_456,
            xhci: None,
            xhci_ns_remainder: 0,
        };

        let bytes = snapshot.save_state();

        let mut decoded = MachineUsbSnapshot::default();
        decoded.load_state(&bytes).expect("load USBC v1.2");

        assert_eq!(decoded, snapshot);

        // Ensure we can re-encode deterministically after restore.
        assert_eq!(decoded.save_state(), bytes);
    }

    #[test]
    fn usb_snapshot_container_omits_xhci_tags_when_state_absent() {
        let snapshot = MachineUsbSnapshot {
            uhci: Some(vec![1, 2, 3]),
            uhci_ns_remainder: 1,
            ehci: Some(vec![4, 5, 6, 7]),
            ehci_ns_remainder: 2,
            xhci: None,
            // Even if the field is non-zero, USBC should omit xHCI fields when no xHCI snapshot is
            // present.
            xhci_ns_remainder: 123_456,
        };

        let bytes = snapshot.save_state();
        let r = IoSnapshotReader::parse(&bytes, *b"USBC").expect("parse USBC v1.2");
        assert!(
            r.u64(MachineUsbSnapshot::TAG_XHCI_NS_REMAINDER)
                .expect("read xHCI remainder tag")
                .is_none(),
            "xHCI remainder tag should be absent when xHCI state is absent"
        );
        assert!(
            r.bytes(MachineUsbSnapshot::TAG_XHCI_STATE).is_none(),
            "xHCI state tag should be absent when xHCI state is absent"
        );
    }

    #[test]
    fn vga_lfb_mmio_size0_is_noop() {
        let vga = Rc::new(RefCell::new(VgaDevice::new()));
        vga.borrow_mut().vram_mut()[0] = 0xAA;

        let mut mmio = VgaLfbMmioHandler { dev: vga.clone() };

        assert_eq!(MmioHandler::read(&mut mmio, 0, 0), 0);
        MmioHandler::write(&mut mmio, 0, 0, 0x55);

        assert_eq!(vga.borrow().vram()[0], 0xAA);
    }

    #[test]
    fn vga_legacy_mmio_size0_is_noop() {
        let vga = Rc::new(RefCell::new(VgaDevice::new()));
        vga.borrow_mut().vram_mut()[0] = 0xAA;

        let mut mmio = VgaLegacyMmioHandler {
            base_paddr: aero_gpu_vga::VGA_LEGACY_MEM_START,
            dev: vga.clone(),
        };

        assert_eq!(MmioHandler::read(&mut mmio, 0, 0), 0);
        MmioHandler::write(&mut mmio, 0, 0, 0x55);

        assert_eq!(vga.borrow().vram()[0], 0xAA);
    }

    #[test]
    fn virtio_pci_bar0_mmio_size0_is_noop() {
        struct DummyVirtioDevice;

        impl aero_virtio::devices::VirtioDevice for DummyVirtioDevice {
            fn device_type(&self) -> u16 {
                1
            }

            fn device_features(&self) -> u64 {
                0
            }

            fn set_features(&mut self, _features: u64) {}

            fn num_queues(&self) -> u16 {
                0
            }

            fn queue_max_size(&self, _queue: u16) -> u16 {
                0
            }

            fn process_queue(
                &mut self,
                _queue_index: u16,
                _chain: aero_virtio::queue::DescriptorChain,
                _queue: &mut aero_virtio::queue::VirtQueue,
                _mem: &mut dyn aero_virtio::memory::GuestMemory,
            ) -> Result<bool, aero_virtio::devices::VirtioDeviceError> {
                Ok(false)
            }

            fn poll_queue(
                &mut self,
                _queue_index: u16,
                _queue: &mut aero_virtio::queue::VirtQueue,
                _mem: &mut dyn aero_virtio::memory::GuestMemory,
            ) -> Result<bool, aero_virtio::devices::VirtioDeviceError> {
                Ok(false)
            }

            fn read_config(&self, _offset: u64, data: &mut [u8]) {
                data.fill(0);
            }

            fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

            fn reset(&mut self) {}

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }

        let dev = Rc::new(RefCell::new(VirtioPciDevice::new(
            Box::new(DummyVirtioDevice),
            Box::new(NoopVirtioInterruptSink),
        )));

        let pci_cfg: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::new()));
        let mut mmio =
            VirtioPciBar0Mmio::new(pci_cfg, dev, aero_devices::pci::profile::VIRTIO_NET.bdf);

        assert_eq!(PciBarMmioHandler::read(&mut mmio, 0, 0), 0);
        PciBarMmioHandler::write(&mut mmio, 0, 0, 0xDEAD_BEEF);
    }

    fn build_paged_serial_boot_sector(message: &[u8]) -> [u8; 512] {
        assert!(!message.is_empty());
        assert!(message.len() <= 32, "test boot sector message too long");

        // Identity-map the code page (0x7000) so execution continues after enabling paging.
        //
        // Map a separate linear page (0x4000) to a different physical page (0x2000) containing
        // the output message. If paging is not active, the guest will read from physical 0x4000
        // instead and the serial output will not match.
        const PD_BASE: u16 = 0x1000;
        const PT_BASE: u16 = 0x3000;
        const MSG_PHYS_BASE: u16 = 0x2000;
        const MSG_LINEAR_BASE: u16 = 0x4000;

        let mut sector = [0u8; 512];
        let mut i = 0usize;

        // xor ax, ax
        sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
        i += 2;
        // mov ds, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
        i += 2;

        // Write the message bytes into a physical RAM page (MSG_PHYS_BASE).
        for (off, &b) in message.iter().enumerate() {
            let addr = MSG_PHYS_BASE.wrapping_add(off as u16);
            // mov byte ptr [addr], imm8
            sector[i..i + 5].copy_from_slice(&[0xC6, 0x06, addr as u8, (addr >> 8) as u8, b]);
            i += 5;
        }

        // PDE[0] -> page table at PT_BASE (present + RW).
        let pde0: u32 = (PT_BASE as u32) | 0x3;
        // 66 c7 06 <disp16> <imm32>
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (PD_BASE & 0xFF) as u8,
            (PD_BASE >> 8) as u8,
            (pde0 & 0xFF) as u8,
            ((pde0 >> 8) & 0xFF) as u8,
            ((pde0 >> 16) & 0xFF) as u8,
            ((pde0 >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // PTE[MSG_LINEAR_BASE >> 12] -> MSG_PHYS_BASE (present + RW).
        let pte_msg_off = PT_BASE.wrapping_add(((MSG_LINEAR_BASE as u32 >> 12) * 4) as u16);
        let pte_msg: u32 = (MSG_PHYS_BASE as u32) | 0x3;
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (pte_msg_off & 0xFF) as u8,
            (pte_msg_off >> 8) as u8,
            (pte_msg & 0xFF) as u8,
            ((pte_msg >> 8) & 0xFF) as u8,
            ((pte_msg >> 16) & 0xFF) as u8,
            ((pte_msg >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // PTE[0x7000 >> 12] -> 0x7000 (code page identity map; present + RW).
        let pte_code_off = PT_BASE.wrapping_add(((0x7000u32 >> 12) * 4) as u16);
        let pte_code: u32 = 0x7000 | 0x3;
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (pte_code_off & 0xFF) as u8,
            (pte_code_off >> 8) as u8,
            (pte_code & 0xFF) as u8,
            ((pte_code >> 8) & 0xFF) as u8,
            ((pte_code >> 16) & 0xFF) as u8,
            ((pte_code >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // mov eax, PD_BASE (32-bit immediate)
        sector[i..i + 6].copy_from_slice(&[
            0x66,
            0xB8,
            (PD_BASE & 0xFF) as u8,
            (PD_BASE >> 8) as u8,
            0x00,
            0x00,
        ]);
        i += 6;
        // mov cr3, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xD8]);
        i += 3;

        // mov eax, cr0
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x20, 0xC0]);
        i += 3;
        // or eax, 0x8000_0000
        sector[i..i + 6].copy_from_slice(&[0x66, 0x0D, 0x00, 0x00, 0x00, 0x80]);
        i += 6;
        // mov cr0, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xC0]);
        i += 3;

        // mov dx, 0x3f8
        sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;

        for (off, _) in message.iter().enumerate() {
            let addr = MSG_LINEAR_BASE.wrapping_add(off as u16);
            // mov al, moffs8
            sector[i..i + 3].copy_from_slice(&[0xA0, addr as u8, (addr >> 8) as u8]);
            i += 3;
            // out dx, al
            sector[i] = 0xEE;
            i += 1;
        }

        // hlt
        sector[i] = 0xF4;

        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    fn build_long_mode_paged_serial_boot_sector(message: &[u8]) -> [u8; 512] {
        assert!(!message.is_empty());
        assert!(
            message.len() <= 64,
            "test boot sector message too long (must fit in disp8 addressing)"
        );

        // This boot sector:
        // - writes `message` into a *different physical page* (`MSG_PHYS_BASE`)
        // - sets up 4-level (long mode) paging mapping:
        //     - code page @ 0x7000  -> physical 0x7000 (identity)
        //     - msg page  @ 0x4000  -> physical MSG_PHYS_BASE
        // - enables IA-32e long mode (PAE + EFER.LME + CR0.PG + CR0.PE)
        // - jumps to a 64-bit code segment and prints the message via COM1.
        //
        // If paging translation is not active, the guest will read from physical 0x4000 (the page
        // table page) instead of the message bytes, and the serial output will not match.
        const PML4_BASE: u16 = 0x1000;
        const PDPT_BASE: u16 = 0x2000;
        const PD_BASE: u16 = 0x3000;
        const PT_BASE: u16 = 0x4000;
        const MSG_PHYS_BASE: u16 = 0x5000;
        const MSG_LINEAR_BASE: u32 = 0x4000;

        // GDT + GDTR pointer are embedded in the boot sector (loaded at 0x7C00).
        const GDTR_OFF: usize = 0x1E0;
        const GDT_OFF: usize = GDTR_OFF + 6;

        let mut sector = [0u8; 512];
        let mut i = 0usize;

        fn write_dword(sector: &mut [u8; 512], i: &mut usize, addr: u16, value: u32) {
            // 66 c7 06 <disp16> <imm32>
            sector[*i..*i + 9].copy_from_slice(&[
                0x66,
                0xC7,
                0x06,
                (addr & 0xFF) as u8,
                (addr >> 8) as u8,
                (value & 0xFF) as u8,
                ((value >> 8) & 0xFF) as u8,
                ((value >> 16) & 0xFF) as u8,
                ((value >> 24) & 0xFF) as u8,
            ]);
            *i += 9;
        }

        // xor ax, ax
        sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
        i += 2;
        // mov ds, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
        i += 2;

        // Write the message bytes into a physical RAM page (MSG_PHYS_BASE).
        for (off, &b) in message.iter().enumerate() {
            let addr = MSG_PHYS_BASE.wrapping_add(off as u16);
            // mov byte ptr [addr], imm8
            sector[i..i + 5].copy_from_slice(&[0xC6, 0x06, addr as u8, (addr >> 8) as u8, b]);
            i += 5;
        }

        // Build long mode page tables. We only populate the entries needed for:
        // - the boot sector code/data page at 0x7000, and
        // - the message page at 0x4000.
        //
        // Write the low dword and explicitly zero the high dword so we don't rely on RAM being
        // pre-zeroed.
        let pml4e0: u32 = (PDPT_BASE as u32) | 0x7;
        write_dword(&mut sector, &mut i, PML4_BASE, pml4e0);
        write_dword(&mut sector, &mut i, PML4_BASE.wrapping_add(4), 0);

        let pdpte0: u32 = (PD_BASE as u32) | 0x7;
        write_dword(&mut sector, &mut i, PDPT_BASE, pdpte0);
        write_dword(&mut sector, &mut i, PDPT_BASE.wrapping_add(4), 0);

        let pde0: u32 = (PT_BASE as u32) | 0x7;
        write_dword(&mut sector, &mut i, PD_BASE, pde0);
        write_dword(&mut sector, &mut i, PD_BASE.wrapping_add(4), 0);

        let pte_msg_off = PT_BASE.wrapping_add(((MSG_LINEAR_BASE >> 12) * 8) as u16);
        let pte_msg: u32 = (MSG_PHYS_BASE as u32) | 0x7;
        write_dword(&mut sector, &mut i, pte_msg_off, pte_msg);
        write_dword(&mut sector, &mut i, pte_msg_off.wrapping_add(4), 0);

        let pte_code_off = PT_BASE.wrapping_add(((0x7000u32 >> 12) * 8) as u16);
        let pte_code: u32 = 0x7000 | 0x7;
        write_dword(&mut sector, &mut i, pte_code_off, pte_code);
        write_dword(&mut sector, &mut i, pte_code_off.wrapping_add(4), 0);

        // lgdt [0x7C00 + GDTR_OFF]
        let gdtr_addr: u16 = 0x7C00u16.wrapping_add(GDTR_OFF as u16);
        sector[i..i + 5].copy_from_slice(&[
            0x0F,
            0x01,
            0x16,
            gdtr_addr as u8,
            (gdtr_addr >> 8) as u8,
        ]);
        i += 5;

        // Enable CR4.PAE (bit 5) for long mode paging.
        // mov eax, cr4
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x20, 0xE0]);
        i += 3;
        // or eax, 0x20
        sector[i..i + 4].copy_from_slice(&[0x66, 0x83, 0xC8, 0x20]);
        i += 4;
        // mov cr4, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xE0]);
        i += 3;

        // Set IA32_EFER.LME via WRMSR (MSR 0xC000_0080).
        // mov ecx, 0xC000_0080
        sector[i..i + 6].copy_from_slice(&[0x66, 0xB9, 0x80, 0x00, 0x00, 0xC0]);
        i += 6;
        // mov eax, 0x0000_0100 (LME)
        sector[i..i + 6].copy_from_slice(&[0x66, 0xB8, 0x00, 0x01, 0x00, 0x00]);
        i += 6;
        // mov edx, 0
        sector[i..i + 6].copy_from_slice(&[0x66, 0xBA, 0x00, 0x00, 0x00, 0x00]);
        i += 6;
        // wrmsr
        sector[i..i + 2].copy_from_slice(&[0x0F, 0x30]);
        i += 2;

        // mov eax, PML4_BASE
        sector[i..i + 6].copy_from_slice(&[
            0x66,
            0xB8,
            (PML4_BASE & 0xFF) as u8,
            (PML4_BASE >> 8) as u8,
            0x00,
            0x00,
        ]);
        i += 6;
        // mov cr3, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xD8]);
        i += 3;

        // Enable protected mode + paging (CR0.PE | CR0.PG).
        // mov eax, cr0
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x20, 0xC0]);
        i += 3;
        // or eax, 0x8000_0001
        sector[i..i + 6].copy_from_slice(&[0x66, 0x0D, 0x01, 0x00, 0x00, 0x80]);
        i += 6;
        // mov cr0, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xC0]);
        i += 3;

        // Far jump to 64-bit code segment (selector 0x08). This is a 16-bit far jump (offset16 +
        // selector16) because we're still executing 16-bit code at this point. Keep the target
        // within the 64KiB window.
        let long_mode_entry = 0x7C00u16.wrapping_add((i + 5) as u16);
        sector[i..i + 5].copy_from_slice(&[
            0xEA,
            (long_mode_entry & 0xFF) as u8,
            (long_mode_entry >> 8) as u8,
            0x08,
            0x00,
        ]);
        i += 5;

        // ---- 64-bit code (long mode) --------------------------------------------------------

        // mov ax, 0x10
        sector[i..i + 4].copy_from_slice(&[0x66, 0xB8, 0x10, 0x00]);
        i += 4;
        // mov ds, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
        i += 2;
        // mov es, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
        i += 2;
        // mov ss, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD0]);
        i += 2;

        // mov edx, 0x3f8
        sector[i..i + 5].copy_from_slice(&[0xBA, 0xF8, 0x03, 0x00, 0x00]);
        i += 5;
        // mov esi, MSG_LINEAR_BASE
        sector[i..i + 5].copy_from_slice(&[
            0xBE,
            (MSG_LINEAR_BASE & 0xFF) as u8,
            ((MSG_LINEAR_BASE >> 8) & 0xFF) as u8,
            ((MSG_LINEAR_BASE >> 16) & 0xFF) as u8,
            ((MSG_LINEAR_BASE >> 24) & 0xFF) as u8,
        ]);
        i += 5;

        for (off, _) in message.iter().enumerate() {
            let disp = u8::try_from(off).unwrap_or(0);
            // mov al, byte ptr [rsi + disp8]
            sector[i..i + 3].copy_from_slice(&[0x8A, 0x46, disp]);
            i += 3;
            // out dx, al
            sector[i] = 0xEE;
            i += 1;
        }

        // hlt
        sector[i] = 0xF4;

        // ---- GDTR + GDT ---------------------------------------------------------------------

        // GDTR (limit=u16, base=u32) at 0x7C00 + GDTR_OFF.
        let gdt_base = 0x7C00u32 + (GDT_OFF as u32);
        let gdt_limit: u16 = (3 * 8 - 1) as u16;
        sector[GDTR_OFF..GDTR_OFF + 6].copy_from_slice(&[
            (gdt_limit & 0xFF) as u8,
            (gdt_limit >> 8) as u8,
            (gdt_base & 0xFF) as u8,
            ((gdt_base >> 8) & 0xFF) as u8,
            ((gdt_base >> 16) & 0xFF) as u8,
            ((gdt_base >> 24) & 0xFF) as u8,
        ]);

        // Null descriptor.
        sector[GDT_OFF..GDT_OFF + 8].fill(0);
        // 64-bit code descriptor (base=0, limit=4GB, L=1, D=0).
        sector[GDT_OFF + 8..GDT_OFF + 16]
            .copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0xAF, 0x00]);
        // Data descriptor (base=0, limit=4GB).
        sector[GDT_OFF + 16..GDT_OFF + 24]
            .copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x8F, 0x00]);

        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    #[test]
    fn boots_mbr_and_writes_to_serial() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let boot = build_serial_boot_sector(b"OK\n");
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();

        for _ in 0..100 {
            match m.run_slice(10_000) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let out = m.take_serial_output();
        assert_eq!(out, b"OK\n");
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn install_media_overlay_ref_is_updated_by_attach_and_eject_helpers() {
        use aero_storage::{MemBackend, RawDisk};

        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 8 * 1024 * 1024,
            enable_pc_platform: true,
            enable_ide: true,
            ..Default::default()
        })
        .unwrap();

        assert!(
            m.ide_secondary_master_atapi_overlay.is_none(),
            "install media overlay ref should start unset"
        );
        assert!(
            m.install_media.is_none(),
            "install media backend should start detached"
        );

        let iso_bytes = vec![0u8; 2048 * 16];
        let disk = RawDisk::open(MemBackend::from_vec(iso_bytes)).unwrap();
        m.attach_install_media_iso_and_set_overlay_ref(Box::new(disk), "/state/win7.iso")
            .unwrap();

        let overlay = m
            .ide_secondary_master_atapi_overlay
            .as_ref()
            .expect("overlay ref should be set after attach_install_media_iso_and_set_overlay_ref");
        assert_eq!(overlay.disk_id, Machine::DISK_ID_INSTALL_MEDIA);
        assert_eq!(overlay.base_image, "/state/win7.iso");
        assert_eq!(overlay.overlay_image, "");
        assert!(
            m.install_media
                .as_ref()
                .and_then(InstallMedia::upgrade)
                .is_some(),
            "install media backend should be attached after attach_install_media_iso_and_set_overlay_ref"
        );

        m.eject_install_media();
        assert!(
            m.ide_secondary_master_atapi_overlay.is_none(),
            "install media overlay ref should be cleared after eject_install_media"
        );
        assert!(
            m.install_media.is_none(),
            "install media backend should be detached after eject_install_media"
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn snapshot_restore_drops_install_media_backend_handle() {
        use aero_storage::{MemBackend, RawDisk};

        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 8 * 1024 * 1024,
            enable_pc_platform: true,
            enable_ide: true,
            ..Default::default()
        })
        .unwrap();

        let iso_bytes = vec![0u8; 2048 * 16];
        let disk = RawDisk::open(MemBackend::from_vec(iso_bytes)).unwrap();
        m.attach_install_media_iso_and_set_overlay_ref(Box::new(disk), "/state/win7.iso")
            .unwrap();
        assert!(
            m.install_media
                .as_ref()
                .and_then(InstallMedia::upgrade)
                .is_some(),
            "expected install media backend to be attached before snapshot"
        );

        let snap = m.take_snapshot_full().unwrap();

        // Mutate state away from the snapshot.
        m.eject_install_media();
        assert!(
            m.install_media.is_none(),
            "expected install media backend to be detached after explicit eject"
        );
        assert!(
            m.ide_secondary_master_atapi_overlay.is_none(),
            "expected install media overlay ref to be cleared after explicit eject"
        );

        m.restore_snapshot_bytes(&snap).unwrap();

        // Snapshot restore drops host backends; install media must be reattached by the platform.
        assert!(
            m.install_media.is_none(),
            "install media backend should be detached after snapshot restore"
        );
        // Overlay refs are part of snapshot state and should be restored so the host can reattach.
        let overlay = m
            .ide_secondary_master_atapi_overlay
            .as_ref()
            .expect("install media overlay ref should be restored from snapshot");
        assert_eq!(overlay.disk_id, Machine::DISK_ID_INSTALL_MEDIA);
        assert_eq!(overlay.base_image, "/state/win7.iso");
        assert_eq!(overlay.overlay_image, "");
    }

    #[test]
    fn snapshot_restore_drops_network_backend_even_when_restoring_via_snapshot_crate() {
        struct DropBackend {
            dropped: Arc<AtomicUsize>,
        }

        impl aero_net_backend::NetworkBackend for DropBackend {
            fn transmit(&mut self, _frame: Vec<u8>) {}
        }

        impl Drop for DropBackend {
            fn drop(&mut self) {
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }

        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };
        let mut m = Machine::new(cfg).unwrap();
        let snap = m.take_snapshot_full().unwrap();

        let dropped = Arc::new(AtomicUsize::new(0));
        m.set_network_backend(Box::new(DropBackend {
            dropped: dropped.clone(),
        }));

        // Restore via the snapshot crate directly (bypasses `Machine::restore_snapshot_*` helpers).
        snapshot::restore_snapshot(&mut Cursor::new(&snap), &mut m).unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn paging_translation_and_io_work_together() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let boot = build_paged_serial_boot_sector(b"OK\n");
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();

        // Run at least two slices with paging enabled so the machine-level bus/MMU can be reused
        // across `run_slice` calls.
        match m.run_slice(15) {
            RunExit::Completed { .. } => {}
            other => panic!("unexpected exit: {other:?}"),
        }
        assert_ne!(
            m.cpu().control.cr0 & aero_cpu_core::state::CR0_PG,
            0,
            "expected paging to be enabled after first slice"
        );

        for _ in 0..200 {
            match m.run_slice(15) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let out = m.take_serial_output();
        assert_eq!(out, b"OK\n");
    }

    #[test]
    fn long_mode_paging_translation_and_io_work_together() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let boot = build_long_mode_paged_serial_boot_sector(b"LM\n");
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();

        for _ in 0..200 {
            match m.run_slice(50_000) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let out = m.take_serial_output();
        assert_eq!(out, b"LM\n");
    }

    #[test]
    fn snapshot_restore_syncs_time_source_with_ia32_tsc() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.time.set_tsc(0x1234);
        src.cpu.state.msr.tsc = 0x1234;
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert_eq!(restored.cpu.state.msr.tsc, 0x1234);
        assert_eq!(restored.cpu.time.read_tsc(), 0x1234);
    }

    #[test]
    fn boot_drive_is_preserved_across_reset_and_snapshot_restore() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut m = Machine::new(cfg.clone()).unwrap();
        m.set_boot_drive(0xE0);
        m.reset();
        assert_eq!(m.bios.config().boot_drive, 0xE0);

        let snap = m.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();
        assert_eq!(restored.bios.config().boot_drive, 0xE0);

        restored.reset();
        assert_eq!(restored.bios.config().boot_drive, 0xE0);
    }

    #[test]
    fn run_slice_advances_bios_bda_ticks_deterministically_without_pc_platform() {
        use firmware::bios::{BDA_TICK_COUNT_ADDR, TICKS_PER_DAY};

        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: false,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        // Simple infinite loop boot sector: CLI; JMP $-2.
        let mut sector = [0u8; 512];
        sector[0] = 0xFA;
        sector[1] = 0xEB;
        sector[2] = 0xFE;
        sector[510] = 0x55;
        sector[511] = 0xAA;

        m.set_disk_image(sector.to_vec()).unwrap();
        m.reset();

        // Use a tiny deterministic TSC frequency so we can advance 1 second worth of guest time with
        // a small number of retired instructions (Tier-0 currently uses 1 cycle per instruction).
        m.cpu.time.set_tsc_hz(1000);
        m.cpu.state.msr.tsc = m.cpu.time.read_tsc();

        let start = m.read_physical_u32(BDA_TICK_COUNT_ADDR);
        let cycles_per_second = m.cpu.time.tsc_hz();
        assert!(cycles_per_second > 0);

        for elapsed_secs in 1u64..=10 {
            match m.run_slice(cycles_per_second) {
                RunExit::Completed { executed } => assert_eq!(executed, cycles_per_second),
                other => panic!("unexpected run exit: {other:?}"),
            }

            let expected_delta: u32 = (u64::from(TICKS_PER_DAY) * elapsed_secs / 86_400)
                .try_into()
                .unwrap();
            let expected = start.wrapping_add(expected_delta);
            assert_eq!(
                m.read_physical_u32(BDA_TICK_COUNT_ADDR),
                expected,
                "unexpected tick count after {elapsed_secs} seconds"
            );
        }
    }

    #[test]
    fn halted_run_slice_advances_bios_bda_ticks_without_pc_platform() {
        use firmware::bios::{BDA_TICK_COUNT_ADDR, TICKS_PER_DAY};

        // This constant is not part of the public BIOS API but is required to compute expected
        // BDA tick deltas here.
        const NANOS_PER_DAY: u128 = 86_400_000_000_000;

        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: false,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        // Boot sector: STI; HLT; JMP $-3 (halt loop).
        let mut sector = [0u8; 512];
        sector[0] = 0xFB; // sti
        sector[1] = 0xF4; // hlt
        sector[2] = 0xEB; // jmp short
        sector[3] = 0xFD; // -3 (back to hlt)
        sector[510] = 0x55;
        sector[511] = 0xAA;

        m.set_disk_image(sector.to_vec()).unwrap();
        m.reset();

        // Use a small deterministic TSC frequency so `idle_tick_platform_1ms` advances exactly 1ms
        // per call.
        m.cpu.time.set_tsc_hz(1000);
        m.cpu.state.msr.tsc = m.cpu.time.read_tsc();

        // Run until the guest halts.
        for _ in 0..200 {
            match m.run_slice(50_000) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let start = m.read_physical_u32(BDA_TICK_COUNT_ADDR);

        // While halted, each `run_slice` call advances guest time by 1ms (deterministically). After
        // 200ms, the BIOS tick count (18.2Hz) must have advanced by at least 3 ticks.
        const IDLE_MS: u64 = 200;
        for _ in 0..IDLE_MS {
            assert!(matches!(m.run_slice(1), RunExit::Halted { executed: 0 }));
        }

        let end = m.read_physical_u32(BDA_TICK_COUNT_ADDR);
        let delta = end.wrapping_sub(start);

        let expected_min: u32 = (u128::from(IDLE_MS) * 1_000_000u128 * u128::from(TICKS_PER_DAY)
            / NANOS_PER_DAY)
            .try_into()
            .unwrap();
        assert!(
            delta >= expected_min,
            "expected BIOS tick count to advance while halted (delta={delta}, expected >= {expected_min})"
        );
        assert!(
            delta <= expected_min.saturating_add(1),
            "expected BIOS tick count advance to be bounded (delta={delta}, expected <= {})",
            expected_min.saturating_add(1)
        );
    }

    #[test]
    fn snapshot_restore_flushes_persistent_mmu_tlb() {
        // Regression test: snapshots restore RAM + paging control registers, but the machine keeps
        // a persistent `aero_mmu::Mmu` with an internal TLB cache. If we restore a snapshot without
        // flushing the MMU, stale translations from "after the snapshot" can be used even when the
        // paging register values (CR0/CR3/CR4/EFER) match, breaking determinism.
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        };
        let mut m = Machine::new(cfg).unwrap();

        // Build a simple 32-bit paging setup:
        //  - PD[0] -> PT
        //  - PT[0] -> code page (linear 0x0000_0000)
        //  - PT[1] -> data page (linear 0x0000_1000), patched later
        let pd_base = 0x1000u64;
        let pt_base = 0x2000u64;
        let code_page = 0x3000u64;
        let page_a = 0x4000u64;
        let page_b = 0x5000u64;

        const PTE_P: u32 = 1 << 0;
        const PTE_RW: u32 = 1 << 1;
        let flags = PTE_P | PTE_RW;

        // Code:
        //   mov eax, dword ptr [0x0000_1000]   ; populate TLB
        //   invlpg [0x0000_1000]               ; flush and re-walk after PTE patch
        //   mov eax, dword ptr [0x0000_1000]   ; populate TLB with new mapping
        //   hlt
        let code: [u8; 18] = [
            0xA1, 0x00, 0x10, 0x00, 0x00, // mov eax, [0x1000]
            0x0F, 0x01, 0x3D, 0x00, 0x10, 0x00, 0x00, // invlpg [0x1000]
            0xA1, 0x00, 0x10, 0x00, 0x00, // mov eax, [0x1000]
            0xF4, // hlt
        ];

        {
            let mem = &mut m.mem;
            mem.write_u32(pd_base, (pt_base as u32) | flags);
            mem.write_u32(pt_base, (code_page as u32) | flags);
            mem.write_u32(pt_base + 4, (page_a as u32) | flags);

            mem.write_u32(page_a, 0x1111_1111);
            mem.write_u32(page_b, 0x2222_2222);

            mem.write_physical(code_page, &code);
        }

        // Jump directly into 32-bit paging mode without relying on BIOS/boot code.
        m.cpu = CpuCore::new(CpuMode::Protected);
        m.cpu.state.control.cr3 = pd_base;
        m.cpu.state.control.cr0 = CR0_PE | CR0_PG;
        m.cpu.state.control.cr4 = 0;
        m.cpu.state.update_mode();
        m.cpu.state.set_rip(0);

        // Execute the first load to populate the TLB with the page-A mapping.
        assert_eq!(m.run_slice(1), RunExit::Completed { executed: 1 });
        assert_eq!(m.cpu.state.read_gpr32(gpr::RAX), 0x1111_1111);

        // Force RIP back to 0 so the post-restore load happens *without* INVLPG.
        m.cpu.state.set_rip(0);
        let snap = m.take_snapshot_full().unwrap();

        // Patch the PTE so linear 0x1000 now maps to page B.
        m.mem.write_u32(pt_base + 4, (page_b as u32) | flags);

        // Run the rest of the code, which executes INVLPG + a second load to populate the TLB with
        // the page-B mapping.
        assert!(matches!(m.run_slice(10), RunExit::Halted { .. }));
        assert_eq!(m.cpu.state.read_gpr32(gpr::RAX), 0x2222_2222);

        // Restoring the snapshot should clear the MMU cache so the next load observes page A.
        m.restore_snapshot_bytes(&snap).unwrap();
        m.cpu.state.write_gpr32(gpr::RAX, 0);
        assert_eq!(m.run_slice(1), RunExit::Completed { executed: 1 });
        assert_eq!(m.cpu.state.read_gpr32(gpr::RAX), 0x1111_1111);
    }

    #[test]
    fn snapshot_restore_roundtrips_cpu_internal_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        // Use a non-standard value (>1) to ensure snapshot/restore preserves the raw counter rather
        // than clamping it to the Tier-0 current "0/1 only" semantics.
        src.cpu.pending.set_interrupt_inhibit(7);
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.cpu.pending.set_interrupt_inhibit(0);
        restored.cpu.pending.inject_external_interrupt(0x33);
        restored.cpu.pending.raise_software_interrupt(0x80, 0);
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert!(!restored.cpu.pending.has_pending_event());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 7);
        assert_eq!(
            restored
                .cpu
                .pending
                .external_interrupts()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0x20, 0x21]
        );
    }

    #[test]
    fn dirty_snapshot_roundtrips_cpu_internal_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        // Base snapshot: initial CPU_INTERNAL state.
        src.cpu.pending.set_interrupt_inhibit(7);
        src.cpu.pending.inject_external_interrupt(0x20);
        let base = src.take_snapshot_full().unwrap();

        // Dirty snapshot: updated CPU_INTERNAL state, with no RAM changes required.
        src.cpu.pending.set_interrupt_inhibit(1);
        src.cpu.pending.clear_external_interrupts();
        src.cpu.pending.inject_external_interrupt(0x33);
        src.cpu.pending.inject_external_interrupt(0x34);
        let diff = src.take_snapshot_dirty().unwrap();

        // Restore chain (base -> diff).
        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&base).unwrap();
        restored.restore_snapshot_bytes(&diff).unwrap();

        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 1);
        assert_eq!(
            restored
                .cpu
                .pending
                .external_interrupts()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0x33, 0x34]
        );
    }

    fn strip_cpu_internal_device_state(bytes: &[u8]) -> Vec<u8> {
        const FILE_HEADER_LEN: usize = 16;
        const SECTION_HEADER_LEN: usize = 16;

        let mut r = Cursor::new(bytes);
        let mut file_header = [0u8; FILE_HEADER_LEN];
        r.read_exact(&mut file_header).unwrap();

        let mut out = Vec::with_capacity(bytes.len());
        out.extend_from_slice(&file_header);

        let mut removed = 0usize;

        while (r.position() as usize) < bytes.len() {
            let mut section_header = [0u8; SECTION_HEADER_LEN];
            // Valid snapshots end cleanly at EOF.
            if let Err(e) = r.read_exact(&mut section_header) {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    break;
                }
                panic!("failed to read section header: {e}");
            }

            let id = u32::from_le_bytes(section_header[0..4].try_into().unwrap());
            let version = u16::from_le_bytes(section_header[4..6].try_into().unwrap());
            let flags = u16::from_le_bytes(section_header[6..8].try_into().unwrap());
            let len = u64::from_le_bytes(section_header[8..16].try_into().unwrap());

            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload).unwrap();

            if id != snapshot::SectionId::DEVICES.0 {
                out.extend_from_slice(&section_header);
                out.extend_from_slice(&payload);
                continue;
            }

            let mut pr = Cursor::new(&payload);
            let mut count_bytes = [0u8; 4];
            pr.read_exact(&mut count_bytes).unwrap();
            let count = u32::from_le_bytes(count_bytes) as usize;

            let mut kept = Vec::new();
            for _ in 0..count {
                let mut dev_header = [0u8; 16];
                pr.read_exact(&mut dev_header).unwrap();
                let dev_id = u32::from_le_bytes(dev_header[0..4].try_into().unwrap());
                let dev_len = u64::from_le_bytes(dev_header[8..16].try_into().unwrap());
                let mut dev_data = vec![0u8; dev_len as usize];
                pr.read_exact(&mut dev_data).unwrap();

                if dev_id == snapshot::DeviceId::CPU_INTERNAL.0 {
                    removed += 1;
                    continue;
                }

                let mut bytes = Vec::with_capacity(dev_header.len() + dev_data.len());
                bytes.extend_from_slice(&dev_header);
                bytes.extend_from_slice(&dev_data);
                kept.push(bytes);
            }

            assert_eq!(
                pr.position() as usize,
                payload.len(),
                "devices section parse did not consume full payload"
            );

            let mut new_payload = Vec::new();
            let new_count: u32 = kept.len().try_into().unwrap();
            new_payload.extend_from_slice(&new_count.to_le_bytes());
            for dev in kept {
                new_payload.extend_from_slice(&dev);
            }
            let new_len: u64 = new_payload.len().try_into().unwrap();

            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&version.to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&new_len.to_le_bytes());
            out.extend_from_slice(&new_payload);
        }

        assert!(removed > 0, "snapshot did not contain a CPU_INTERNAL entry");
        out
    }

    #[test]
    fn restore_snapshot_without_cpu_internal_clears_pending_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.set_interrupt_inhibit(7);
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        let snap = src.take_snapshot_full().unwrap();
        let snap_without_cpu_internal = strip_cpu_internal_device_state(&snap);

        let mut restored = Machine::new(cfg).unwrap();
        restored.cpu.pending.set_interrupt_inhibit(1);
        restored.cpu.pending.inject_external_interrupt(0x33);
        restored.cpu.pending.raise_software_interrupt(0x80, 0);

        restored
            .restore_snapshot_bytes(&snap_without_cpu_internal)
            .unwrap();

        assert!(!restored.cpu.pending.has_pending_event());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 0);
        assert!(restored.cpu.pending.external_interrupts().is_empty());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 0);
    }

    #[test]
    fn snapshot_restore_preserves_cpu_internal_interrupt_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        src.cpu.pending.inhibit_interrupts_for_one_instruction();

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        // Ensure restore does not merge with pre-existing state.
        restored.cpu.pending.inject_external_interrupt(0x99);
        restored.cpu.pending.set_interrupt_inhibit(0);

        restored.restore_snapshot_bytes(&snap).unwrap();

        let restored_irqs: Vec<u8> = restored
            .cpu
            .pending
            .external_interrupts()
            .iter()
            .copied()
            .collect();
        assert_eq!(restored_irqs, vec![0x20, 0x21]);
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 1);
    }

    #[test]
    fn snapshot_restore_preserves_interrupt_shadow_and_ages_after_one_instruction() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();

        // Program: NOP; HLT.
        src.write_physical(0x100, &[0x90, 0xF4]);
        src.cpu.state.mode = CpuMode::Real;
        src.cpu.state.segments.cs.selector = 0;
        src.cpu.state.segments.cs.base = 0;
        src.cpu.state.set_rip(0x100);
        src.cpu.state.halted = false;

        src.cpu.pending.set_interrupt_inhibit(1);
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 1);
        let exit = restored.run_slice(1);
        assert_eq!(exit.executed(), 1);
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 0);
    }

    #[test]
    fn inject_keyboard_and_mouse_produces_i8042_output_bytes() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        m.inject_browser_key("KeyA", true);
        m.inject_browser_key("KeyA", false);

        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);

        // Enable mouse reporting so injected motion generates stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        m.inject_mouse_motion(10, 5, 0);
        let packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(packet, vec![0x28, 10, 0xFB]);
    }

    #[test]
    fn ps2_keyboard_leds_are_reported_as_hid_style_bitmask() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        assert!(m.i8042.is_some(), "sanity: default config enables i8042");

        // Default: no LEDs asserted.
        assert_eq!(m.ps2_keyboard_leds(), 0);

        // PS/2 Set LEDs payload uses bit1=NumLock. The machine API reports HID-style bit0=NumLock.
        m.io_write(aero_devices::i8042::I8042_DATA_PORT, 1, 0xED); // Set LEDs
        m.io_write(aero_devices::i8042::I8042_DATA_PORT, 1, 0x02); // NumLock (PS/2 bit1)
        assert_eq!(m.ps2_keyboard_leds(), 0x01);

        // PS/2 bit2=CapsLock should map to HID bit1.
        m.io_write(aero_devices::i8042::I8042_DATA_PORT, 1, 0xED);
        m.io_write(aero_devices::i8042::I8042_DATA_PORT, 1, 0x04);
        assert_eq!(m.ps2_keyboard_leds(), 0x02);

        // PS/2 bit0=ScrollLock should map to HID bit2.
        m.io_write(aero_devices::i8042::I8042_DATA_PORT, 1, 0xED);
        m.io_write(aero_devices::i8042::I8042_DATA_PORT, 1, 0x01);
        assert_eq!(m.ps2_keyboard_leds(), 0x04);
    }

    #[test]
    fn ps2_keyboard_leds_can_be_read_while_i8042_is_borrowed() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        assert!(m.i8042.is_some(), "sanity: default config enables i8042");

        // Set NumLock on via the PS/2 Set LEDs command.
        m.io_write(aero_devices::i8042::I8042_DATA_PORT, 1, 0xED);
        m.io_write(aero_devices::i8042::I8042_DATA_PORT, 1, 0x02);

        let ctrl = m.i8042.as_ref().unwrap().borrow();
        assert_eq!(
            m.ps2_keyboard_leds(),
            0x01,
            "expected ps2_keyboard_leds to succeed under an existing i8042 borrow"
        );
        drop(ctrl);
    }

    #[test]
    fn inject_input_batch_produces_i8042_output_bytes() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Enable mouse reporting so injected motion generates stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        // InputEventQueue wire format:
        //   [count, batch_ts, type, event_ts, a, b, ...]
        //
        // Batch: KeyA make + break + mouse move (dx=10, dy=5 in PS/2 coordinates).
        let words: [u32; 14] = [
            3,
            0,
            // KeyScancode: 0x1C (make)
            1,
            0,
            0x1C,
            1,
            // KeyScancode: 0xF0 0x1C (break)
            1,
            0,
            0x0000_1CF0,
            2,
            // MouseMove: dx=10, dy=5 (positive=up)
            2,
            0,
            10,
            5,
        ];

        m.inject_input_batch(&words);

        // Drain all bytes produced by the batch. The i8042 model can interleave keyboard and mouse
        // bytes (the real controller has separate sources feeding a shared output buffer), so avoid
        // asserting a strict ordering between devices. Instead, assert the expected bytes are
        // present.
        let mut out = Vec::new();
        for _ in 0..16 {
            // Status bit0: output buffer full.
            if (ctrl.borrow_mut().read_port(0x64) & 0x01) == 0 {
                break;
            }
            out.push(ctrl.borrow_mut().read_port(0x60));
        }

        let mut expected = vec![0x1e, 0x9e, 0x08, 10, 5];
        expected.sort_unstable();
        out.sort_unstable();
        assert_eq!(out, expected);
    }

    #[test]
    fn inject_input_batch_malformed_does_not_panic() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        // Truncated header.
        m.inject_input_batch(&[]);
        m.inject_input_batch(&[1]);

        // Declared count exceeds buffer length (truncated events).
        m.inject_input_batch(&[10, 0, 1, 0, 0x1C, 1]);

        // Unknown event type should be ignored.
        m.inject_input_batch(&[1, 0, 0xDEAD_BEEF, 0, 0, 0]);
    }

    #[test]
    fn inject_key_scancode_bytes_produces_i8042_output_bytes() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Raw Set-2 bytes: make 0x1C, break 0xF0 0x1C. The i8042 translation bit is enabled by
        // default, so we observe Set-1 scancodes on port 0x60.
        m.inject_key_scancode_bytes(&[0x1C]);
        m.inject_key_scancode_bytes(&[0xF0, 0x1C]);

        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);
    }

    #[test]
    fn inject_key_scancode_packed_produces_i8042_output_bytes() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Raw Set-2 bytes: make 0x1C, break 0xF0 0x1C. The i8042 translation bit is enabled by
        // default, so we observe Set-1 scancodes on port 0x60.
        m.inject_key_scancode_packed(0x1C, 1);
        m.inject_key_scancode_packed(0xDEAD_BEEF, 0); // len=0 is a no-op
        m.inject_key_scancode_packed(0x1C << 8 | 0xF0, 2);

        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);
    }

    #[test]
    fn inject_key_scancode_packed_clamps_len_to_4() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // PrintScreen make sequence in Set-2: E0 12 E0 7C (4 bytes). Use an out-of-range length to
        // ensure the helper clamps safely without panicking.
        let packed = u32::from_le_bytes([0xE0, 0x12, 0xE0, 0x7C]);
        m.inject_key_scancode_packed(packed, 255);

        let bytes: Vec<u8> = (0..4).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        // The i8042 translation bit is enabled by default, so we observe Set-1 scancode bytes.
        assert_eq!(bytes, vec![0xE0, 0x2A, 0xE0, 0x37]);
    }

    #[test]
    fn inject_ps2_mouse_motion_inverts_dy() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Enable mouse reporting so injected motion generates stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        // `inject_ps2_mouse_motion` expects dy>0 as "up". The underlying PS/2 mouse model expects
        // browser-style dy>0 as "down", so Machine must invert it.
        m.inject_ps2_mouse_motion(0, 5, 0);
        let packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(packet, vec![0x08, 0x00, 0x05]);
    }

    #[test]
    fn inject_ps2_mouse_motion_handles_i32_min_without_overflow() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Enable mouse reporting so injected motion generates stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        // `dy` uses PS/2 convention (+Y is up). If `dy == i32::MIN`, inverting it would overflow
        // without saturating arithmetic.
        m.inject_ps2_mouse_motion(0, i32::MIN, 0);

        let packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(packet, vec![0x28, 0x00, 0x80]);
    }

    #[test]
    fn inject_mouse_button_dom_maps_left_click() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Enable mouse reporting so injected events generate stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        m.inject_mouse_button_dom(0, true);
        let pressed_packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(pressed_packet, vec![0x09, 0x00, 0x00]);
    }

    #[test]
    fn mixing_per_button_and_absolute_mask_mouse_injection_does_not_stick_buttons() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Enable mouse reporting so injected events generate stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        // Press left via the per-button API.
        m.inject_mouse_button(Ps2MouseButton::Left, true);
        let pressed_packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(pressed_packet, vec![0x09, 0x00, 0x00]);

        // Release via the absolute-mask API.
        m.inject_ps2_mouse_buttons(0x00);
        assert_ne!(ctrl.borrow_mut().read_port(0x64) & 0x01, 0);
        let released_packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(released_packet, vec![0x08, 0x00, 0x00]);
    }

    #[test]
    fn mouse_button_helper_methods_update_host_button_cache() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        // `inject_mouse_buttons_mask` should ignore bits above 0x1f.
        m.inject_mouse_buttons_mask(0xff);
        assert_eq!(m.ps2_mouse_buttons, 0x1f);

        m.inject_mouse_buttons_mask(0x00);
        assert_eq!(m.ps2_mouse_buttons, 0x00);

        m.inject_mouse_back(true);
        assert_eq!(m.ps2_mouse_buttons & 0x08, 0x08);
        m.inject_mouse_forward(true);
        assert_eq!(m.ps2_mouse_buttons & 0x10, 0x10);

        m.inject_mouse_back(false);
        m.inject_mouse_forward(false);
        assert_eq!(m.ps2_mouse_buttons & 0x18, 0x00);
    }

    #[test]
    fn inject_ps2_mouse_buttons_resyncs_after_guest_mouse_reset() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Enable mouse reporting so button injections generate stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        // Set left pressed; cache should now reflect pressed.
        m.inject_ps2_mouse_buttons(0x01);
        let pressed_packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(pressed_packet, vec![0x09, 0x00, 0x00]);
        assert_eq!(m.ps2_mouse_buttons, 0x01);

        // Guest resets the mouse (D4 FF). This clears the device-side button image, but not the
        // host-side cache.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xFF);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xAA); // self-test pass
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x00); // device id

        // Re-enable reporting after reset (D4 F4).
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        // Re-apply the same absolute button mask. This should not be a no-op: the device state was
        // reset, so we expect a new packet with left pressed.
        m.inject_ps2_mouse_buttons(0x01);
        let packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(packet, vec![0x09, 0x00, 0x00]);
    }

    #[test]
    fn snapshot_restore_preserves_i8042_pending_output_bytes() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.inject_browser_key("KeyA", true);
        src.inject_browser_key("KeyA", false);
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x00);
    }

    #[test]
    fn snapshot_restore_preserves_i8042_output_port_and_pending_write() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        let ctrl = src.i8042.as_ref().expect("i8042 enabled").clone();
        {
            let mut dev = ctrl.borrow_mut();
            // Set an initial output-port value.
            dev.write_port(0x64, 0xD1);
            dev.write_port(0x60, 0x03);

            // Leave an in-flight "write output port" pending write.
            dev.write_port(0x64, 0xD1);
        }

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        let mut dev = ctrl.borrow_mut();

        // Verify output port preserved.
        dev.write_port(0x64, 0xD0);
        assert_eq!(dev.read_port(0x60), 0x03);

        // Verify pending write preserved and targets the output port.
        dev.write_port(0x60, 0x01);
        dev.write_port(0x64, 0xD0);
        assert_eq!(dev.read_port(0x60), 0x01);
    }

    #[test]
    fn restoring_i8042_state_resynchronizes_platform_a20() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let src = Machine::new(cfg.clone()).unwrap();
        let ctrl = src.i8042.as_ref().expect("i8042 enabled").clone();

        // Save a snapshot with A20 disabled in the controller output port.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD1);
            dev.write_port(0x60, 0x01);
        }
        assert!(!src.chipset.a20().enabled());

        let state = {
            let dev = ctrl.borrow();
            snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::I8042,
                &*dev,
            )
        };

        // Simulate restoring into an environment where A20 is currently enabled.
        let mut restored = Machine::new(cfg).unwrap();
        restored.chipset.a20().set_enabled(true);
        assert!(restored.chipset.a20().enabled());

        snapshot::SnapshotTarget::restore_device_states(&mut restored, vec![state]);

        assert!(!restored.chipset.a20().enabled());
    }

    #[test]
    fn i8042_injection_apis_are_noops_when_disabled() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_i8042: false,
            ..Default::default()
        })
        .unwrap();

        // Should not panic.
        m.inject_browser_key("KeyA", true);
        m.inject_mouse_motion(1, 2, 3);
        m.inject_mouse_button(Ps2MouseButton::Left, true);
        m.inject_mouse_back(true);
        m.inject_mouse_forward(true);
        m.inject_mouse_buttons_mask(0x1f);
        m.inject_key_scancode_packed(0x1c, 1);
        m.inject_keyboard_bytes(&[0x1c]);
        m.inject_mouse_button_dom(0, true);

        assert!(m.i8042.is_none());
        let devices = snapshot::SnapshotSource::device_states(&m);
        assert!(
            devices.iter().all(|d| d.id != snapshot::DeviceId::I8042),
            "i8042 device state should not be emitted when disabled"
        );
    }

    #[test]
    fn dirty_snapshot_roundtrip_preserves_i8042_pending_output_bytes() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut vm = Machine::new(cfg.clone()).unwrap();
        vm.inject_browser_key("KeyA", true);
        vm.inject_browser_key("KeyA", false);
        let base = vm.take_snapshot_full().unwrap();

        vm.inject_browser_key("KeyB", true);
        vm.inject_browser_key("KeyB", false);
        let diff = vm.take_snapshot_dirty().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&base).unwrap();
        restored.restore_snapshot_bytes(&diff).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x30);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xB0);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x00);
    }

    #[test]
    fn dirty_tracking_includes_device_writes_to_ram() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 16 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        // `Machine::new` performs a reset which clears dirty pages.
        assert!(m.mem.take_dirty_pages().is_empty());

        // Simulate a DMA/device write by bypassing the CPU memory wrapper and writing directly to
        // the underlying physical bus.
        m.mem.write_physical(0x2000, &[0xAA, 0xBB, 0xCC, 0xDD]);

        assert_eq!(m.mem.take_dirty_pages(), vec![2]);

        // Drain semantics.
        assert!(m.mem.take_dirty_pages().is_empty());
    }

    #[test]
    fn dirty_snapshot_includes_device_writes_to_ram() {
        let cfg = MachineConfig {
            ram_size_bytes: 16 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        let base = src.take_snapshot_full().unwrap();

        // Simulate a DMA/device write by bypassing `SystemMemory` and writing directly to the
        // physical bus RAM backend.
        let addr = 0x2000u64;
        let data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        src.mem.write_physical(addr, &data);

        // Take a dirty snapshot diff and ensure the restored VM observes the change.
        let diff = src.take_snapshot_dirty().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&base).unwrap();
        restored.restore_snapshot_bytes(&diff).unwrap();

        assert_eq!(restored.read_physical_bytes(addr, data.len()), data);
    }

    #[test]
    fn snapshot_restore_clears_ps2_mouse_button_cache() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();
        m.inject_ps2_mouse_buttons(0x01);
        assert_eq!(m.ps2_mouse_buttons, 0x01);

        let snap = m.take_snapshot_full().unwrap();

        // Mutate the cache so we can verify restore resets it.
        m.inject_ps2_mouse_buttons(0x07);
        assert_eq!(m.ps2_mouse_buttons, 0x07);

        m.restore_snapshot_bytes(&snap).unwrap();
        assert_eq!(m.ps2_mouse_buttons, 0xFF);

        // Next injection should re-sync and clear the invalid marker.
        m.inject_ps2_mouse_buttons(0x00);
        assert_eq!(m.ps2_mouse_buttons, 0x00);
    }

    #[test]
    fn snapshot_restore_allows_resyncing_ps2_mouse_buttons_to_pressed_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        // Take a snapshot with mouse reporting enabled so button injections generate packets.
        let mut src = Machine::new(cfg.clone()).unwrap();
        {
            let ctrl = src.i8042.as_ref().expect("i8042 enabled").clone();
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(src.io.read_u8(0x60), 0xFA); // mouse ACK

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        // Post-restore the cache is invalid; the first absolute mask should force a resync.
        assert_eq!(restored.ps2_mouse_buttons, 0xFF);

        restored.inject_ps2_mouse_buttons(0x01); // left pressed

        // The first generated packet should reflect the left button down and no movement.
        let packet: Vec<u8> = (0..3).map(|_| restored.io.read_u8(0x60)).collect();
        assert_eq!(packet, vec![0x09, 0x00, 0x00]);
    }

    fn write_ivt_entry(m: &mut Machine, vector: u8, offset: u16, segment: u16) {
        let addr = u64::from(vector) * 4;
        let bytes = [
            (offset & 0xFF) as u8,
            (offset >> 8) as u8,
            (segment & 0xFF) as u8,
            (segment >> 8) as u8,
        ];
        m.mem.write_physical(addr, &bytes);
    }

    fn init_real_mode_cpu(m: &mut Machine, entry_ip: u16, rflags: u64) {
        fn set_real_segment(seg: &mut aero_cpu_core::state::Segment, selector: u16) {
            seg.selector = selector;
            seg.base = u64::from(selector) << 4;
            seg.limit = 0xFFFF;
            seg.access = 0;
        }

        m.cpu.pending = Default::default();
        set_real_segment(&mut m.cpu.state.segments.cs, 0);
        set_real_segment(&mut m.cpu.state.segments.ds, 0);
        set_real_segment(&mut m.cpu.state.segments.es, 0);
        set_real_segment(&mut m.cpu.state.segments.ss, 0);
        m.cpu.state.set_stack_ptr(0x8000);
        m.cpu.state.set_rip(u64::from(entry_ip));
        m.cpu.state.set_rflags(rflags);
        m.cpu.state.halted = false;

        // Ensure the real-mode IVT is in use.
        m.cpu.state.tables.idtr.base = 0;
        m.cpu.state.tables.idtr.limit = 0x03FF;
    }

    #[test]
    fn pc_platform_irq_is_delivered_to_cpu_core() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        // Simple handler for IRQ0 (vector 0x20): write a byte to RAM and IRET.
        //
        // mov byte ptr [0x2000], 0xAA
        // iret
        const HANDLER_IP: u16 = 0x1100;
        m.mem
            .write_physical(u64::from(HANDLER_IP), &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF]);
        write_ivt_entry(&mut m, 0x20, HANDLER_IP, 0x0000);

        // Program CPU at 0x1000 with a small NOP sled.
        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .write_physical(u64::from(ENTRY_IP), &[0x90, 0x90, 0x90, 0x90, 0x90]);
        m.mem.write_physical(0x2000, &[0x00]);

        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);

        // Configure the legacy PIC to use the standard remapped offsets and unmask IRQ0.
        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            for irq in 0..16 {
                ints.pic_mut().set_masked(irq, irq != 0);
            }

            ints.raise_irq(aero_platform::interrupts::InterruptInput::IsaIrq(0));
        }

        // Simulate the CPU being halted: Tier-0 should wake it once the interrupt vector is delivered.
        m.cpu.state.halted = true;

        // Sanity: the interrupt controller sees the pending vector.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );

        // Run a few instructions; the interrupt should be injected and delivered before the first
        // guest instruction executes.
        let exit = m.run_slice(5);
        assert_eq!(exit, RunExit::Completed { executed: 5 });
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
        assert!(
            !m.cpu.state.halted,
            "CPU should wake from HLT once IRQ is delivered"
        );
    }

    #[test]
    fn pc_platform_irq_is_not_acknowledged_during_interrupt_shadow() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        // Simple handler for IRQ0 (vector 0x20): write a byte to RAM and IRET.
        const HANDLER_IP: u16 = 0x1100;
        m.mem
            .write_physical(u64::from(HANDLER_IP), &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF]);
        write_ivt_entry(&mut m, 0x20, HANDLER_IP, 0x0000);

        // Program CPU at 0x1000 with enough NOPs to cover the instruction budgets below.
        const ENTRY_IP: u16 = 0x1000;
        m.mem.write_physical(u64::from(ENTRY_IP), &[0x90; 32]);
        m.mem.write_physical(0x2000, &[0x00]);

        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);
        m.cpu.pending.inhibit_interrupts_for_one_instruction();

        // Configure the legacy PIC to use the standard remapped offsets and unmask IRQ0.
        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            for irq in 0..16 {
                ints.pic_mut().set_masked(irq, irq != 0);
            }
            ints.raise_irq(aero_platform::interrupts::InterruptInput::IsaIrq(0));
        }

        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );

        // While the interrupt shadow is active, the machine should not poll/acknowledge the PIC.
        assert_eq!(m.run_slice(1), RunExit::Completed { executed: 1 });
        assert_eq!(m.cpu.pending.interrupt_inhibit(), 0);
        assert!(m.cpu.pending.external_interrupts().is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );
        assert_eq!(m.read_physical_u8(0x2000), 0x00);

        // Once the shadow expires, the pending IRQ should be acknowledged + delivered.
        let _ = m.run_slice(10);
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
    }

    #[test]
    fn pc_platform_mmio_mappings_route_ioapic_interrupts_in_apic_mode() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            // Keep the machine minimal for deterministic MMIO + interrupt routing assertions.
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();
        // Exercise stable `Rc` identities and idempotent MMIO mappings across resets.
        m.reset();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        interrupts
            .borrow_mut()
            .set_mode(aero_platform::interrupts::PlatformInterruptMode::Apic);

        // Program IOAPIC redirection entry for GSI10 -> vector 0x60 (active-low, level-triggered).
        const GSI: u32 = 10;
        const VECTOR: u32 = 0x60;
        let low: u32 = VECTOR | (1 << 13) | (1 << 15); // polarity low + level triggered
        let redtbl_low = 0x10u32 + GSI * 2;
        let redtbl_high = redtbl_low + 1;

        m.mem.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
        m.mem.write_u32(IOAPIC_MMIO_BASE + 0x10, low);
        m.mem.write_u32(IOAPIC_MMIO_BASE, redtbl_high);
        m.mem.write_u32(IOAPIC_MMIO_BASE + 0x10, 0);

        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        interrupts
            .borrow_mut()
            .raise_irq(aero_platform::interrupts::InterruptInput::Gsi(GSI));

        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(VECTOR as u8)
        );

        // Smoke test LAPIC + HPET MMIO mappings as well.
        let svr = m.read_lapic_u32(0, 0xF0);
        assert_eq!(svr & 0x1FF, 0x1FF);

        let caps = m.mem.read_u64(hpet::HPET_MMIO_BASE);
        assert_eq!((caps >> 16) & 0xFFFF, 0x8086);
    }

    #[test]
    fn pc_platform_irq_is_not_acknowledged_when_interrupts_disabled() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .write_physical(u64::from(ENTRY_IP), &[0x90, 0x90, 0x90, 0x90]);
        init_real_mode_cpu(&mut m, ENTRY_IP, 0);

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            for irq in 0..16 {
                ints.pic_mut().set_masked(irq, irq != 0);
            }
            ints.raise_irq(aero_platform::interrupts::InterruptInput::IsaIrq(0));
        }

        // Halted + IF=0: the CPU cannot accept maskable interrupts, so the machine should not
        // acknowledge or enqueue the interrupt vector.
        m.cpu.state.halted = true;
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );
        let exit = m.run_slice(5);
        assert_eq!(exit, RunExit::Halted { executed: 0 });
        assert!(m.cpu.pending.external_interrupts().is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );
    }

    #[test]
    fn pc_e1000_intx_is_synced_and_delivered_to_cpu_core() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        let pci_intx = m.pci_intx_router().expect("pc platform enabled");

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Install a trivial real-mode ISR for the expected vector.
        //
        // mov byte ptr [0x2000], 0xAA
        // iret
        const HANDLER_IP: u16 = 0x1100;
        m.mem
            .write_physical(u64::from(HANDLER_IP), &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF]);
        write_ivt_entry(&mut m, expected_vector, HANDLER_IP, 0x0000);

        const ENTRY_IP: u16 = 0x1000;
        m.mem.write_physical(u64::from(ENTRY_IP), &[0x90; 32]);
        m.mem.write_physical(0x2000, &[0x00]);

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Assert E1000 INTx level by enabling + setting a cause bit.
        let e1000 = m.e1000().expect("e1000 enabled");
        {
            let mut dev = e1000.borrow_mut();
            dev.mmio_write_reg(0x00D0, 4, aero_net_e1000::ICR_TXDW); // IMS
            dev.mmio_write_reg(0x00C8, 4, aero_net_e1000::ICR_TXDW); // ICS
            assert!(dev.irq_level());
        }

        // Prior to running a slice, the INTx level has not been synced into the platform
        // interrupt controller yet.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        // With IF=0, `run_slice` must not acknowledge the interrupt, but it should still sync PCI
        // INTx sources so the PIC sees the asserted line.
        init_real_mode_cpu(&mut m, ENTRY_IP, 0);
        m.cpu.state.halted = true;
        let exit = m.run_slice(5);
        assert_eq!(exit, RunExit::Halted { executed: 0 });
        assert!(m.cpu.pending.external_interrupts().is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(expected_vector)
        );
        assert_eq!(m.read_physical_u8(0x2000), 0x00);

        // Once IF is set, the queued/pending interrupt should be delivered into the CPU core and
        // the handler should run.
        m.cpu.state.set_rflags(RFLAGS_IF);
        m.cpu.state.halted = true;
        let _ = m.run_slice(5);
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
        assert!(
            !m.cpu.state.halted,
            "CPU should wake from HLT once PCI INTx is delivered"
        );
    }

    #[test]
    fn pc_e1000_intx_is_synced_even_when_external_interrupt_queue_is_full() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        let pci_intx = m.pci_intx_router().expect("pc platform enabled");

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Assert E1000 INTx level by enabling + setting a cause bit.
        let e1000 = m.e1000().expect("e1000 enabled");
        {
            let mut dev = e1000.borrow_mut();
            dev.mmio_write_reg(0x00D0, 4, aero_net_e1000::ICR_TXDW); // IMS
            dev.mmio_write_reg(0x00C8, 4, aero_net_e1000::ICR_TXDW); // ICS
            assert!(dev.irq_level());
        }

        // Prior to running a slice, the INTx level has not been synced into the platform
        // interrupt controller yet.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        // Fill the external interrupt FIFO to its capacity (1) and disable IF so the CPU cannot
        // drain it. Even though no additional vectors can be enqueued, the machine must still sync
        // PCI INTx sources so the interrupt controller sees the asserted line.
        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .write_physical(u64::from(ENTRY_IP), &[0x90, 0x90, 0x90, 0x90]);
        init_real_mode_cpu(&mut m, ENTRY_IP, 0);
        m.cpu.state.halted = true;
        m.cpu.pending.inject_external_interrupt(0xF0);
        assert_eq!(
            m.cpu
                .pending
                .external_interrupts()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0xF0]
        );

        let exit = m.run_slice(5);
        assert_eq!(exit, RunExit::Halted { executed: 0 });

        // The FIFO is still full, so no new vector should have been enqueued.
        assert_eq!(
            m.cpu
                .pending
                .external_interrupts()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0xF0]
        );

        // But the platform interrupt controller should now see the pending INTx vector.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(expected_vector)
        );
    }

    #[test]
    fn pc_e1000_intx_is_synced_but_not_acknowledged_during_interrupt_shadow() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        let pci_intx = m.pci_intx_router().expect("pc platform enabled");

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Assert E1000 INTx level by enabling + setting a cause bit.
        let e1000 = m.e1000().expect("e1000 enabled");
        {
            let mut dev = e1000.borrow_mut();
            dev.mmio_write_reg(0x00D0, 4, aero_net_e1000::ICR_TXDW); // IMS
            dev.mmio_write_reg(0x00C8, 4, aero_net_e1000::ICR_TXDW); // ICS
            assert!(dev.irq_level());
        }

        // Prior to running a slice, the INTx level has not been synced into the platform
        // interrupt controller yet.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        // Simulate an `STI` interrupt shadow at a slice boundary: IF=1, but interrupts are
        // inhibited for exactly one instruction.
        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .write_physical(u64::from(ENTRY_IP), &[0x90, 0x90, 0x90, 0x90]);
        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);
        m.cpu.pending.inhibit_interrupts_for_one_instruction();

        // Run a single instruction. The machine must not acknowledge/enqueue the interrupt while
        // the shadow is active, but it should still sync PCI INTx sources so the PIC sees the
        // asserted line.
        let exit = m.run_slice(1);
        assert_eq!(exit, RunExit::Completed { executed: 1 });
        assert!(m.cpu.pending.external_interrupts().is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(expected_vector)
        );
    }

    #[test]
    fn pc_e1000_intx_is_synced_but_not_acknowledged_when_pending_event() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        let pci_intx = m.pci_intx_router().expect("pc platform enabled");

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            for irq in 0..16 {
                ints.pic_mut().set_masked(irq, true);
            }
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Assert E1000 INTx level by enabling + setting a cause bit.
        let e1000 = m.e1000().expect("e1000 enabled");
        {
            let mut dev = e1000.borrow_mut();
            dev.mmio_write_reg(0x00D0, 4, aero_net_e1000::ICR_TXDW); // IMS
            dev.mmio_write_reg(0x00C8, 4, aero_net_e1000::ICR_TXDW); // ICS
            assert!(dev.irq_level());
        }

        // Prior to syncing/polling, the INTx level should not yet be visible to the interrupt
        // controller.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        // Set up a CPU state with IF=1 but a pending (non-external) event.
        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .write_physical(u64::from(ENTRY_IP), &[0x90, 0x90, 0x90, 0x90]);
        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);
        m.cpu
            .pending
            .raise_software_interrupt(0x80, u64::from(ENTRY_IP));
        assert!(
            m.cpu.pending.has_pending_event(),
            "test setup: expected a pending event to block external interrupt delivery"
        );

        // Even though a pending event blocks delivery/acknowledge, the machine must still sync PCI
        // INTx sources so the PIC sees the asserted line.
        let queued = m.poll_platform_interrupt(1);
        assert!(
            !queued,
            "poll_platform_interrupt should not enqueue/ack while a pending event exists"
        );
        assert!(m.cpu.pending.external_interrupts().is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(expected_vector)
        );
    }

    #[test]
    fn pc_e1000_intx_asserted_via_bar1_io_wakes_hlt_in_same_slice() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        let pci_intx = m.pci_intx_router().expect("pc platform enabled");
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Resolve the E1000 BAR1 I/O port base assigned by BIOS POST.
        let bar1_base = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .and_then(|cfg| cfg.bar_range(1))
                .expect("missing E1000 BAR1")
                .base
        };
        let ioaddr_port = u16::try_from(bar1_base).expect("E1000 BAR1 should fit in u16 I/O space");
        let iodata_port = ioaddr_port.wrapping_add(4);

        // Install a trivial real-mode ISR for the routed vector.
        //
        // Handler:
        //   mov byte ptr [0x2000], 0xAA
        //   ; clear interrupt by reading ICR via BAR1
        //   mov dx, ioaddr_port
        //   mov eax, 0x00C0 (ICR)
        //   out dx, eax
        //   mov dx, iodata_port
        //   in eax, dx
        //   iret
        const HANDLER_IP: u16 = 0x1100;
        let mut handler = Vec::new();
        handler.extend_from_slice(&[0xC6, 0x06, 0x00, 0x20, 0xAA]); // mov byte ptr [0x2000], 0xAA
        handler.extend_from_slice(&[0xBA, ioaddr_port as u8, (ioaddr_port >> 8) as u8]); // mov dx, ioaddr_port
        handler.extend_from_slice(&[0x66, 0xB8]);
        handler.extend_from_slice(&0x00C0u32.to_le_bytes()); // mov eax, ICR
        handler.extend_from_slice(&[0x66, 0xEF]); // out dx, eax
        handler.extend_from_slice(&[0xBA, iodata_port as u8, (iodata_port >> 8) as u8]); // mov dx, iodata_port
        handler.extend_from_slice(&[0x66, 0xED]); // in eax, dx
        handler.push(0xCF); // iret
        m.mem.write_physical(u64::from(HANDLER_IP), &handler);
        write_ivt_entry(&mut m, expected_vector, HANDLER_IP, 0x0000);

        // Guest program:
        //   ; IMS = ICR_TXDW
        //   mov dx, ioaddr_port
        //   mov eax, 0x00D0 (IMS)
        //   out dx, eax
        //   mov dx, iodata_port
        //   mov eax, ICR_TXDW
        //   out dx, eax
        //
        //   ; ICS = ICR_TXDW (assert INTx)
        //   mov dx, ioaddr_port
        //   mov eax, 0x00C8 (ICS)
        //   out dx, eax
        //   mov dx, iodata_port
        //   mov eax, ICR_TXDW
        //   out dx, eax
        //
        //   hlt
        //   hlt
        const ENTRY_IP: u16 = 0x1000;
        let mut code = Vec::new();
        // IOADDR = IMS
        code.extend_from_slice(&[0xBA, ioaddr_port as u8, (ioaddr_port >> 8) as u8]);
        code.extend_from_slice(&[0x66, 0xB8]);
        code.extend_from_slice(&0x00D0u32.to_le_bytes());
        code.extend_from_slice(&[0x66, 0xEF]);
        // IODATA = ICR_TXDW
        code.extend_from_slice(&[0xBA, iodata_port as u8, (iodata_port >> 8) as u8]);
        code.extend_from_slice(&[0x66, 0xB8]);
        code.extend_from_slice(&aero_net_e1000::ICR_TXDW.to_le_bytes());
        code.extend_from_slice(&[0x66, 0xEF]);
        // IOADDR = ICS
        code.extend_from_slice(&[0xBA, ioaddr_port as u8, (ioaddr_port >> 8) as u8]);
        code.extend_from_slice(&[0x66, 0xB8]);
        code.extend_from_slice(&0x00C8u32.to_le_bytes());
        code.extend_from_slice(&[0x66, 0xEF]);
        // IODATA = ICR_TXDW
        code.extend_from_slice(&[0xBA, iodata_port as u8, (iodata_port >> 8) as u8]);
        code.extend_from_slice(&[0x66, 0xB8]);
        code.extend_from_slice(&aero_net_e1000::ICR_TXDW.to_le_bytes());
        code.extend_from_slice(&[0x66, 0xEF]);
        // HLT (twice so we can observe wakeup + re-halt deterministically).
        code.extend_from_slice(&[0xF4, 0xF4]);

        m.mem.write_physical(u64::from(ENTRY_IP), &code);
        m.mem.write_physical(0x2000, &[0x00]);

        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);

        // One slice should be sufficient: the guest asserts INTx, executes HLT, and the machine
        // should sync + deliver the interrupt within the same `run_slice` call, running the ISR.
        let _ = m.run_slice(100);
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
    }

    #[test]
    fn pc_e1000_tx_dma_completion_wakes_hlt_in_same_slice() {
        // Regression test: if the guest kicks E1000 DMA (e.g. TDT doorbell) and then immediately
        // executes `HLT`, the machine must still poll the NIC and deliver the resulting interrupt
        // within the same `run_slice` call. Otherwise the host would need to call `run_slice` again
        // to observe the interrupt, which is inconsistent with how we treat other DMA devices
        // (e.g. AHCI) in the halted path.

        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        let pci_intx = m.pci_intx_router().expect("pc platform enabled");
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Resolve BAR0 MMIO and BAR1 I/O bases assigned by BIOS POST.
        let (bar0_base, bar1_base) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config(bdf)
                .expect("E1000 device missing from PCI bus");
            let bar0_base = cfg.bar_range(0).expect("missing E1000 BAR0").base;
            let bar1_base = cfg.bar_range(1).expect("missing E1000 BAR1").base;
            (bar0_base, bar1_base)
        };
        let ioaddr_port = u16::try_from(bar1_base).expect("E1000 BAR1 should fit in u16 I/O space");
        let iodata_port = ioaddr_port.wrapping_add(4);

        // Enable PCI decoding + bus mastering (required for E1000 DMA).
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("E1000 device missing from PCI bus");
            cfg.set_command(0x7); // IO + MEM + BME
        }

        // Guest memory layout for TX descriptor ring + packet bytes.
        let tx_ring_base = 0x3000u64;
        let pkt_base = 0x4000u64;
        const MIN_L2_FRAME_LEN: usize = 14;
        let frame = vec![0x11u8; MIN_L2_FRAME_LEN];

        // Write packet bytes + legacy TX descriptor 0 (EOP|RS).
        m.mem.write_physical(pkt_base, &frame);
        let mut desc = [0u8; 16];
        desc[0..8].copy_from_slice(&pkt_base.to_le_bytes());
        desc[8..10].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        desc[11] = (1 << 0) | (1 << 3); // EOP|RS
        m.mem.write_physical(tx_ring_base, &desc);

        // Program E1000 TX ring over MMIO (BAR0) and enable TXDW interrupts.
        m.mem.write_u32(bar0_base + 0x3800, tx_ring_base as u32); // TDBAL
        m.mem.write_u32(bar0_base + 0x3804, 0); // TDBAH
        m.mem.write_u32(bar0_base + 0x3808, 16 * 4); // TDLEN (4 descriptors)
        m.mem.write_u32(bar0_base + 0x3810, 0); // TDH
        m.mem.write_u32(bar0_base + 0x3818, 0); // TDT
        m.mem.write_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN
        m.mem
            .write_u32(bar0_base + 0x00D0, aero_net_e1000::ICR_TXDW); // IMS = TXDW

        // Install a real-mode ISR for the routed vector that records its execution and clears the
        // E1000 interrupt by reading ICR via BAR1.
        const HANDLER_IP: u16 = 0x1100;
        let mut handler = Vec::new();
        handler.extend_from_slice(&[0xC6, 0x06, 0x00, 0x20, 0xAA]); // mov byte ptr [0x2000], 0xAA
        handler.extend_from_slice(&[0xBA, ioaddr_port as u8, (ioaddr_port >> 8) as u8]); // mov dx, ioaddr_port
        handler.extend_from_slice(&[0x66, 0xB8]);
        handler.extend_from_slice(&0x00C0u32.to_le_bytes()); // mov eax, ICR
        handler.extend_from_slice(&[0x66, 0xEF]); // out dx, eax
        handler.extend_from_slice(&[0xBA, iodata_port as u8, (iodata_port >> 8) as u8]); // mov dx, iodata_port
        handler.extend_from_slice(&[0x66, 0xED]); // in eax, dx
        handler.push(0xCF); // iret
        m.mem.write_physical(u64::from(HANDLER_IP), &handler);
        write_ivt_entry(&mut m, expected_vector, HANDLER_IP, 0x0000);

        // Guest program:
        //   ; write TDT=1 via BAR1 I/O then HLT (wait for TXDW interrupt)
        //   mov dx, ioaddr_port
        //   mov eax, 0x3818 (TDT)
        //   out dx, eax
        //   mov dx, iodata_port
        //   mov eax, 1
        //   out dx, eax
        //   hlt
        //   hlt
        const ENTRY_IP: u16 = 0x1000;
        let mut code = Vec::new();
        code.extend_from_slice(&[0xBA, ioaddr_port as u8, (ioaddr_port >> 8) as u8]); // mov dx, ioaddr_port
        code.extend_from_slice(&[0x66, 0xB8]);
        code.extend_from_slice(&0x3818u32.to_le_bytes()); // mov eax, TDT
        code.extend_from_slice(&[0x66, 0xEF]); // out dx, eax
        code.extend_from_slice(&[0xBA, iodata_port as u8, (iodata_port >> 8) as u8]); // mov dx, iodata_port
        code.extend_from_slice(&[0x66, 0xB8]);
        code.extend_from_slice(&1u32.to_le_bytes()); // mov eax, 1
        code.extend_from_slice(&[0x66, 0xEF]); // out dx, eax
        code.extend_from_slice(&[0xF4, 0xF4]); // hlt; hlt
        m.mem.write_physical(u64::from(ENTRY_IP), &code);
        m.mem.write_physical(0x2000, &[0x00]);

        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);

        // One slice should be sufficient: guest kicks TX, halts, machine polls E1000 DMA in the
        // halted path, delivers INTx, runs ISR, and then re-halts.
        let _ = m.run_slice(200);
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
    }

    #[test]
    fn pc_e1000_intx_is_delivered_via_ioapic_in_apic_mode() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        interrupts
            .borrow_mut()
            .set_mode(aero_platform::interrupts::PlatformInterruptMode::Apic);

        let pci_intx = m.pci_intx_router().expect("pc platform enabled");
        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

        // Program IOAPIC entry for this GSI -> vector 0x60 (active-low, level-triggered).
        const VECTOR: u8 = 0x60;
        let low: u32 = u32::from(VECTOR) | (1 << 13) | (1 << 15); // polarity low + level triggered
        let redtbl_low = 0x10u32 + gsi * 2;
        let redtbl_high = redtbl_low + 1;
        m.mem.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
        m.mem.write_u32(IOAPIC_MMIO_BASE + 0x10, low);
        m.mem.write_u32(IOAPIC_MMIO_BASE, redtbl_high);
        m.mem.write_u32(IOAPIC_MMIO_BASE + 0x10, 0);

        // Install a trivial real-mode ISR for the vector.
        //
        // mov byte ptr [0x2000], 0xAA
        // iret
        const HANDLER_IP: u16 = 0x1100;
        m.mem
            .write_physical(u64::from(HANDLER_IP), &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF]);
        write_ivt_entry(&mut m, VECTOR, HANDLER_IP, 0x0000);

        // Program CPU at 0x1000 with enough NOPs to cover the instruction budgets below.
        const ENTRY_IP: u16 = 0x1000;
        m.mem.write_physical(u64::from(ENTRY_IP), &[0x90; 32]);
        m.mem.write_physical(0x2000, &[0x00]);

        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);

        // Assert E1000 INTx level by enabling + setting a cause bit.
        let e1000 = m.e1000().expect("e1000 enabled");
        {
            let mut dev = e1000.borrow_mut();
            dev.mmio_write_reg(0x00D0, 4, aero_net_e1000::ICR_TXDW); // IMS
            dev.mmio_write_reg(0x00C8, 4, aero_net_e1000::ICR_TXDW); // ICS
            assert!(dev.irq_level());
        }

        // Before the machine runs a slice, the INTx level has not been synced into the platform.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        // Simulate the CPU being halted: Tier-0 should wake it once the interrupt vector is
        // delivered (via IOAPIC + LAPIC).
        m.cpu.state.halted = true;
        let _ = m.run_slice(10);
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
        assert!(
            !m.cpu.state.halted,
            "CPU should wake from HLT once PCI INTx is delivered via IOAPIC"
        );
    }

    #[test]
    fn pc_e1000_bar1_io_is_routed_and_gated_by_pci_command_io_enable() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;

        // BAR1 should be assigned during BIOS POST and routed via the machine's PCI I/O window.
        let bar1_base = {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .and_then(|cfg| cfg.bar_range(1))
                .expect("missing E1000 BAR1")
                .base
        };

        let ioaddr_port = u16::try_from(bar1_base).expect("E1000 BAR1 should fit in u16 I/O space");
        let iodata_port = ioaddr_port.wrapping_add(4);

        // Seed a known register value in the device model.
        let e1000 = m.e1000().expect("e1000 enabled");
        e1000.borrow_mut().mmio_write_u32_reg(0x00D0, 0x1234_5678); // IMS

        // Disable PCI I/O decoding: the I/O window should behave as unmapped (reads return 0xFF).
        {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("E1000 device missing from PCI bus");
            cfg.set_command(0x0000);
        }

        m.io_write(ioaddr_port, 4, 0x00D0); // IOADDR = IMS
        assert_eq!(m.io_read(iodata_port, 4), 0xFFFF_FFFF);

        // Re-enable PCI I/O decoding: reads/writes should be dispatched to the E1000 model.
        {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("E1000 device missing from PCI bus");
            cfg.set_command(0x0001);
        }

        m.io_write(ioaddr_port, 4, 0x00D0); // IOADDR = IMS
        assert_eq!(m.io_read(iodata_port, 4), 0x1234_5678);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn machine_e1000_tx_ring_requires_bus_master_and_transmits_to_ring_backend() {
        use aero_ipc::ring::RingBuffer;
        use memory::MemoryBus as _;
        use std::sync::Arc;

        // Host rings (NET_TX is guest->host).
        let tx_ring = Arc::new(RingBuffer::new(16 * 1024));
        let rx_ring = Arc::new(RingBuffer::new(16 * 1024));

        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        m.attach_l2_tunnel_rings(tx_ring.clone(), rx_ring);

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;

        // BAR0 should be assigned by the machine's PCI BIOS POST helper.
        let bar0_base = {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .and_then(|cfg| cfg.bar_range(0))
                .expect("missing E1000 BAR0")
                .base
        };

        // Guest memory layout.
        let tx_ring_base = 0x1000u64;
        let pkt_base = 0x2000u64;

        // Minimum Ethernet frame length: dst MAC (6) + src MAC (6) + ethertype (2).
        const MIN_L2_FRAME_LEN: usize = 14;
        let frame = vec![0x11u8; MIN_L2_FRAME_LEN];

        // Write packet bytes into guest RAM.
        m.mem.write_physical(pkt_base, &frame);

        // Legacy TX descriptor: buffer_addr + length + cmd(EOP|RS).
        let mut desc = [0u8; 16];
        desc[0..8].copy_from_slice(&pkt_base.to_le_bytes());
        desc[8..10].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        desc[10] = 0; // CSO
        desc[11] = (1 << 0) | (1 << 3); // EOP|RS
        desc[12] = 0; // status
        desc[13] = 0; // CSS
        desc[14..16].copy_from_slice(&0u16.to_le_bytes());
        m.mem.write_physical(tx_ring_base, &desc);

        // Program E1000 TX registers over MMIO (BAR0).
        {
            m.mem.write_u32(bar0_base + 0x3800, tx_ring_base as u32); // TDBAL
            m.mem.write_u32(bar0_base + 0x3804, 0); // TDBAH
            m.mem.write_u32(bar0_base + 0x3808, 16 * 4); // TDLEN (4 descriptors)
            m.mem.write_u32(bar0_base + 0x3810, 0); // TDH
            m.mem.write_u32(bar0_base + 0x3818, 0); // TDT
            m.mem.write_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN

            // Doorbell: advance tail to include descriptor 0.
            m.mem.write_u32(bar0_base + 0x3818, 1); // TDT = 1
        }

        // Enable PCI decoding but keep bus mastering disabled.
        {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("E1000 device missing from PCI bus");
            // bit0 = IO space, bit1 = memory space
            cfg.set_command(0x3);
        }

        // Poll once: without BME, the E1000 model must not DMA, so no frame should appear.
        m.poll_network();
        assert!(
            tx_ring.try_pop().is_err(),
            "unexpected TX frame without bus mastering enabled"
        );
        let stats = m
            .network_backend_l2_ring_stats()
            .expect("expected ring backend stats to be available");
        assert_eq!(stats.tx_pushed_frames, 0);
        assert_eq!(stats.tx_dropped_oversize, 0);
        assert_eq!(stats.tx_dropped_full, 0);
        assert_eq!(stats.rx_popped_frames, 0);
        assert_eq!(stats.rx_dropped_oversize, 0);
        assert_eq!(stats.rx_corrupt, 0);

        // Now enable Bus Mastering and poll again; the descriptor should be processed and the
        // resulting frame should appear on NET_TX.
        {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("E1000 device missing from PCI bus");
            // bit0 = IO space, bit1 = memory space, bit2 = bus master
            cfg.set_command(0x7);
        }

        m.poll_network();
        assert_eq!(tx_ring.try_pop(), Ok(frame));
        let stats = m
            .network_backend_l2_ring_stats()
            .expect("expected ring backend stats after successful TX");
        assert_eq!(stats.tx_pushed_frames, 1);
        assert_eq!(stats.tx_dropped_oversize, 0);
        assert_eq!(stats.tx_dropped_full, 0);
        assert_eq!(stats.rx_popped_frames, 0);
        assert_eq!(stats.rx_dropped_oversize, 0);
        assert_eq!(stats.rx_corrupt, 0);
    }

    #[test]
    fn snapshot_restore_preserves_cpu_internal_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.inhibit_interrupts_for_one_instruction();
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        src.cpu.pending.inject_external_interrupt(0x22);

        let expected_inhibit = src.cpu.pending.interrupt_inhibit();
        let expected_external = src.cpu.pending.external_interrupts().clone();

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert_eq!(restored.cpu.pending.interrupt_inhibit(), expected_inhibit);
        assert_eq!(
            restored.cpu.pending.external_interrupts(),
            &expected_external
        );
    }

    #[test]
    fn bios_vbe_lfb_base_uses_configured_vga_lfb_base_only_when_vga_is_enabled() {
        // When VGA is disabled, the BIOS should keep its default RAM-backed LFB base instead of
        // pointing at the VGA MMIO LFB base (which overlaps the canonical PCI MMIO window).
        let headless = Machine::new(MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: false,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            headless.vbe_lfb_base(),
            u64::from(firmware::video::vbe::VbeDevice::LFB_BASE_DEFAULT)
        );
        // When VGA is disabled, `aero-machine` should not request a BIOS LFB base override.
        assert_eq!(headless.bios.config().vbe_lfb_base, None);

        // When VGA is enabled, the BIOS should report the configured MMIO-mapped base.
        // Choose a base outside the BIOS PCI BAR allocator default window
        // (`0xE000_0000..0xF000_0000`) to ensure we don't rely on that sub-window.
        let lfb_base = 0xD000_0000;
        let vga = Machine::new(MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: true,
            vga_lfb_base: Some(lfb_base),
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(vga.vbe_lfb_base(), u64::from(lfb_base));
        assert_eq!(vga.bios.config().vbe_lfb_base, Some(lfb_base));
    }

    #[test]
    fn bios_vbe_lfb_base_uses_aerogpu_bar1_base_when_aerogpu_is_enabled() {
        let m = Machine::new(MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: false,
            enable_aerogpu: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        let bdf = m.aerogpu().expect("AeroGPU should be present");
        let bar1_base = m
            .pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
            .unwrap_or(0);
        assert_ne!(bar1_base, 0, "AeroGPU BAR1 should be assigned by BIOS POST");
        let expected = u64::from(
            u32::try_from(bar1_base + VBE_LFB_OFFSET as u64).expect("LFB base should fit in u32"),
        );
        assert_eq!(m.vbe_lfb_base(), expected);
    }

    fn push_u32_le(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u64_le(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn push_aerogpu_snapshot_bar0_prefix(out: &mut Vec<u8>) {
        // Field order must match `decode_aerogpu_snapshot_v1` / `apply_aerogpu_snapshot_v2`.
        push_u32_le(out, 0); // abi_version
        push_u64_le(out, 0); // features

        push_u64_le(out, 0); // ring_gpa
        push_u32_le(out, 0); // ring_size_bytes
        push_u32_le(out, 0); // ring_control

        push_u64_le(out, 0); // fence_gpa
        push_u64_le(out, 0); // completed_fence

        push_u32_le(out, 0); // irq_status
        push_u32_le(out, 0); // irq_enable

        push_u32_le(out, 0); // scanout0_enable
        push_u32_le(out, 0); // scanout0_width
        push_u32_le(out, 0); // scanout0_height
        push_u32_le(out, 0); // scanout0_format
        push_u32_le(out, 0); // scanout0_pitch_bytes
        push_u64_le(out, 0); // scanout0_fb_gpa

        push_u64_le(out, 0); // scanout0_vblank_seq
        push_u64_le(out, 0); // scanout0_vblank_time_ns
        push_u32_le(out, 0); // scanout0_vblank_period_ns

        push_u32_le(out, 0); // cursor_enable
        push_u32_le(out, 0); // cursor_x
        push_u32_le(out, 0); // cursor_y
        push_u32_le(out, 0); // cursor_hot_x
        push_u32_le(out, 0); // cursor_hot_y
        push_u32_le(out, 0); // cursor_width
        push_u32_le(out, 0); // cursor_height
        push_u32_le(out, 0); // cursor_format
        push_u64_le(out, 0); // cursor_fb_gpa
        push_u32_le(out, 0); // cursor_pitch_bytes

        out.push(0); // wddm_scanout_active
    }

    fn push_aerogpu_snapshot_error_payload(out: &mut Vec<u8>) {
        push_u32_le(out, 0); // error_code
        push_u64_le(out, 0); // error_fence
        push_u32_le(out, 0); // error_count
    }

    fn push_aerogpu_snapshot_dacp(out: &mut Vec<u8>, pel_mask: u8, palette: &[[u8; 3]; 256]) {
        out.extend_from_slice(b"DACP");
        out.push(pel_mask);
        for entry in palette {
            out.extend_from_slice(entry);
        }
    }

    fn new_minimal_aerogpu_device_for_snapshot_tests() -> AeroGpuDevice {
        AeroGpuDevice {
            vram: Vec::new(),
            vram_mmio_reads: Cell::new(0),
            vbe_mode_active: false,
            vbe_bank: 0,
            vbe_dispi_index: 0,
            vbe_dispi_xres: 0,
            vbe_dispi_yres: 0,
            vbe_dispi_bpp: 0,
            vbe_dispi_enable: 0,
            vbe_dispi_virt_width: 0,
            vbe_dispi_virt_height: 0,
            vbe_dispi_x_offset: 0,
            vbe_dispi_y_offset: 0,
            vbe_dispi_guest_owned: false,
            misc_output: 0,
            seq_index: 0,
            seq_regs: [0; 256],
            gc_index: 0,
            gc_regs: [0; 256],
            crtc_index: 0,
            crtc_regs: [0; 256],
            attr_index: 0,
            attr_regs: AeroGpuDevice::default_attr_regs(),
            attr_flip_flop: false,
            pel_mask: 0xFF,
            dac_write_index: 0,
            dac_write_subindex: 0,
            dac_write_latch: [0; 3],
            dac_read_index: 0,
            dac_read_subindex: 0,
            dac_palette: AeroGpuDevice::default_dac_palette(),
        }
    }

    #[test]
    fn decode_aerogpu_snapshot_v1_does_not_parse_dacp_as_pending_fb_gpa() {
        // Regression test for older v1 snapshots: the optional pending FB_GPA pairs were added
        // after the error payload, but pre-existing snapshots may have `DACP` immediately after the
        // error fields. Restoring them must not treat the `DACP` payload as `(pending_lo, flag)`.
        let pel_mask = 0;
        let mut palette = [[0u8; 3]; 256];
        palette[1] = [1, 2, 3];
        palette[255] = [4, 5, 6];

        let mut bytes = Vec::new();
        push_aerogpu_snapshot_bar0_prefix(&mut bytes);
        push_u32_le(&mut bytes, 0); // vram_len
        push_aerogpu_snapshot_error_payload(&mut bytes);
        push_aerogpu_snapshot_dacp(&mut bytes, pel_mask, &palette);

        let snap = decode_aerogpu_snapshot_v1(&bytes).expect("snapshot v1 should decode");
        assert_eq!(snap.bar0.scanout0_fb_gpa_pending_lo, 0);
        assert!(!snap.bar0.scanout0_fb_gpa_lo_pending);
        assert_eq!(snap.bar0.cursor_fb_gpa_pending_lo, 0);
        assert!(!snap.bar0.cursor_fb_gpa_lo_pending);

        let dac = snap.vga_dac.expect("DACP tag should be parsed");
        assert_eq!(dac.pel_mask, pel_mask);
        assert_eq!(dac.palette[0], [0, 0, 0]);
        assert_eq!(dac.palette[1], [1, 2, 3]);
        assert_eq!(dac.palette[255], [4, 5, 6]);
    }

    #[test]
    fn apply_aerogpu_snapshot_v2_does_not_parse_dacp_as_pending_fb_gpa() {
        // Same regression as `decode_aerogpu_snapshot_v1_*`, but through the v2 sparse page
        // snapshot restore path.
        let pel_mask = 0;
        let mut palette = [[0u8; 3]; 256];
        palette[1] = [1, 2, 3];
        palette[255] = [4, 5, 6];

        let mut bytes = Vec::new();
        push_aerogpu_snapshot_bar0_prefix(&mut bytes);
        push_u32_le(&mut bytes, 0); // vram_len
        push_u32_le(&mut bytes, 4096); // page_size
        push_u32_le(&mut bytes, 0); // page_count
        push_aerogpu_snapshot_error_payload(&mut bytes);
        push_aerogpu_snapshot_dacp(&mut bytes, pel_mask, &palette);

        let mut vram = new_minimal_aerogpu_device_for_snapshot_tests();
        let mut bar0 = AeroGpuMmioDevice::default();
        let restored_dac = apply_aerogpu_snapshot_v2(&bytes, &mut vram, &mut bar0)
            .expect("snapshot v2 should apply");
        assert!(restored_dac);

        let regs = bar0.snapshot_v1();
        assert_eq!(regs.scanout0_fb_gpa_pending_lo, 0);
        assert!(!regs.scanout0_fb_gpa_lo_pending);
        assert_eq!(regs.cursor_fb_gpa_pending_lo, 0);
        assert!(!regs.cursor_fb_gpa_lo_pending);

        assert_eq!(vram.pel_mask, pel_mask);
        assert_eq!(vram.dac_palette[0], [0, 0, 0]);
        assert_eq!(vram.dac_palette[1], [1, 2, 3]);
        assert_eq!(vram.dac_palette[255], [4, 5, 6]);
    }

    #[test]
    fn decode_aerogpu_snapshot_v1_parses_pending_pairs_even_when_pending_lo_looks_like_tag() {
        // The pending FB_GPA pairs are untagged u32 values. Ensure we don't mis-detect their
        // presence based purely on the *bytes* of the `pending_lo` field.
        let scanout_pending_lo = u32::from_le_bytes(*b"DACP");
        let cursor_pending_lo = 0x1122_3344;

        let pel_mask = 0xAA;
        let mut palette = [[0u8; 3]; 256];
        palette[0] = [7, 8, 9];

        let mut bytes = Vec::new();
        push_aerogpu_snapshot_bar0_prefix(&mut bytes);
        push_u32_le(&mut bytes, 0); // vram_len
        push_aerogpu_snapshot_error_payload(&mut bytes);

        // Pending pairs (2 × (pending_lo, pending_flag_u32)).
        push_u32_le(&mut bytes, scanout_pending_lo);
        push_u32_le(&mut bytes, 1);
        push_u32_le(&mut bytes, cursor_pending_lo);
        push_u32_le(&mut bytes, 0);

        push_aerogpu_snapshot_dacp(&mut bytes, pel_mask, &palette);

        let snap = decode_aerogpu_snapshot_v1(&bytes).expect("snapshot v1 should decode");
        assert_eq!(snap.bar0.scanout0_fb_gpa_pending_lo, scanout_pending_lo);
        assert!(snap.bar0.scanout0_fb_gpa_lo_pending);
        assert_eq!(snap.bar0.cursor_fb_gpa_pending_lo, cursor_pending_lo);
        assert!(!snap.bar0.cursor_fb_gpa_lo_pending);

        let dac = snap.vga_dac.expect("DACP tag should be parsed");
        assert_eq!(dac.pel_mask, pel_mask);
        assert_eq!(dac.palette[0], [7, 8, 9]);
    }

    #[test]
    fn apply_aerogpu_snapshot_v2_parses_pending_pairs_even_when_pending_lo_looks_like_tag() {
        let scanout_pending_lo = u32::from_le_bytes(*b"DACP");
        let cursor_pending_lo = 0x1122_3344;

        let pel_mask = 0xAA;
        let mut palette = [[0u8; 3]; 256];
        palette[0] = [7, 8, 9];

        let mut bytes = Vec::new();
        push_aerogpu_snapshot_bar0_prefix(&mut bytes);
        push_u32_le(&mut bytes, 0); // vram_len
        push_u32_le(&mut bytes, 4096); // page_size
        push_u32_le(&mut bytes, 0); // page_count
        push_aerogpu_snapshot_error_payload(&mut bytes);

        push_u32_le(&mut bytes, scanout_pending_lo);
        push_u32_le(&mut bytes, 1);
        push_u32_le(&mut bytes, cursor_pending_lo);
        push_u32_le(&mut bytes, 0);

        push_aerogpu_snapshot_dacp(&mut bytes, pel_mask, &palette);

        let mut vram = new_minimal_aerogpu_device_for_snapshot_tests();
        let mut bar0 = AeroGpuMmioDevice::default();
        let restored_dac = apply_aerogpu_snapshot_v2(&bytes, &mut vram, &mut bar0)
            .expect("snapshot v2 should apply");
        assert!(restored_dac);

        let regs = bar0.snapshot_v1();
        assert_eq!(regs.scanout0_fb_gpa_pending_lo, scanout_pending_lo);
        assert!(regs.scanout0_fb_gpa_lo_pending);
        assert_eq!(regs.cursor_fb_gpa_pending_lo, cursor_pending_lo);
        assert!(!regs.cursor_fb_gpa_lo_pending);

        assert_eq!(vram.pel_mask, pel_mask);
        assert_eq!(vram.dac_palette[0], [7, 8, 9]);
    }

    #[test]
    fn decode_aerogpu_snapshot_v1_parses_pending_pairs_without_tags_even_when_pending_lo_looks_like_tag(
    ) {
        // If a snapshot contains pending pairs but no tagged trailing sections, `pending_lo` may
        // still coincidentally match a tag string. Ensure we treat it as a pending field rather
        // than skipping pending parsing.
        let scanout_pending_lo = u32::from_le_bytes(*b"DACP");
        let cursor_pending_lo = 0x1122_3344;

        let mut bytes = Vec::new();
        push_aerogpu_snapshot_bar0_prefix(&mut bytes);
        push_u32_le(&mut bytes, 0); // vram_len
        push_aerogpu_snapshot_error_payload(&mut bytes);

        // Pending pairs (2 × (pending_lo, pending_flag_u32)).
        push_u32_le(&mut bytes, scanout_pending_lo);
        push_u32_le(&mut bytes, 1);
        push_u32_le(&mut bytes, cursor_pending_lo);
        push_u32_le(&mut bytes, 0);

        let snap = decode_aerogpu_snapshot_v1(&bytes).expect("snapshot v1 should decode");
        assert_eq!(snap.bar0.scanout0_fb_gpa_pending_lo, scanout_pending_lo);
        assert!(snap.bar0.scanout0_fb_gpa_lo_pending);
        assert_eq!(snap.bar0.cursor_fb_gpa_pending_lo, cursor_pending_lo);
        assert!(!snap.bar0.cursor_fb_gpa_lo_pending);
        assert!(snap.vga_dac.is_none());
    }

    #[test]
    fn apply_aerogpu_snapshot_v2_parses_pending_pairs_without_tags_even_when_pending_lo_looks_like_tag(
    ) {
        let scanout_pending_lo = u32::from_le_bytes(*b"DACP");
        let cursor_pending_lo = 0x1122_3344;

        let mut bytes = Vec::new();
        push_aerogpu_snapshot_bar0_prefix(&mut bytes);
        push_u32_le(&mut bytes, 0); // vram_len
        push_u32_le(&mut bytes, 4096); // page_size
        push_u32_le(&mut bytes, 0); // page_count
        push_aerogpu_snapshot_error_payload(&mut bytes);

        push_u32_le(&mut bytes, scanout_pending_lo);
        push_u32_le(&mut bytes, 1);
        push_u32_le(&mut bytes, cursor_pending_lo);
        push_u32_le(&mut bytes, 0);

        let mut vram = new_minimal_aerogpu_device_for_snapshot_tests();
        let mut bar0 = AeroGpuMmioDevice::default();
        let restored_dac = apply_aerogpu_snapshot_v2(&bytes, &mut vram, &mut bar0)
            .expect("snapshot v2 should apply");
        assert!(!restored_dac, "no DACP tag was provided");

        let regs = bar0.snapshot_v1();
        assert_eq!(regs.scanout0_fb_gpa_pending_lo, scanout_pending_lo);
        assert!(regs.scanout0_fb_gpa_lo_pending);
        assert_eq!(regs.cursor_fb_gpa_pending_lo, cursor_pending_lo);
        assert!(!regs.cursor_fb_gpa_lo_pending);
    }

    #[test]
    fn aerogpu_snapshot_v2_restores_bochs_vbe_dispi_state() {
        // Regression coverage for the optional `BVBE` trailing section.
        let mut src_vram = new_minimal_aerogpu_device_for_snapshot_tests();
        src_vram.vbe_dispi_guest_owned = true;
        src_vram.vbe_dispi_index = 0x1234;
        src_vram.vbe_dispi_xres = 800;
        src_vram.vbe_dispi_yres = 600;
        src_vram.vbe_dispi_bpp = 32;
        src_vram.vbe_dispi_enable = 0x0001;
        src_vram.vbe_bank = 3;
        src_vram.vbe_dispi_virt_width = 1024;
        src_vram.vbe_dispi_virt_height = 768;
        src_vram.vbe_dispi_x_offset = 10;
        src_vram.vbe_dispi_y_offset = 20;

        let src_bar0 = AeroGpuMmioDevice::default();
        let bytes = encode_aerogpu_snapshot_v2(&src_vram, &src_bar0);

        let mut dst_vram = new_minimal_aerogpu_device_for_snapshot_tests();
        let mut dst_bar0 = AeroGpuMmioDevice::default();
        let _ = apply_aerogpu_snapshot_v2(&bytes, &mut dst_vram, &mut dst_bar0)
            .expect("snapshot v2 should apply");

        assert_eq!(dst_vram.vbe_dispi_guest_owned, true);
        assert_eq!(dst_vram.vbe_dispi_index, 0x1234);
        assert_eq!(dst_vram.vbe_dispi_xres, 800);
        assert_eq!(dst_vram.vbe_dispi_yres, 600);
        assert_eq!(dst_vram.vbe_dispi_bpp, 32);
        assert_eq!(dst_vram.vbe_dispi_enable, 0x0001);
        assert_eq!(dst_vram.vbe_bank, 3);
        assert_eq!(dst_vram.vbe_dispi_virt_width, 1024);
        assert_eq!(dst_vram.vbe_dispi_virt_height, 768);
        assert_eq!(dst_vram.vbe_dispi_x_offset, 10);
        assert_eq!(dst_vram.vbe_dispi_y_offset, 20);
    }

    #[test]
    fn restore_snapshot_ignores_vga_state_when_vga_is_disabled() {
        // Restoring a VGA-enabled snapshot into a headless machine should not panic due to MMIO
        // overlap (the headless PC platform maps its PCI MMIO window at 0xE0000000, which overlaps
        // the VGA MMIO LFB base).
        let mut src = Machine::new(MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();
        let snap = src.take_snapshot_full().unwrap();

        let mut dst = Machine::new(MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: false,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        dst.restore_snapshot_bytes(&snap).unwrap();

        assert!(
            dst.vga.is_none(),
            "headless restore should not create a VGA device"
        );
        assert_eq!(
            dst.vbe_lfb_base(),
            u64::from(firmware::video::vbe::VbeDevice::LFB_BASE_DEFAULT)
        );
    }

    #[test]
    fn machine_cpu_bus_declines_bulk_ops_when_a20_is_disabled() {
        use aero_cpu_core::mem::CpuBus as _;

        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        // BIOS POST enables A20; force it off so we exercise the A20-aliasing path.
        m.chipset.a20().set_enabled(false);

        let interrupts = m.interrupts.clone();
        let phys = PerCpuSystemMemoryBus::new(
            0,
            interrupts,
            ApCpus::All(m.ap_cpus.as_mut_slice()),
            &mut m.mem,
        );
        let inner = aero_cpu_core::PagingBus::new_with_io(phys, StrictIoPortBus { io: &mut m.io });
        let mut bus = MachineCpuBus {
            a20: m.chipset.a20(),
            reset: m.reset_latch.clone(),
            inner,
        };

        assert!(!bus.supports_bulk_copy());
        assert!(!bus.supports_bulk_set());
        assert_eq!(bus.bulk_copy(0x2000, 0x1000, 64).unwrap(), false);
        assert_eq!(bus.bulk_set(0x3000, &[0xAA], 64).unwrap(), false);
    }

    #[test]
    fn pc_ram_is_remapped_above_4gib_and_pci_hole_is_open_bus() {
        // Exercise the PC-style RAM layout without allocating multi-GB dense backing memory.
        //
        // - low RAM:  [0, PCIE_ECAM_BASE)
        // - hole:     [PCIE_ECAM_BASE, 4GiB) (open bus)
        // - high RAM: [4GiB, 4GiB + (ram_size - PCIE_ECAM_BASE))
        const FOUR_GIB: u64 = 0x1_0000_0000;
        let low_end = firmware::bios::PCIE_ECAM_BASE;
        let high_len = 0x2000u64;
        let ram_size_bytes = low_end + high_len;
        let phys_size = FOUR_GIB + high_len;

        let backing = memory::SparseMemory::new(ram_size_bytes).unwrap();
        let (backing, _dirty) = DirtyGuestMemory::new(Box::new(backing), SNAPSHOT_DIRTY_PAGE_SIZE);

        let mapped = memory::MappedGuestMemory::new(
            Box::new(backing),
            phys_size,
            vec![
                memory::GuestMemoryMapping {
                    phys_start: 0,
                    phys_end: low_end,
                    inner_offset: 0,
                },
                memory::GuestMemoryMapping {
                    phys_start: FOUR_GIB,
                    phys_end: phys_size,
                    inner_offset: low_end,
                },
            ],
        )
        .unwrap();

        let mut bus = memory::PhysicalMemoryBus::new(Box::new(mapped));

        // Writing to high RAM at 4GiB should succeed and be observable via the same address.
        bus.write_physical(FOUR_GIB, &[0xAA]);
        let mut byte = [0u8; 1];
        bus.read_physical(FOUR_GIB, &mut byte);
        assert_eq!(byte[0], 0xAA);

        // The reserved PCI hole should behave like open bus.
        let hole_addr = low_end + 0x1000;
        bus.read_physical(hole_addr, &mut byte);
        assert_eq!(byte[0], 0xFF);

        bus.write_physical(hole_addr, &[0x00]);
        bus.read_physical(hole_addr, &mut byte);
        assert_eq!(byte[0], 0xFF);
    }
}
